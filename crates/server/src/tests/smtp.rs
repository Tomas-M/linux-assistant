use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::TcpListener};
use tokio_rustls::{TlsAcceptor, rustls::{self, pki_types::PrivatePkcs8KeyDer}};

/// Configure a real-mode backend without depending on operator credentials or external services.
fn smtp_config() -> Config {
    Config { public_url:"https://assistant.example.test".into(),email_enabled:true,smtp_host:"127.0.0.1".into(),smtp_username:Some("test-user".into()),
        smtp_password:Some("test-password".into()),email_from:Some("Průvodce 世界 <sender@example.test>".into()),..Config::default() }
}

/// Reject incomplete SMTP configuration before the service starts.
#[test]
fn smtp_configuration_requires_credentials_and_sender() -> anyhow::Result<()> {
    let config=smtp_config();
    config.validate()?;
    assert_eq!(config.smtp_port,465);
    let mut missing=config.clone();
    missing.smtp_password=None;
    assert!(missing.validate().is_err());
    missing.smtp_password=Some(" ".into());
    assert!(missing.validate().is_err());
    missing=config.clone();
    missing.smtp_username=Some(String::new());
    assert!(missing.validate().is_err());
    missing=config.clone();
    missing.email_from=None;
    assert!(missing.validate().is_err());
    missing=config.clone();
    missing.smtp_host="ssl://smtp.gmail.com".into();
    assert!(missing.validate().is_err());
    missing=config;
    missing.smtp_port=0;
    assert!(missing.validate().is_err());
    Config { dev:true,public_url:"http://127.0.0.1:8787".into(),email_enabled:true,..Config::default() }.validate()?;
    Ok(())
}

/// Supply a local TLS endpoint with a certificate trusted only by the test client.
async fn smtp_listener() -> anyhow::Result<(TcpListener,TlsAcceptor,Certificate)> {
    let certificate=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
    let key=PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let server=rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?.with_no_client_auth()
        .with_single_cert(vec![certificate.cert.der().clone()],key.into())?;
    Ok((TcpListener::bind("127.0.0.1:0").await?,TlsAcceptor::from(Arc::new(server)),
        Certificate::from_der(certificate.cert.der().to_vec())?))
}

/// Check authentication and SMTP envelopes over TLS, then capture or reject the message.
async fn smtp_session(listener: TcpListener, tls: TlsAcceptor, reject_auth: bool) -> anyhow::Result<String> {
    let (socket,_)=listener.accept().await?;
    let mut stream=BufReader::new(tls.accept(socket).await?);
    stream.get_mut().write_all(b"220 localhost ESMTP\r\n").await?;
    let mut message=String::new();
    let mut authenticated=false;
    loop {
        let mut line=String::new();
        anyhow::ensure!(stream.read_line(&mut line).await?>0,"SMTP client disconnected");
        let response=if line.starts_with("EHLO ") {
            "250-localhost\r\n250-AUTH PLAIN\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n"
        } else if let Some(encoded)=line.strip_prefix("AUTH PLAIN ") {
            assert_eq!(STANDARD.decode(encoded.trim())?,b"\0test-user\0test-password");
            if reject_auth {
                stream.get_mut().write_all(b"535 5.7.8 Authentication failed\r\n").await?;
                return Ok(message);
            }
            authenticated=true;
            "235 2.7.0 Authenticated\r\n"
        } else if line.starts_with("MAIL FROM:") {
            assert!(authenticated);
            assert!(line.contains("<sender@example.test>"));
            "250 Sender accepted\r\n"
        } else if line.starts_with("RCPT TO:") {
            assert!(line.contains("<žluťoučký@example.test>"));
            "250 Recipient accepted\r\n"
        } else if line=="DATA\r\n" {
            stream.get_mut().write_all(b"354 Send message\r\n").await?;
            loop {
                line.clear();
                anyhow::ensure!(stream.read_line(&mut line).await?>0,"Incomplete SMTP message");
                if line==".\r\n" { break; }
                message.push_str(&line);
            }
            "250 Message accepted\r\n"
        } else if line=="QUIT\r\n" {
            stream.get_mut().write_all(b"221 Bye\r\n").await?;
            return Ok(message);
        } else { anyhow::bail!("Unexpected SMTP command"); };
        stream.get_mut().write_all(response.as_bytes()).await?;
    }
}

/// Exercise the real login route, TLS, credentials, Unicode recipient, and failed-delivery response.
#[tokio::test]
async fn smtp_delivery_and_authentication_failure() -> anyhow::Result<()> {
    for reject_auth in [false,true] {
        let (listener,tls,certificate)=smtp_listener().await?;
        let state=application()?;
        let mut config=smtp_config();
        config.smtp_port=listener.local_addr()?.port();
        config.validate()?;
        let trust=TlsParameters::builder(config.smtp_host.clone()).add_root_certificate(certificate).build()?;
        *state.settings.write().unwrap()=Arc::new(crate::settings::Runtime { mail:Some(crate::mail::transport(&config)?.tls(Tls::Wrapper(trust)).build()),config });
        let server=tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10),smtp_session(listener,tls,reject_auth)).await?
        });
        let app=router(state.clone());
        let (_,device)=request(&app,"/v1/auth/device",None,json!({})).await?;
        let id=device["device_id"].as_str().unwrap();
        let (status,body)=request(&app,"/v1/auth/email",None,json!({"device_id":id,"poll_secret":device["poll_secret"],"email":"žluťoučký@example.test"})).await?;
        let message=server.await??;
        if reject_auth {
            assert_eq!(status,StatusCode::INTERNAL_SERVER_ERROR);
            assert!(message.is_empty());
            assert!(body["error"].is_string());
        } else {
            assert_eq!(status,StatusCode::OK);
            assert!(body["development_code"].is_null());
            assert!(message.contains("Subject: Your Linux Assistant sign-in code"));
            let code=message.split("Enter this code in your assistant:").nth(1).unwrap().trim_start().lines().next().unwrap().trim();
            assert_eq!(code.len(),6);
            assert!(code.bytes().all(|c|c.is_ascii_digit()));
            assert!(!body.to_string().contains(code));
            assert!(message.contains("multipart/alternative") && message.contains("text/html") && message.contains("text/plain"));
            let html=message.replace("=\r\n","").replace("=3D","=");
            assert!(html.contains("<h1 ") && html.contains(&format!("<span>{}</span><span style=\"margin-left:0.35em\">{}</span>",&code[..3],&code[3..])));
            let stored: String=state.db.0.lock().unwrap().query_row("SELECT code_hash FROM devices WHERE id=?1",[id],|row|row.get(0))?;
            assert_eq!(stored,db::hash(&format!("{id}:{code}")));
            assert_eq!(request(&app,"/v1/auth/verify",None,json!({"device_id":id,"poll_secret":device["poll_secret"],"code":code})).await?.0,StatusCode::OK);
        }
        assert!(!body.to_string().contains("test-password"));
    }
    Ok(())
}

/// The production transport must reject untrusted certificates before sending credentials.
#[tokio::test]
async fn smtp_rejects_untrusted_certificate() -> anyhow::Result<()> {
    let (listener,tls,_)=smtp_listener().await?;
    let state=application()?;
    let mut config=smtp_config();
    config.smtp_port=listener.local_addr()?.port();
    *state.settings.write().unwrap()=Arc::new(crate::settings::Runtime { mail:Some(crate::mail::transport(&config)?.build()),config });
    let server=tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10),smtp_session(listener,tls,false)).await?
    });
    assert!(crate::mail::send_code(&state,"žluťoučký@example.test".parse()?,"012345").await.is_err());
    assert!(server.await?.is_err());
    Ok(())
}
