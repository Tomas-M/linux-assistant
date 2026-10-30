use std::{ops::Deref,sync::Arc};
use anyhow::Result;
use axum::{extract::State,http::{header,HeaderMap,StatusCode},response::{Html,IntoResponse,Redirect,Response},Form,Json};
use hmac::{Hmac,Mac};
use rusqlite::{params,OptionalExtension};
use serde::Deserialize;
use serde_json::{json,Value};
use crate::{config::{Config,FieldError},db::{secret,Db},App,ApiError,ApiResult};
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct Runtime { pub config:Config,pub mail:Option<lettre::AsyncSmtpTransport<lettre::Tokio1Executor>> }
impl Deref for Runtime {
    type Target=Config;
    /// Existing generation code consumes the same immutable configuration snapshot.
    fn deref(&self)->&Config { &self.config }
}
impl Runtime {
    /// Prepare optional integrations before publishing a new configuration to requests.
    pub fn new(config:Config)->Result<Self> {
        if !config.public_url.is_empty() { config.validate()?; }
        let mail=if config.dev || config.smtp_username.is_none() { None } else { Some(crate::mail::transport(&config)?.build()) };
        Ok(Self { config,mail })
    }
}

pub const SECRETS:[&str;6]=["smtp_password","google_client_secret","microsoft_client_secret","github_client_secret","stripe_secret","stripe_webhook_secret"];

/// Load only the current database settings format; no environment configuration or import path exists.
pub fn load(db:&Db)->Result<Config> {
    let row:Option<(String,Option<String>)>=db.0.lock().unwrap().query_row("SELECT settings,password_hash FROM backend_settings WHERE id=1",[],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((value,password_hash))=row else { return Ok(Config::default()); };
    let mut config:Config=serde_json::from_str(&value)?;
    config.admin_password_hash=password_hash;
    Ok(config)
}

/// Salt and stretch administrator passwords; raw passwords never enter the database.
pub fn password_hash(password:&str)->String {
    let salt=secret();
    format!("{salt}:{}",hex::encode(password_key(password,&salt)))
}

/// Use the same fixed parameters for verification and password creation.
fn password_key(password:&str,salt:&str)->[u8;32] {
    let mut key=[0;32];
    let rounds=if cfg!(test) { 1000 } else { 600_000 };
    pbkdf2::pbkdf2::<Hmac<sha2::Sha256>>(password.as_bytes(),salt.as_bytes(),rounds,&mut key).unwrap();
    key
}

/// Verify the derived key in constant time without comparing raw password strings.
pub fn password_matches(password:&str,stored:&str)->bool {
    let Some((salt,expected))=stored.split_once(':') else { return false; };
    let Ok(expected)=hex::decode(expected) else { return false; };
    let mut supplied=Hmac::<sha2::Sha256>::new_from_slice(&password_key(password,salt)).unwrap();
    supplied.update(b"admin-login");
    let mut saved=Hmac::<sha2::Sha256>::new_from_slice(&expected).unwrap();
    saved.update(b"admin-login");
    saved.verify_slice(&supplied.finalize().into_bytes()).is_ok()
}

/// Initial setup is public only until the first password has been committed.
pub async fn setup_page(State(app):State<App>)->Response {
    if app.config().admin_password_hash.is_some() { return Redirect::to("/admin/login").into_response(); }
    Html(include_str!("admin/setup.html").replace("{{ERROR}}","")).into_response()
}

#[derive(Deserialize)]
pub struct Setup { password:String,confirmation:String }

/// A database condition makes first-run setup single-winner even when two browsers submit together.
pub async fn setup(State(app):State<App>,headers:HeaderMap,Form(input):Form<Setup>)->ApiResult<Response> {
    if app.config().admin_password_hash.is_some() { return Err(ApiError::bad("Administrator is already configured")); }
    if input.password.chars().count()<16 || input.password.len()>1024 || input.password!=input.confirmation {
        return Ok(Html(include_str!("admin/setup.html").replace("{{ERROR}}","Use at least 16 characters, at most 1024 bytes, and enter the password identically twice.")).into_response());
    }
    crate::diagnostics::protect(&input.password);
    let password_hash=tokio::task::spawn_blocking(move||password_hash(&input.password)).await?;
    {
        let mut runtime=app.settings.write().unwrap();
        if runtime.admin_password_hash.is_some() { return Err(ApiError::bad("Administrator is already configured")); }
        let mut config=runtime.config.clone();
        config.admin_password_hash=Some(password_hash.clone());
        if config.public_url.is_empty() {
            config.public_url=match headers.get(header::ORIGIN) { Some(origin)=>origin.to_str()?.to_owned(),None=>format!("https://{}",headers.get(header::HOST).ok_or_else(||ApiError::bad("Missing host"))?.to_str()?) };
        }
        let next=Runtime::new(config)?;
        let changed=app.db.0.lock().unwrap().execute("INSERT INTO backend_settings(id,settings,password_hash) VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET settings=excluded.settings,password_hash=excluded.password_hash WHERE backend_settings.password_hash IS NULL",params![serde_json::to_string(&next.config)?,password_hash])?;
        if changed!=1 { return Err(ApiError::bad("Administrator is already configured")); }
        *runtime=Arc::new(next);
    }
    Ok(([(header::SET_COOKIE,crate::admin::session_cookie(&app,&password_hash)?)],Redirect::to("/admin#settings")).into_response())
}

/// Display saved fields but only existence flags for stored integration secrets.
pub async fn view(State(app):State<App>)->ApiResult<Json<Value>> {
    let config=app.config();
    let mut value=serde_json::to_value(&config.config)?;
    let saved=SECRETS.into_iter().filter(|key|!value[*key].is_null()).collect::<Vec<_>>();
    for key in SECRETS { value[key]=Value::Null; }
    Ok(Json(json!({"settings":value,"saved_secrets":saved})))
}

#[derive(Deserialize)]
pub struct Save { settings:Config,clear_secrets:Vec<String>,new_password:Option<String> }

impl IntoResponse for FieldError {
    /// Let the dashboard focus the invalid setting without exposing rejected values or credentials.
    fn into_response(self)->Response { (StatusCode::BAD_REQUEST,Json(json!({"error":self.message,"field":self.field}))).into_response() }
}

/// Save atomically and reload integrations without restarting or interrupting running AI tasks.
pub async fn save(State(app):State<App>,Json(value):Json<Value>)->ApiResult<Response> {
    // Preserve field paths for type/range errors as well as semantic validation failures.
    let input:Save=match serde_path_to_error::deserialize(value) {
        Ok(input)=>input,
        Err(error)=>{
            let path=error.path().to_string().replace('[',".").replace(']',"");
            let field=path.strip_prefix("settings.").unwrap_or(&path);
            let message=if field.starts_with("pricing.") { "Enter a nonnegative rate with at most six decimal places" }
                else if field=="smtp_port" { "Enter a whole number between 1 and 65535" }
                else { "Enter a valid value for this setting" };
            return Ok(FieldError::new(field,message).into_response());
        }
    };
    let password=if let Some(password)=input.new_password {
        if password.chars().count()<16 || password.len()>1024 { return Ok(FieldError::new("new_password","Use at least 16 characters and at most 1024 bytes for the administrator password").into_response()); }
        crate::diagnostics::protect(&password);
        Some(tokio::task::spawn_blocking(move||password_hash(&password)).await?)
    } else { None };
    {
        let mut runtime=app.settings.write().unwrap();
        let previous=serde_json::to_value(&runtime.config)?;
        let mut value=serde_json::to_value(input.settings)?;
        if input.clear_secrets.iter().any(|key|!SECRETS.contains(&key.as_str())) { return Err(ApiError::bad("Unknown secret field")); }
        for key in SECRETS {
            if input.clear_secrets.iter().any(|clear|clear==key) { value[key]=Value::Null; }
            else if value[key].is_null() { value[key]=previous[key].clone(); }
            if let Some(secret)=value[key].as_str() { crate::diagnostics::protect(secret); }
        }
        let mut config:Config=serde_json::from_value(value)?;
        config.database=runtime.database.clone(); config.listen=runtime.listen; config.dev=runtime.dev;
        config.admin_password_hash=password.clone().or_else(||runtime.admin_password_hash.clone());
        config.public_url=config.public_url.trim_end_matches('/').to_owned();
        if let Err(error)=config.validate() { return Ok(error.into_response()); }
        let next=Runtime::new(config)?;
        let mut connection=app.db.0.lock().unwrap();
        let tx=connection.transaction()?;
        tx.execute("UPDATE backend_settings SET settings=?1,password_hash=?2 WHERE id=1",params![serde_json::to_string(&next.config)?,next.admin_password_hash])?;
        if password.is_some() { tx.execute("DELETE FROM admin_sessions",[])?; }
        tx.commit()?;
        *runtime=Arc::new(next);
    }
    crate::diagnostics::record("info","settings",None,None,"Backend settings updated");
    if let Some(password)=password { return Ok(([(header::SET_COOKIE,crate::admin::session_cookie(&app,&password)?)],Json(json!({"ok":true}))).into_response()); }
    Ok(Json(json!({"ok":true})).into_response())
}
