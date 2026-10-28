use super::*;
use axum::body::to_bytes;
use tower::ServiceExt;
mod smtp;
mod conversations;

/// Disconnecting a live reader cannot abandon settlement; replay returns the cached answer once.
#[tokio::test]
async fn streaming_disconnect_settles_once() -> anyhow::Result<()> {
    let state=application()?;
    let user=state.db.user("stream@example.test",None,3)?;
    let token=db::secret();
    state.db.0.lock().unwrap().execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",rusqlite::params![db::hash(&token),user,db::now()+60])?;
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url=format!("http://{}/v1/chat",listener.local_addr()?);
    let app=router(state.clone());
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let input=json!({"base":0,"request_id":uuid::Uuid::new_v4(),"conversation_id":uuid::Uuid::new_v4(),"platform":"linux",
        "character":"guide","messages":[{"role":"user","text":"Hello 世界"}],"commands_enabled":false,"cwd":"/tmp"});
    let http=reqwest::Client::new();
    let mut response=http.post(&url).bearer_auth(&token).header("accept","text/event-stream").json(&input).send().await?;
    assert!(response.status().is_success());
    assert_eq!(response.headers()["content-type"],"text/event-stream");
    let first=tokio::time::timeout(Duration::from_secs(1),response.chunk()).await??.unwrap();
    assert!(std::str::from_utf8(&first)?.contains("progress"));
    assert_eq!(state.db.balance(user)?,2);
    drop(response);
    let pending=http.post(&url).bearer_auth(&token).json(&input).send().await?;
    assert_eq!(pending.status(),StatusCode::CONFLICT);
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let response=http.post(&url).bearer_auth(&token).header("accept","text/event-stream").json(&input).send().await?;
            if response.status().is_success() {
                let body=response.text().await?;
                assert!(body.contains("\"type\":\"complete\"") && body.contains("Hello 世界"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_,anyhow::Error>(())
    }).await??;
    assert_eq!(state.db.balance(user)?,2);
    assert_eq!(state.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM ledger WHERE delta=-1",[],|r|r.get::<_,i64>(0))?,1);
    server.abort();
    Ok(())
}

/// Build a fully isolated backend with deterministic development integrations.
fn application() -> anyhow::Result<App> {
    let config = Config::parse_from(["assistant-server","--dev"]);
    Ok(App { turns:Arc::new(Default::default()),pool:Arc::new(provider::Pool::new(&config)?),config:Arc::new(config),db:Arc::new(Db::open(std::path::Path::new(":memory:"))?),http:reqwest::Client::new(),mail:None })
}

/// Exercise the actual HTTP routing, authentication, body parsing, and ledger together.
async fn request(app: &Router, path: &str, token: Option<&str>, value: Value) -> anyhow::Result<(StatusCode,Value)> {
    let mut request = Request::builder().method("POST").uri(path).header(header::CONTENT_TYPE,"application/json");
    if let Some(token) = token { request=request.header(header::AUTHORIZATION,format!("Bearer {token}")); }
    let response=app.clone().oneshot(request.body(Body::from(value.to_string()))?).await?;
    let status=response.status();
    Ok((status,serde_json::from_slice(&to_bytes(response.into_body(),256*1024).await?)?))
}

/// Issuing a session consumes the device, and retrying a chat does not double-charge.
#[tokio::test]
async fn login_and_chat_are_bound_to_user_and_request() -> anyhow::Result<()> {
    let state=application()?;
    let app=router(state.clone());
    let (_,device)=request(&app,"/v1/auth/device",None,json!({})).await?;
    let poll=json!({"device_id":device["device_id"],"poll_secret":device["poll_secret"]});
    assert!(request(&app,"/v1/auth/poll",None,poll.clone()).await?.1["token"].is_null());
    let user=state.db.user("demo@example.com",None,2)?;
    state.db.0.lock().unwrap().execute("UPDATE devices SET user_id=?1 WHERE id=?2",rusqlite::params![user,device["device_id"].as_str().unwrap()])?;
    let (_,session)=request(&app,"/v1/auth/poll",None,poll.clone()).await?;
    let token=session["token"].as_str().unwrap();
    assert_eq!(request(&app,"/v1/auth/poll",None,poll).await?.0,StatusCode::BAD_REQUEST);
    let chat=json!({"base":0,"request_id":uuid::Uuid::new_v4(),"conversation_id":uuid::Uuid::new_v4(),"platform":"linux","character":"guide","messages":[{"role":"user","text":"Hello 世界"}],"commands_enabled":false,"cwd":"/tmp"});
    assert_eq!(request(&app,"/v1/chat",None,chat.clone()).await?.0,StatusCode::UNAUTHORIZED);
    let (status,reply)=request(&app,"/v1/chat",Some(token),chat.clone()).await?;
    assert_eq!(status,StatusCode::OK);
    assert_eq!(reply["credits"],1);
    assert!(reply["reply"]["command"].is_null());
    assert_eq!(request(&app,"/v1/chat",Some(token),chat.clone()).await?.1["credits"],1);
    assert_eq!(state.db.balance(user)?,1);
    let mut stale=chat.clone();
    stale["request_id"]=json!(uuid::Uuid::new_v4());
    assert_eq!(request(&app,"/v1/chat",Some(token),stale).await?.0,StatusCode::BAD_REQUEST);
    assert_eq!(state.db.balance(user)?,1);
    let mut second=chat.clone();
    second["request_id"]=json!(uuid::Uuid::new_v4());
    second["base"]=reply["revision"].clone();
    second["messages"]=json!([{"role":"user","text":"Continue"}]);
    assert_eq!(request(&app,"/v1/chat",Some(token),second).await?.1["credits"],0);
    let mut third=chat;
    third["request_id"]=json!(uuid::Uuid::new_v4());
    third["conversation_id"]=json!(uuid::Uuid::new_v4());
    assert_eq!(request(&app,"/v1/chat",Some(token),third).await?.0,StatusCode::PAYMENT_REQUIRED);
    request(&app,"/v1/logout",Some(token),json!({})).await?;
    assert!(state.db.authenticate(token).is_err());
    Ok(())
}

/// Paid webhook replay is idempotent even when Stripe delivers multiple event IDs.
#[tokio::test]
async fn signed_payment_is_credited_once() -> anyhow::Result<()> {
    use hmac::{Hmac,Mac};
    use sha2::Sha256;
    let mut state=application()?;
    let config=Arc::make_mut(&mut state.config);
    config.dev=false;
    config.stripe_webhook_secret=Some("test-secret".into());
    let user=state.db.user("buyer@example.com",None,0)?;
    state.db.0.lock().unwrap().execute("INSERT INTO purchases(id,user_id,credits,stripe_session) VALUES('purchase',?1,100,'cs_test')",[user])?;
    let app=router(state.clone());
    for _ in 0..2 {
        let body=json!({"id":uuid::Uuid::new_v4(),"type":"checkout.session.completed","data":{"object":{"id":"cs_test","mode":"payment","payment_status":"paid","metadata":{"purchase_id":"purchase"}}}}).to_string();
        let time=db::now();
        let mut mac=Hmac::<Sha256>::new_from_slice(b"test-secret")?;
        mac.update(format!("{time}.{body}").as_bytes());
        let signature=format!("t={time},v1={}",hex::encode(mac.finalize().into_bytes()));
        let response=app.clone().oneshot(Request::builder().method("POST").uri("/webhooks/stripe").header("stripe-signature",signature).body(Body::from(body))?).await?;
        assert_eq!(response.status(),StatusCode::OK);
    }
    assert_eq!(state.db.balance(user)?,100);
    Ok(())
}

/// Code attempts are consumed even when verification fails; guessing cannot continue forever.
#[tokio::test]
async fn email_codes_expire_after_five_attempts() -> anyhow::Result<()> {
    let state=application()?;
    let app=router(state.clone());
    let (_,device)=request(&app,"/v1/auth/device",None,json!({})).await?;
    let id=device["device_id"].as_str().unwrap();
    state.db.0.lock().unwrap().execute("UPDATE devices SET email='code@example.com',code_hash=?1 WHERE id=?2",rusqlite::params![db::hash(&format!("{id}:correct")),id])?;
    for code in ["wrong","wrong","wrong","wrong","wrong","correct"] {
        let response=app.clone().oneshot(Request::builder().method("POST").uri("/auth/verify").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
            .body(Body::from(format!("device={id}&code={code}")))?).await?;
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    }
    assert!(request(&app,"/v1/auth/poll",None,json!({"device_id":id,"poll_secret":device["poll_secret"]})).await?.1["token"].is_null());
    Ok(())
}
