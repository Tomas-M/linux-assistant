use super::*;
use serde::Serialize;

#[derive(Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
struct Backup { settings:Config,accounts:Vec<crate::provider::AccountBackup> }

/// A dedicated authenticated download includes integration secrets but omits administrator authentication.
pub async fn download(State(app):State<App>)->ApiResult<Response> {
    let config=app.config();
    let accounts=app.pool.backup(&app.db).await?;
    if !Arc::ptr_eq(&config,&app.config()) { return Err(ApiError::bad("Settings changed while preparing the backup; download it again")); }
    let backup=Backup { settings:config.config.clone(),accounts };
    Ok(([(header::CONTENT_DISPOSITION,format!("attachment; filename=\"linux-assistant-settings-{}.json\"",crate::db::now()))],Json(backup)).into_response())
}

/// Replace configuration and account credentials while retaining the current password and application data.
pub async fn restore(State(app):State<App>,Json(value):Json<Value>)->ApiResult<Response> {
    let backup:Backup=serde_json::from_value(value).map_err(|_|ApiError::bad("Invalid backup file. Select a JSON settings backup downloaded from this application"))?;
    let mut config=backup.settings;
    config.dev=app.config().dev;
    config.public_url=config.public_url.trim_end_matches('/').to_owned();
    if let Err(error)=config.validate() { return Ok(error.into_response()); }
    let values=serde_json::to_value(&config)?;
    for key in SECRETS { if let Some(value)=values[key].as_str() { crate::diagnostics::protect(value); } }
    for account in &backup.accounts {
        for key in ["/OPENAI_API_KEY","/tokens/id_token","/tokens/access_token","/tokens/refresh_token"] {
            if let Some(value)=account.auth.pointer(key).and_then(Value::as_str) { crate::diagnostics::protect(value); }
        }
    }
    app.pool.restore(&backup.accounts,|homes| {
        let mut runtime=app.settings.write().unwrap();
        config.database=runtime.database.clone(); config.listen=runtime.listen; config.dev=runtime.dev;
        config.admin_password_hash=runtime.admin_password_hash.clone();
        let next=Runtime::new(config)?;
        let mut connection=app.db.0.lock().unwrap(); let tx=connection.transaction()?;
        tx.execute("UPDATE backend_settings SET settings=?1 WHERE id=1",[serde_json::to_string(&next.config)?])?;
        tx.execute("DELETE FROM provider_accounts",[])?;
        tx.execute("DELETE FROM provider_pauses",[])?;
        tx.execute("UPDATE conversations SET account_home=NULL,thread_id=NULL",[])?;
        for (home,paused) in homes { if *paused { tx.execute("INSERT INTO provider_pauses(home) VALUES(?1)",[home])?; } }
        tx.commit()?;
        *runtime=Arc::new(next);
        Ok(())
    }).await.map_err(|error|ApiError::bad(&crate::diagnostics::clean(&format!("Could not restore backup: {error:#}"))))?;
    crate::diagnostics::record("info","settings",None,None,"Settings and connected AI accounts restored; administrator password preserved");
    if !app.config().dev { tokio::spawn(app.pool.clone().refresh(app.db.clone())); }
    Ok(Json(json!({"ok":true})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{Body,to_bytes},http::Request};
    use tower::ServiceExt;

    /// Use private temporary account homes and real admin cookies, with no external providers.
    fn fixture()->Result<(tempfile::TempDir,App,String)> {
        let directory=tempfile::tempdir()?;
        let config=Config { database:directory.path().join("assistant.db"),dev:true,public_url:"http://assistant.example.test".into(),
            smtp_username:Some("fixture@example.test".into()),smtp_password:Some("private-smtp-secret".into()),email_from:Some("Fixture <fixture@example.test>".into()),
            admin_password_hash:Some(password_hash("unchanged-admin-password")),..Config::default() };
        for auth in [json!({"OPENAI_API_KEY":"private-api-key"}),json!({"tokens":{"access_token":"private-access","refresh_token":"private-refresh","id_token":"private-id"}})] {
            let home=directory.path().join("managed-accounts").join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&home)?; std::fs::write(home.join("auth.json"),serde_json::to_vec(&auth)?)?;
        }
        let app=App::testing(config.clone())?;
        app.db.0.lock().unwrap().execute("INSERT INTO backend_settings(id,settings,password_hash) VALUES(1,?1,?2)",params![serde_json::to_string(&config)?,config.admin_password_hash])?;
        let cookie=crate::admin::session_cookie(&app,config.admin_password_hash.as_deref().unwrap()).map_err(|error|anyhow::anyhow!("{}",error.1))?.split(';').next().unwrap().to_owned();
        Ok((directory,app,cookie))
    }

    /// Exercise the same authenticated download and JSON upload routes as the browser.
    async fn transfer(app:&App,cookie:&str,backup:Option<Value>)->Result<(StatusCode,HeaderMap,Value)> {
        let path=if backup.is_some() { "/admin/api/settings/restore" } else { "/admin/api/settings/backup" };
        let mut request=Request::builder().uri(path).header(header::COOKIE,cookie);
        if backup.is_some() { request=request.method("POST").header(header::CONTENT_TYPE,"application/json").header("X-Admin-Request","1"); }
        let response=crate::router(app.clone()).oneshot(request.body(backup.map_or_else(Body::empty,|value|Body::from(value.to_string())))?).await?;
        let (parts,body)=response.into_parts(); let bytes=to_bytes(body,1024*1024).await?;
        Ok((parts.status,parts.headers,if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes)? }))
    }

    /// All settings and account credentials round-trip; passwords, users, balances and history remain local.
    #[tokio::test]
    async fn settings_backup_round_trip_preserves_application_data()->Result<()> {
        let (_directory,app,cookie)=fixture()?;
        assert_eq!(transfer(&app,"",None).await?.0,StatusCode::UNAUTHORIZED);
        let account=app.pool.account_status(&app.db)?[0].clone();
        app.pool.pause(&app.db,account["id"].as_str().unwrap(),true)?;
        let hash=app.config().admin_password_hash.clone();
        let (status,headers,backup)=transfer(&app,&cookie,None).await?;
        assert_eq!(status,StatusCode::OK); assert!(headers[header::CONTENT_DISPOSITION].to_str()?.starts_with("attachment;"));
        assert_eq!(backup["settings"]["smtp_password"],"private-smtp-secret");
        assert_eq!(backup["accounts"].as_array().unwrap().len(),2);
        assert!(!backup.to_string().contains("admin_password")); assert!(!backup.to_string().contains(hash.as_deref().unwrap()));
        let user=app.db.user("preserved@example.test",None,107)?;
        app.db.0.lock().unwrap().execute("INSERT INTO conversations(user_id,id,account_home,thread_id,checkpoint) VALUES(?1,'conversation',?2,'thread','Keep this summary')",params![user,account["home"].as_str().unwrap()])?;
        app.pool.remove(&app.db,account["id"].as_str().unwrap()).await?;
        let mut changed=app.config().config.clone(); changed.free_credits=999; changed.stripe_secret=Some("new-secret".into()); changed.stripe_webhook_secret=Some("new-webhook".into());
        crate::settings::save(State(app.clone()),Json(json!({"settings":changed,"clear_secrets":[],"new_password":null}))).await.map_err(|error|anyhow::anyhow!("{}",error.1))?;
        let (status,_,result)=transfer(&app,&cookie,Some(backup.clone())).await?;
        assert_eq!(status,StatusCode::OK,"{result}");
        assert_eq!(app.config().free_credits,100); assert!(app.config().stripe_secret.is_none());
        assert_eq!(app.config().admin_password_hash,hash); assert_eq!(load(&app.db)?.admin_password_hash,hash);
        assert_eq!(app.db.balance(user)?,107);
        assert_eq!(app.db.0.lock().unwrap().query_row("SELECT checkpoint FROM conversations WHERE user_id=?1",[user],|row|row.get::<_,String>(0))?,"Keep this summary");
        assert_eq!(app.pool.account_status(&app.db)?.iter().filter(|account|account["paused"]==true).count(),1);
        assert_eq!(transfer(&app,&cookie,None).await?.2,backup);
        let mut injected=backup.clone(); injected["new_password"]=json!("must-not-replace-password");
        assert_eq!(transfer(&app,&cookie,Some(injected)).await?.0,StatusCode::BAD_REQUEST);
        let mut invalid=backup; invalid["settings"]["email_from"]=json!("Invalid sender");
        assert_eq!(transfer(&app,&cookie,Some(invalid)).await?.0,StatusCode::BAD_REQUEST);
        assert_eq!(app.config().admin_password_hash,hash);
        Ok(())
    }

    /// A rejected restore or database failure must not erase the existing account directory or registry.
    #[tokio::test]
    async fn settings_backup_validation_and_file_rollback()->Result<()> {
        let (_directory,app,cookie)=fixture()?;
        let original=transfer(&app,&cookie,None).await?.2;
        let mut duplicate=original.clone(); duplicate["accounts"][1]=duplicate["accounts"][0].clone();
        assert_eq!(transfer(&app,&cookie,Some(duplicate)).await?.0,StatusCode::BAD_REQUEST);
        let accounts=vec![crate::provider::AccountBackup { id:uuid::Uuid::new_v4(),auth:json!({"OPENAI_API_KEY":"new-key"}),paused:false }];
        assert!(app.pool.restore(&accounts,|_|anyhow::bail!("Simulated database failure")).await.is_err());
        assert_eq!(transfer(&app,&cookie,None).await?.2,original);
        Ok(())
    }
}
