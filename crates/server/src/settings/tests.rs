use super::*;
use axum::{body::{Body,to_bytes},http::{Request,StatusCode}};
use tower::ServiceExt;

/// Exercise setup through the real public route, including host/origin checks and session issuance.
async fn configured()->Result<(App,String)> {
    let app=App::testing(Config { dev:true,public_url:String::new(),..Config::default() })?;
    let response=crate::router(app.clone()).oneshot(Request::builder().method("POST").uri("/admin/setup")
        .header(header::HOST,"assistant.example.test").header(header::ORIGIN,"http://assistant.example.test")
        .header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .body(Body::from("password=first-admin-password&confirmation=first-admin-password"))?).await?;
    assert_eq!(response.status(),StatusCode::SEE_OTHER);
    let cookie=response.headers()[header::SET_COOKIE].to_str()?.split(';').next().unwrap().to_owned();
    Ok((app,cookie))
}

/// Use the same authenticated JSON contract as the dashboard settings form.
async fn request(app:&App,cookie:&str,value:Option<Value>)->Result<(StatusCode,HeaderMap,Value)> {
    let mut request=Request::builder().uri("/admin/api/settings").header(header::COOKIE,cookie);
    if value.is_some() { request=request.method("POST").header(header::CONTENT_TYPE,"application/json").header("X-Admin-Request","1"); }
    let response=crate::router(app.clone()).oneshot(request.body(value.map_or_else(Body::empty,|value|Body::from(value.to_string())))?).await?;
    let (parts,body)=response.into_parts();
    let bytes=to_bytes(body,128*1024).await?;
    Ok((parts.status,parts.headers,if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes)? }))
}

/// Fresh setup exposes no operational data, is single-use, and persists only the password hash.
#[tokio::test]
async fn settings_first_run_and_password_guard()->Result<()> {
    let app=App::testing(Config::default())?;
    let response=crate::router(app.clone()).oneshot(Request::builder().uri("/admin").body(Body::empty())?).await?;
    assert_eq!(response.headers()[header::LOCATION],"/admin/setup");
    assert_eq!(request(&app,"",None).await?.0,StatusCode::UNAUTHORIZED);
    let rejected=crate::router(app).oneshot(Request::builder().method("POST").uri("/admin/setup").header(header::HOST,"assistant.example.test")
        .header(header::ORIGIN,"https://attacker.example").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .body(Body::from("password=first-admin-password&confirmation=first-admin-password"))?).await?;
    assert_eq!(rejected.status(),StatusCode::FORBIDDEN);
    let (app,cookie)=configured().await?;
    let stored=load(&app.db)?;
    assert!(password_matches("first-admin-password",stored.admin_password_hash.as_deref().unwrap()));
    assert!(!password_matches("wrong-password",stored.admin_password_hash.as_deref().unwrap()));
    assert!(!stored.admin_password_hash.unwrap().contains("first-admin-password"));
    assert_eq!(request(&app,&cookie,None).await?.0,StatusCode::OK);
    let response=crate::router(app.clone()).oneshot(Request::builder().method("POST").uri("/admin/setup")
        .header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .body(Body::from("password=another-admin-password&confirmation=another-admin-password"))?).await?;
    assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    assert!(password_matches("first-admin-password",app.config().admin_password_hash.as_deref().unwrap()));
    assert_eq!(request(&app,"",None).await?.0,StatusCode::UNAUTHORIZED);
    Ok(())
}

/// Saving takes effect immediately, keeps active snapshots stable, and never returns integration secrets.
#[tokio::test]
async fn settings_hot_reload_secrets_and_restart()->Result<()> {
    let (app,cookie)=configured().await?;
    let old=app.config();
    let mut settings=request(&app,&cookie,None).await?.2["settings"].clone();
    settings["model"]=json!("operator-model"); settings["free_credits"]=json!(42);
    settings["google_client_id"]=json!("fixture-google"); settings["google_client_secret"]=json!("private-google-secret"); settings["google_enabled"]=json!(true);
    settings["pricing"]["input_credits_per_million"]=json!("123.456789");
    settings["credit_packs"]=json!([{"price_id":"price_small","usd":"2.99","credits":100},{"price_id":"price_medium","usd":"5.49","credits":200},{"price_id":"price_large","usd":"9.99","credits":500}]);
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":null}))).await?.0,StatusCode::OK);
    assert_eq!(app.config().model.as_deref(),Some("operator-model"));
    assert!(old.model.is_none());
    let restored=load(&app.db)?;
    assert_eq!(restored.free_credits,42);
    assert_eq!(restored.pricing.input_credits_per_million.0,123_456_789);
    assert_eq!(restored.credit_packs.len(),3);
    assert_eq!(restored.credit_packs[0].cents()?,299);
    assert_eq!(restored.google_client_secret.as_deref(),Some("private-google-secret"));
    let public=request(&app,&cookie,None).await?.2;
    assert!(public["settings"]["google_client_secret"].is_null());
    assert!(public["saved_secrets"].as_array().unwrap().contains(&json!("google_client_secret")));
    assert!(!public.to_string().contains("private-google-secret"));
    assert!(public["settings"].get("admin_password_hash").is_none());
    let mut settings=public["settings"].clone(); settings["model"]=json!("changed-model");
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":null}))).await?.0,StatusCode::OK);
    assert_eq!(app.config().google_client_secret.as_deref(),Some("private-google-secret"));
    settings["google_enabled"]=json!(false); settings["google_client_id"]=Value::Null;
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":["google_client_secret"],"new_password":null}))).await?.0,StatusCode::OK);
    assert!(load(&app.db)?.google_client_secret.is_none());
    Ok(())
}

/// Password rotation revokes other sessions while returning a replacement cookie to the saving browser.
#[tokio::test]
async fn settings_password_rotation_and_validation()->Result<()> {
    let (app,cookie)=configured().await?;
    let previous_hash=app.config().admin_password_hash.clone().unwrap();
    let mut settings=request(&app,&cookie,None).await?.2["settings"].clone();
    settings["email_enabled"]=json!(true);
    settings["smtp_username"]=json!("incomplete");
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":null}))).await?.0,StatusCode::BAD_REQUEST);
    assert!(!app.config().email_enabled);
    settings=request(&app,&cookie,None).await?.2["settings"].clone();
    let (status,headers,_)=request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":"a-new-administrator-password"}))).await?;
    assert_eq!(status,StatusCode::OK);
    let new_cookie=headers[header::SET_COOKIE].to_str()?.split(';').next().unwrap();
    assert_eq!(request(&app,&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,new_cookie,None).await?.0,StatusCode::OK);
    assert!(crate::admin::session_cookie(&app,&previous_hash).is_err());
    assert!(password_matches("a-new-administrator-password",load(&app.db)?.admin_password_hash.as_deref().unwrap()));
    Ok(())
}

/// Sign-in flags affect discovery and backend enforcement; disabled primary email cannot issue a code.
#[tokio::test]
async fn settings_disabled_sign_ins_are_enforced()->Result<()> {
    let (app,cookie)=configured().await?;
    let mut settings=request(&app,&cookie,None).await?.2["settings"].clone();
    settings["google_client_id"]=json!("fixture-id"); settings["google_client_secret"]=json!("fixture-secret");
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":null}))).await?.0,StatusCode::OK);
    let providers=crate::auth::social::providers(State(app.clone())).await.0;
    assert_eq!(providers,json!({"providers":[],"email_enabled":false}));
    let device=crate::auth::start(State(app.clone())).await.map_err(|error|anyhow::anyhow!("{}",error.1))?.0;
    let response=crate::router(app.clone()).oneshot(Request::builder().uri(format!("/auth/google/{}",device.device_id)).body(Body::empty())?).await?;
    assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    let email=crate::auth::email(State(app.clone()),Json(protocol::EmailLogin { device_id:device.device_id,poll_secret:device.poll_secret,email:"user@example.test".into() })).await;
    assert!(email.is_err());
    settings["google_enabled"]=json!(true);
    assert_eq!(request(&app,&cookie,Some(json!({"settings":settings,"clear_secrets":[],"new_password":null}))).await?.0,StatusCode::OK);
    assert_eq!(crate::auth::social::providers(State(app.clone())).await.0["providers"],json!(["google"]));
    Ok(())
}
