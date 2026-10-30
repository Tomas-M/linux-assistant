mod reports;
#[cfg(test)]
mod tests;

use axum::{body::Body, extract::{DefaultBodyLimit, Path, State}, http::{header, HeaderMap, Request, StatusCode}, middleware::{self, Next}, response::{Html, IntoResponse, Redirect, Response}, routing::{get, post}, Form, Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use crate::{db::{hash, now, secret}, App, ApiError, ApiResult};

/// Keep every administrative data route behind the same session and browser-write checks.
pub fn router(app: App) -> Router<App> {
    let private=Router::new().route("/admin",get(||async { Html(include_str!("admin/dashboard.html").replace("{{ICONS}}",protocol::ICONS_SVG)) }))
        .route("/admin/api/accounts",get(accounts).post(add_account)).route("/admin/api/report",get(reports::report))
        .route("/admin/api/accounts/login/{id}/cancel",post(cancel_account_login)).route("/admin/api/accounts/{id}/pause",post(pause_account))
        .route("/admin/api/accounts/{id}/delete",post(delete_account))
        .route("/admin/api/logs",get(crate::diagnostics::list))
        .route("/admin/api/users",get(reports::users)).route("/admin/api/accounts/{id}/test",post(test_account))
        .route("/admin/api/users/{id}/credits",post(credits)).route("/admin/api/users/{id}/block",post(block))
        .route("/admin/api/users/{id}/revoke",post(revoke)).route("/admin/api/users/{id}/checkout",post(checkout))
        .route("/admin/api/settings",get(crate::settings::view).post(crate::settings::save))
        .route("/admin/api/settings/backup",get(crate::settings::download))
        .route("/admin/api/settings/restore",post(crate::settings::restore).layer(DefaultBodyLimit::max(16*1024*1024)))
        .route("/admin/api/logout",post(logout)).route_layer(middleware::from_fn_with_state(app,authorize));
    private.route("/admin/setup",get(crate::settings::setup_page).post(crate::settings::setup))
        .route("/admin/login",get(|State(app):State<App>|async move {
            if app.config().admin_password_hash.is_none() { Redirect::to("/admin/setup").into_response() }
            else { Html(include_str!("admin/login.html")).into_response() }
        }).post(login))
        .route("/admin/settings.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("admin/settings.js")) }))
        .route("/admin/dashboard.css",get(||async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("admin/dashboard.css")) }))
        .route("/admin/dashboard.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("admin/dashboard.js")) }))
}

/// Extract only the dedicated admin cookie; application bearer tokens never grant admin access.
fn token(headers: &HeaderMap) -> &str {
    headers.get(header::COOKIE).and_then(|value|value.to_str().ok()).unwrap_or("").split(';')
        .filter_map(|part|part.trim().split_once('=')).find(|(name,_)|*name=="assistant_admin").map(|(_,value)|value).unwrap_or("")
}

/// Password rotation invalidates all old sessions, and cross-site forms cannot perform mutations.
async fn authorize(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    let config=app.config();
    let Some(password)=&config.admin_password_hash else {
        return if request.uri().path()=="/admin" { Redirect::to("/admin/setup").into_response() }
            else { (StatusCode::UNAUTHORIZED,Json(json!({"error":"Set up the administrator password first"}))).into_response() };
    };
    let valid=app.db.0.lock().unwrap().query_row("SELECT 1 FROM admin_sessions WHERE token_hash=?1 AND password_hash=?2 AND expires_at>?3",
        params![hash(token(request.headers())),hash(password),now()],|_|Ok(())).optional();
    if !matches!(valid,Ok(Some(()))) {
        return if request.uri().path()=="/admin" { Redirect::to("/admin/login").into_response() } else { StatusCode::UNAUTHORIZED.into_response() };
    }
    if request.method()!=axum::http::Method::GET && request.headers().get("X-Admin-Request").and_then(|v|v.to_str().ok())!=Some("1") {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
struct Login { password: String }

/// Authenticate against the private server setting and issue an eight-hour HttpOnly cookie.
async fn login(State(app): State<App>, Form(input): Form<Login>) -> ApiResult<Response> {
    let config=app.config();
    let expected=config.admin_password_hash.clone().ok_or_else(||ApiError::bad("Set up the administrator password first"))?;
    // Bound password guessing globally without trusting client-supplied proxy addresses.
    static ATTEMPTS: std::sync::Mutex<(i64,u32)>=std::sync::Mutex::new((0,0));
    {
        let mut attempts=ATTEMPTS.lock().unwrap();
        if now()-attempts.0>=60 { *attempts=(now(),0); }
        if attempts.1>=20 { return Err(ApiError(StatusCode::TOO_MANY_REQUESTS,"Too many attempts; retry in a minute".into())); }
        attempts.1+=1;
    }
    let verified=tokio::task::spawn_blocking(move||crate::settings::password_matches(&input.password,&expected)).await?;
    if !verified { return Err(ApiError(StatusCode::UNAUTHORIZED,"Incorrect password; return to the login page and retry".into())); }
    Ok(([(header::SET_COOKIE,session_cookie(&app,config.admin_password_hash.as_deref().unwrap())?)],Redirect::to("/admin")).into_response())
}

/// Create an admin session bound to the current password fingerprint.
pub(super) fn session_cookie(app:&App,expected:&str)->ApiResult<String> {
    let config=app.config();
    if config.admin_password_hash.as_deref()!=Some(expected) { return Err(ApiError(StatusCode::UNAUTHORIZED,"Administrator password changed; sign in again".into())); }
    let token=secret();
    let conn=app.db.0.lock().unwrap();
    conn.execute("DELETE FROM admin_sessions WHERE expires_at<=?1 OR password_hash!=?2",params![now(),hash(expected)])?;
    conn.execute("INSERT INTO admin_sessions VALUES(?1,?2,?3)",params![hash(&token),hash(expected),now()+8*3600])?;
    Ok(format!("assistant_admin={token}; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=28800{}",if config.dev { "" } else { "; Secure" }))
}

/// Invalidate the server-side session as well as the browser cookie.
async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult<Response> {
    app.db.0.lock().unwrap().execute("DELETE FROM admin_sessions WHERE token_hash=?1",[hash(token(&headers))])?;
    Ok(([(header::SET_COOKIE,"assistant_admin=; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=0")],Json(json!({"ok":true}))).into_response())
}

/// Expose fresh or explicitly stale quota readings without waiting for running model requests.
async fn accounts(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"accounts":app.pool.account_status(&app.db)?,"login":app.pool.login_status().await,"now":now(),"pricing":app.config().pricing.usd_per_million()})))
}

#[derive(Deserialize)]
struct AddAccount { api_key:Option<String> }
/// Run sign-in independently of the HTTP connection; all credentials stay on the backend.
async fn add_account(State(app):State<App>,Json(input):Json<AddAccount>)->ApiResult<Json<Value>> {
    let config=app.config();
    if config.dev { return Err(ApiError::bad("Live account sign-in is disabled in development mode")); }
    tokio::spawn(async move {
        app.pool.add(app.db,input.api_key).await.map(Json).map_err(|error| {
            let message=crate::diagnostics::clean(&format!("{error:#}"));
            crate::diagnostics::record("error","account_login",None,None,&message);
            ApiError::bad(&message)
        })
    }).await?
}

/// Cancel only the pending sign-in referenced by the current admin page.
async fn cancel_account_login(State(app):State<App>,Path(id):Path<uuid::Uuid>)->ApiResult<Json<Value>> {
    app.pool.cancel_login(id).await?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
struct AccountAccess { paused:bool }
/// Pauses persist across restarts and never revoke credentials or kill a running turn.
async fn pause_account(State(app):State<App>,Path(id):Path<String>,Json(input):Json<AccountAccess>)->ApiResult<Json<Value>> {
    app.pool.pause(&app.db,&id,input.paused)?;
    Ok(Json(json!({"ok":true})))
}

/// Delete saved credentials and detach recoverable chats, retaining their messages and usage history.
async fn delete_account(State(app):State<App>,Path(id):Path<String>)->ApiResult<Json<Value>> {
    app.pool.remove(&app.db,&id).await.map_err(|error|ApiError::bad(&format!("{error:#}")))?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
struct TestPrompt { prompt: String }

/// Let an operator verify a specific account without silently falling back to another one.
async fn test_account(State(app): State<App>, Path(id): Path<String>, Json(input): Json<TestPrompt>) -> ApiResult<Json<Value>> {
    let config=app.config();
    if input.prompt.trim().is_empty() || input.prompt.len()>2000 { return Err(ApiError::bad("Enter a prompt of 1–2000 UTF-8 bytes")); }
    tokio::spawn(async move {
        app.pool.test_account(&config,&app.db,&id,input.prompt).await.map(Json).map_err(|error|ApiError::busy(&error.to_string()))
    }).await?
}

#[derive(Deserialize)]
struct CreditChange { id: uuid::Uuid, delta: i64, note: String }

/// Apply a signed adjustment once, retaining its reason and rejecting negative final balances.
async fn credits(State(app): State<App>, Path(user): Path<i64>, Json(input): Json<CreditChange>) -> ApiResult<Json<Value>> {
    if input.delta==0 || input.delta.unsigned_abs()>1_000_000_000 || input.note.trim().is_empty() || input.note.len()>500 { return Err(ApiError::bad("Provide a nonzero credit adjustment and a short reason")); }
    let mut conn=app.db.0.lock().unwrap();
    let tx=conn.transaction()?;
    let reference=format!("admin:{}",input.id);
    let previous=tx.query_row("SELECT l.user_id,l.delta,a.note FROM credit_adjustments a JOIN ledger l ON l.id=a.ledger_id WHERE a.id=?1",[input.id.to_string()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional()?;
    if let Some(previous)=previous {
        if previous!=(user,input.delta,input.note) { return Err(ApiError::bad("Adjustment ID was reused with different data")); }
    } else {
        let exists=tx.query_row("SELECT 1 FROM users WHERE id=?1",[user],|_|Ok(())).optional()?.is_some();
        if !exists { return Err(ApiError::bad("Unknown user")); }
        let balance:i64=tx.query_row("SELECT COALESCE(SUM(delta),0) FROM ledger WHERE user_id=?1",[user],|r|r.get(0))?;
        if balance.checked_add(input.delta).is_none() { return Err(ApiError::bad("Credit balance would overflow")); }
        tx.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,?2,?3,?4)",params![user,input.delta,reference,now()])?;
        tx.execute("INSERT INTO credit_adjustments VALUES(?1,?2,?3)",params![input.id.to_string(),tx.last_insert_rowid(),input.note])?;
    }
    tx.commit()?;
    drop(conn);
    Ok(Json(json!({"credits":app.db.balance(user)?})))
}

#[derive(Deserialize)]
struct Block { blocked: bool }

/// Blocking immediately invalidates application access while preserving balances and audit records.
async fn block(State(app): State<App>, Path(user): Path<i64>, Json(input): Json<Block>) -> ApiResult<Json<Value>> {
    let conn=app.db.0.lock().unwrap();
    if input.blocked { conn.execute("INSERT OR IGNORE INTO user_blocks VALUES(?1,?2)",params![user,now()])?; }
    else { conn.execute("DELETE FROM user_blocks WHERE user_id=?1",[user])?; }
    Ok(Json(json!({"ok":true})))
}

/// Force the selected user to sign in again on every client.
async fn revoke(State(app): State<App>, Path(user): Path<i64>) -> ApiResult<Json<Value>> {
    app.db.0.lock().unwrap().execute("DELETE FROM sessions WHERE user_id=?1",[user])?;
    Ok(Json(json!({"ok":true})))
}

/// Create a hosted payment URL for a user without needing the client application.
async fn checkout(State(app): State<App>, Path(user): Path<i64>,Json(input):Json<protocol::CheckoutRequest>) -> ApiResult<Json<Value>> {
    crate::payments::create_checkout(&app,user,&input.price_id).await
}
