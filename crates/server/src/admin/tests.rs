use super::*;
use axum::body::to_bytes;

use std::sync::Arc;
use tower::ServiceExt;

/// Build a private test dashboard with no real accounts, mail, or payment credentials.
fn application() -> anyhow::Result<App> {
    crate::diagnostics::init_test();
    let mut config=crate::config::Config { dev:true,public_url:"http://127.0.0.1:8787".into(),email_enabled:true,..crate::config::Config::default() };
    config.admin_password_hash=Some(crate::settings::password_hash("local-test-password-only"));
    App::testing(config)
}

/// Exercise real login cookies, rather than bypassing the dashboard authorization layer.
async fn sign_in(router: &Router) -> anyhow::Result<String> {
    let response=router.clone().oneshot(Request::builder().method("POST").uri("/admin/login").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .body(Body::from("password=local-test-password-only"))?).await?;
    assert_eq!(response.status(),StatusCode::SEE_OTHER);
    let cookie=response.headers()[header::SET_COOKIE].to_str()?;
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    Ok(cookie.split(';').next().unwrap().into())
}

/// Send authenticated JSON mutations and read filtered report pages through the public router.
async fn request(router: &Router, path: &str, cookie: &str, body: Option<Value>) -> anyhow::Result<(StatusCode,Value)> {
    let mut request=Request::builder().uri(path).header(header::COOKIE,cookie);
    if body.is_some() { request=request.method("POST").header(header::CONTENT_TYPE,"application/json").header("X-Admin-Request","1"); }
    let response=router.clone().oneshot(request.body(Body::from(body.map(|value|value.to_string()).unwrap_or_default()))?).await?;
    let status=response.status();
    let bytes=to_bytes(response.into_body(),2_000_000).await?;
    Ok((status,serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

/// Admin authentication is separate, cookies rotate with the password, and CSRF cannot mutate users.
#[tokio::test]
async fn authentication_and_browser_boundaries() -> anyhow::Result<()> {
    let state=application()?;
    let app=crate::router(state.clone());
    assert_eq!(request(&app,"/admin/api/users","",None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,"/admin/api/logs","",None).await?.0,StatusCode::UNAUTHORIZED);
    let invalid=app.clone().oneshot(Request::builder().method("POST").uri("/admin/login").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded").body(Body::from("password=wrong"))?).await?;
    assert_eq!(invalid.status(),StatusCode::UNAUTHORIZED);
    let cookie=sign_in(&app).await?;
    for (path,body) in [("/admin/api/accounts",json!({"api_key":null})),("/admin/api/accounts/missing/pause",json!({"paused":true})),("/admin/api/accounts/missing/delete",json!({})),("/admin/api/settings/restore",json!({})),("/admin/api/accounts/login/00000000-0000-0000-0000-000000000000/cancel",json!({}))] {
        assert_eq!(request(&app,path,"",Some(body.clone())).await?.0,StatusCode::UNAUTHORIZED);
        let response=app.clone().oneshot(Request::builder().method("POST").uri(path).header(header::COOKIE,&cookie).header(header::CONTENT_TYPE,"application/json").body(Body::from(body.to_string()))?).await?;
        assert_eq!(response.status(),StatusCode::FORBIDDEN);
    }
    assert_eq!(request(&app,"/admin/api/users",&cookie,None).await?.0,StatusCode::OK);
    let reference=uuid::Uuid::new_v4().to_string();
    crate::diagnostics::record("error","quota_refresh",Some(&reference),Some("account-1"),"TLS failed 世界; Bearer private-test-token");
    let (status,logs)=request(&app,&format!("/admin/api/logs?search={reference}"),&cookie,None).await?;
    assert_eq!(status,StatusCode::OK);
    assert!(logs["entries"][0]["message"].as_str().unwrap().contains("TLS failed 世界"));
    assert!(!logs.to_string().contains("private-test-token"));
    assert_eq!(request(&app,"/admin/api/logs?level=invalid",&cookie,None).await?.0,StatusCode::BAD_REQUEST);
    let user=state.db.user("用户@example.test",None,10)?;
    state.db.0.lock().unwrap().execute("INSERT INTO sessions VALUES(?1,?2,?3)",params![hash("ordinary-user"),user,now()+60])?;
    let response=app.clone().oneshot(Request::builder().uri("/admin/api/logs").header(header::AUTHORIZATION,"Bearer ordinary-user").body(Body::empty())?).await?;
    assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    let response=app.clone().oneshot(Request::builder().uri("/admin/api/logs").header(header::COOKIE,&cookie).header(header::ORIGIN,"https://attacker.example").body(Body::empty())?).await?;
    assert_eq!(response.status(),StatusCode::FORBIDDEN);
    let response=app.clone().oneshot(Request::builder().method("POST").uri(format!("/admin/api/users/{user}/block")).header(header::COOKIE,&cookie)
        .header(header::CONTENT_TYPE,"application/json").body(Body::from("{\"blocked\":true}"))?).await?;
    assert_eq!(response.status(),StatusCode::FORBIDDEN);
    let response=app.clone().oneshot(Request::builder().method("POST").uri(format!("/admin/api/users/{user}/block")).header(header::COOKIE,&cookie)
        .header(header::CONTENT_TYPE,"application/json").header("X-Admin-Request","1").header(header::ORIGIN,"https://attacker.example").body(Body::from("{\"blocked\":true}"))?).await?;
    assert_eq!(response.status(),StatusCode::FORBIDDEN);
    Arc::make_mut(&mut *state.settings.write().unwrap()).config.admin_password_hash=Some(crate::settings::password_hash("changed-test-password"));
    assert_eq!(request(&crate::router(state.clone()),"/admin/api/logs",&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&crate::router(state.clone()),"/admin/api/users",&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,"/admin/api/logout",&cookie,Some(json!({}))).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,"/admin/api/users",&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    Arc::make_mut(&mut *state.settings.write().unwrap()).config.admin_password_hash=None;
    assert_eq!(request(&crate::router(state.clone()),"/admin/api/logs",&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&crate::router(state),"/admin/api/users",&cookie,None).await?.0,StatusCode::UNAUTHORIZED);
    Ok(())
}

/// Credit retries are idempotent and blocking/revoking never erases the user's financial history.
#[tokio::test]
async fn adjustments_and_user_access() -> anyhow::Result<()> {
    let state=application()?;
    let user=state.db.user("buyer@example.test",None,10)?;
    let app=crate::router(state.clone());
    let cookie=sign_in(&app).await?;
    let input=json!({"id":uuid::Uuid::new_v4(),"delta":20,"note":"Verified external payment 测试"});
    let path=format!("/admin/api/users/{user}/credits");
    for _ in 0..2 { assert_eq!(request(&app,&path,&cookie,Some(input.clone())).await?.1["credits"],30); }
    let mut changed=input.clone();changed["delta"]=json!(21);
    assert_eq!(request(&app,&path,&cookie,Some(changed)).await?.0,StatusCode::BAD_REQUEST);
    assert_eq!(request(&app,&path,&cookie,Some(json!({"id":uuid::Uuid::new_v4(),"delta":-31,"note":"Correction"}))).await?.1["credits"],-1);
    assert_eq!(request(&app,&path,&cookie,Some(json!({"id":uuid::Uuid::new_v4(),"delta":31,"note":"Restore"}))).await?.1["credits"],30);
    assert_eq!(request(&app,&path,&cookie,Some(json!({"id":uuid::Uuid::new_v4(),"delta":-5,"note":"Correction"}))).await?.1["credits"],25);
    state.db.0.lock().unwrap().execute("INSERT INTO sessions VALUES(?1,?2,?3)",params![hash("user-session"),user,now()+60])?;
    assert_eq!(state.db.authenticate("user-session")?,user);
    request(&app,&format!("/admin/api/users/{user}/block"),&cookie,Some(json!({"blocked":true}))).await?;
    assert!(state.db.authenticate("user-session").is_err());
    request(&app,&format!("/admin/api/users/{user}/block"),&cookie,Some(json!({"blocked":false}))).await?;
    assert_eq!(state.db.authenticate("user-session")?,user);
    request(&app,&format!("/admin/api/users/{user}/revoke"),&cookie,Some(json!({}))).await?;
    assert!(state.db.authenticate("user-session").is_err());
    assert_eq!(state.db.balance(user)?,25);
    Ok(())
}

/// Reports preserve unknown counts, include partial failures, and filter account/purpose without double-counting.
#[tokio::test]
async fn reporting_and_recovery() -> anyhow::Result<()> {
    let state=application()?;
    let user=state.db.user("report@example.test",None,25)?;
    {
        let conn=state.db.0.lock().unwrap();
        conn.execute("INSERT INTO usage(user_id,request_id,conversation_id,account_home,purpose,started_at,finished_at,input_tokens,cached_tokens,output_tokens,reasoning_tokens,total_tokens)
            VALUES(?1,'a','c','account-1','conversation',?2,?2,100,20,10,5,110)",params![user,now()])?;
        conn.execute("INSERT INTO usage(user_id,request_id,conversation_id,account_home,purpose,started_at,error)
            VALUES(?1,'b','c','account-2','output_analysis',?2,'Partial failure')",params![user,now()])?;
        conn.execute("INSERT INTO usage(request_id,conversation_id,account_home,purpose,started_at,finished_at,total_tokens)
            VALUES('c','d','account-1','admin_test',?1,?1,12)",[now()])?;
        conn.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,-1,'request:test',?2),(?1,1,'refund:test',?2)",params![user,now()])?;
        conn.execute("INSERT INTO requests(user_id,id,task_id,conversation_id,fingerprint,response) VALUES(?1,'a','a','c','test','{}')",[user])?;
        conn.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,-1,'usage:1',?2)",params![user,now()])?;
    }
    let app=crate::router(state.clone());
    let cookie=sign_in(&app).await?;
    let (status,report)=request(&app,"/admin/api/report",&cookie,None).await?;
    assert_eq!(status,StatusCode::OK);
    assert_eq!(report["summary"]["total_tokens"],122);
    assert_eq!(report["summary"]["measured"],2);
    assert_eq!(report["summary"]["attempts"],3);
    assert_eq!(report["credits"]["debited"],2);
    assert_eq!(report["credits"]["refunded"],1);
    assert_eq!(report["summary"]["credits_charged"],1);
    assert_eq!(report["pricing"],json!({"input":2.0,"cached":0.1,"output":10.0}));
    let (_,filtered)=request(&app,"/admin/api/report?account=account-2&purpose=output_analysis",&cookie,None).await?;
    assert_eq!(filtered["summary"]["attempts"],1);
    assert!(filtered["summary"]["total_tokens"].is_null());
    assert_eq!(filtered["summary"]["credits_charged"],0);
    assert_eq!(request(&app,"/admin/api/report?from=0&to=999999999999",&cookie,None).await?.0,StatusCode::BAD_REQUEST);
    let (_,users)=request(&app,"/admin/api/users?search=report",&cookie,None).await?;
    assert_eq!(users["users"][0]["tokens"],110);
    assert_eq!(users["users"][0]["unknown_attempts"],1);
    state.db.recover()?;
    assert_eq!(state.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE finished_at IS NULL",[],|r|r.get::<_,i64>(0))?,0);
    Ok(())
}
