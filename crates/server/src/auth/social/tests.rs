use super::*;
use axum::{body::{Body,to_bytes},http::{Request,StatusCode}};
use tower::ServiceExt;
use std::sync::Arc;

/// Discovery contains only complete configurations, without client IDs or secrets.
#[tokio::test]
async fn social_discovery_and_configuration()->anyhow::Result<()> {
    let app=crate::tests::application()?;
    assert_eq!(providers(State(app.clone())).await.0,json!({"email_enabled":true,"providers":[]}));
    {
    let mut settings=app.settings.write().unwrap();
    let config=&mut Arc::make_mut(&mut *settings).config;
    config.google_enabled=true; config.microsoft_enabled=true; config.github_enabled=true;
    config.google_client_id=Some("google-id".into()); config.google_client_secret=Some("google-secret".into());
    config.microsoft_client_id=Some("microsoft-id".into()); config.microsoft_client_secret=Some("microsoft-secret".into());
    config.github_client_id=Some("github-id".into()); config.github_client_secret=Some("github-secret".into());
    config.validate()?;
    }
    let response=crate::router(app.clone()).oneshot(Request::builder().uri("/v1/auth/providers").body(Body::empty())?).await?;
    assert_eq!(response.status(),StatusCode::OK);
    let value:Value=serde_json::from_slice(&to_bytes(response.into_body(),4096).await?)?;
    assert_eq!(value,json!({"email_enabled":true,"providers":["google","microsoft","github"]}));
    Arc::make_mut(&mut *app.settings.write().unwrap()).config.github_client_secret=None;
    assert!(app.config().validate().is_err());
    Ok(())
}

/// Every provider uses the registered callback, browser state and PKCE; declined sign-ins reach the client.
#[tokio::test]
async fn social_state_pkce_cancellation_and_provider_binding()->anyhow::Result<()> {
    let app=crate::tests::application()?;
    {
    let mut settings=app.settings.write().unwrap();
    let config=&mut Arc::make_mut(&mut *settings).config;
    config.google_enabled=true; config.microsoft_enabled=true; config.github_enabled=true;
    config.google_client_id=Some("id".into()); config.google_client_secret=Some("secret".into());
    config.microsoft_client_id=Some("id".into()); config.microsoft_client_secret=Some("secret".into());
    config.github_client_id=Some("id".into()); config.github_client_secret=Some("secret".into());
    }
    for provider in [AuthProvider::Google,AuthProvider::Microsoft,AuthProvider::Github] {
        let device=crate::auth::start(State(app.clone())).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
        let router=crate::router(app.clone());
        let response=router.clone().oneshot(Request::builder().uri(format!("/auth/{}/{}",provider.key(),device.device_id)).body(Body::empty())?).await?;
        assert_eq!(response.status(),StatusCode::SEE_OTHER);
        let url=reqwest::Url::parse(response.headers()[header::LOCATION].to_str()?)?;
        let query=url.query_pairs().collect::<std::collections::HashMap<_,_>>();
        assert_eq!(query["redirect_uri"],format!("{}/auth/{}/callback",app.config().public_url,provider.key()));
        assert_eq!(query["code_challenge_method"],"S256");
        assert!(!query.contains_key("client_secret"));
        let state=&query["state"];
        let verifier:String=app.db.0.lock().unwrap().query_row("SELECT verifier FROM oauth WHERE device_id=?1",[&device.device_id],|r|r.get(0))?;
        assert_eq!(query["code_challenge"],URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())));
        let cookie=response.headers()[header::SET_COOKIE].to_str()?.split(';').next().unwrap();
        let denied=format!("/auth/{}/callback?state={state}&error=access_denied",provider.key());
        let missing_cookie=router.clone().oneshot(Request::builder().uri(&denied).body(Body::empty())?).await?;
        assert_eq!(missing_cookie.status(),StatusCode::BAD_REQUEST);
        let wrong_provider=if provider==AuthProvider::Google { "microsoft" } else { "google" };
        let mismatch=router.clone().oneshot(Request::builder().uri(format!("/auth/{wrong_provider}/callback?state={state}&error=access_denied")).header(header::COOKIE,cookie).body(Body::empty())?).await?;
        assert_eq!(mismatch.status(),StatusCode::BAD_REQUEST);
        let declined=router.clone().oneshot(Request::builder().uri(&denied).header(header::COOKIE,cookie).body(Body::empty())?).await?;
        assert_eq!(declined.status(),StatusCode::OK);
        let replay=router.oneshot(Request::builder().uri(&denied).header(header::COOKIE,cookie).body(Body::empty())?).await?;
        assert_eq!(replay.status(),StatusCode::BAD_REQUEST);
        let error=crate::auth::poll(State(app.clone()),Json(PollLogin { device_id:device.device_id.clone(),poll_secret:device.poll_secret.clone() })).await.err().unwrap();
        assert!(error.1.contains("declined"));
        let _=cancel(State(app.clone()),Json(PollLogin { device_id:device.device_id.clone(),poll_secret:"wrong".into() })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?;
        let count:i64=app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM devices WHERE id=?1",[&device.device_id],|r|r.get(0))?;
        assert_eq!(count,1);
        let _=cancel(State(app.clone()),Json(PollLogin { device_id:device.device_id.clone(),poll_secret:device.poll_secret })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?;
        let count:i64=app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM social_signins WHERE device_id=?1",[device.device_id],|r|r.get(0))?;
        assert_eq!(count,0);
    }
    Ok(())
}

/// Trust GitHub's verified emails and authoritative Google addresses, never Microsoft's email claim alone.
#[test]
fn social_email_authority_and_stable_subjects()->anyhow::Result<()> {
    assert!(identity(AuthProvider::Google,&json!({"sub":"g","email":"USER@GMAIL.COM","email_verified":true}),&Value::Null)?.trusted_email);
    assert!(!identity(AuthProvider::Google,&json!({"sub":"g","email":"owner@example.test","email_verified":true}),&Value::Null)?.trusted_email);
    assert!(identity(AuthProvider::Google,&json!({"sub":"g","email":"owner@example.test","email_verified":true,"hd":"example.test"}),&Value::Null)?.trusted_email);
    assert!(!identity(AuthProvider::Microsoft,&json!({"sub":"m","email":"owner@example.test","email_verified":true}),&Value::Null)?.trusted_email);
    let github=identity(AuthProvider::Github,&json!({"id":123}),&json!([
        {"email":"unverified@example.test","primary":true,"verified":false},
        {"email":"VERIFIED@example.test","primary":false,"verified":true}]))?;
    assert_eq!(github.subject,"123"); assert_eq!(github.email.as_deref(),Some("verified@example.test")); assert!(github.trusted_email);
    assert!(!identity(AuthProvider::Github,&json!({"id":123}),&json!([]))?.trusted_email);
    assert!(identity(AuthProvider::Microsoft,&json!({"email":"owner@example.test"}),&Value::Null).is_err());
    Ok(())
}

/// First Microsoft sign-in cannot take over an existing email account; ownership confirmation links it once.
#[tokio::test]
async fn social_email_verification_links_without_duplicate_grants()->anyhow::Result<()> {
    let app=crate::tests::application()?;
    let owner=app.db.user("owner@example.test",None,100)?;
    let device=crate::auth::start(State(app.clone())).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    app.db.0.lock().unwrap().execute("INSERT INTO social_signins(device_id,provider) VALUES(?1,'microsoft')",[&device.device_id])?;
    assert!(!complete(&app,&device.device_id,AuthProvider::Microsoft,Identity { subject:"ms-user".into(),email:Some("owner@example.test".into()),trusted_email:false })?);
    assert_eq!(app.db.identity_user(AuthProvider::Microsoft,"ms-user")?,None);
    let pending=crate::auth::poll(State(app.clone()),Json(PollLogin { device_id:device.device_id.clone(),poll_secret:device.poll_secret.clone() })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    assert!(pending.token.is_none()); assert_eq!(pending.email_verification.unwrap().provider,AuthProvider::Microsoft);
    let challenge=crate::auth::email(State(app.clone()),Json(protocol::EmailLogin { device_id:device.device_id.clone(),poll_secret:device.poll_secret.clone(),email:"owner@example.test".into() })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    let signed_in=crate::auth::verify(State(app.clone()),Json(protocol::VerifyEmail { device_id:device.device_id,poll_secret:device.poll_secret,code:challenge.development_code.unwrap() })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    assert_eq!(app.db.authenticate(signed_in.token.as_deref().unwrap())?,owner);
    assert_eq!(app.db.identity_user(AuthProvider::Microsoft,"ms-user")?,Some(owner));
    assert_eq!(app.db.social_user("owner@example.test",AuthProvider::Github,"gh-user",100)?,owner);
    assert_eq!(app.db.social_user("owner@example.test",AuthProvider::Google,"g-user",100)?,owner);
    assert_eq!(app.db.balance(owner)?,100);
    let next=crate::auth::start(State(app.clone())).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    assert!(complete(&app,&next.device_id,AuthProvider::Microsoft,Identity { subject:"ms-user".into(),email:Some("changed@example.test".into()),trusted_email:false })?);
    let result=crate::auth::poll(State(app.clone()),Json(PollLogin { device_id:next.device_id,poll_secret:next.poll_secret })).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    assert_eq!(app.db.authenticate(result.token.as_deref().unwrap())?,owner);
    Ok(())
}
