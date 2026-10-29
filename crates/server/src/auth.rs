use axum::{extract::{Path, Query, State}, http::{header, HeaderMap}, response::{Html, IntoResponse, Redirect, Response}, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use protocol::{DeviceLogin, EmailChallenge, EmailLogin, LoginResult, PollLogin, VerifyEmail};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::{db::{hash, now, secret}, App, ApiError, ApiResult};

/// Begin a short-lived sign-in owned by the client, without navigating to the backend.
pub async fn start(State(app): State<App>) -> ApiResult<Json<DeviceLogin>> {
    let device_id = secret();
    let poll_secret = secret();
    let expires_at = now()+600;
    let conn = app.db.0.lock().unwrap();
    conn.execute("DELETE FROM oauth WHERE expires_at<?1",[now()])?;
    conn.execute("DELETE FROM devices WHERE expires_at<?1 AND id NOT IN (SELECT device_id FROM oauth)",[now()])?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM devices",[],|r|r.get(0))?;
    if count >= 1000 { return Err(ApiError::busy("Too many pending sign-ins")); }
    conn.execute("INSERT INTO devices(id,poll_hash,expires_at) VALUES(?1,?2,?3)",params![device_id,hash(&poll_secret),expires_at])?;
    Ok(Json(DeviceLogin { device_id,poll_secret,expires_at }))
}

/// Exchange an approved device for a single-use session issuance.
pub async fn poll(State(app): State<App>, Json(input): Json<PollLogin>) -> ApiResult<Json<LoginResult>> {
    let mut conn = app.db.0.lock().unwrap();
    let tx = conn.transaction()?;
    let user: Option<Option<i64>> = tx.query_row("SELECT user_id FROM devices WHERE id=?1 AND poll_hash=?2 AND expires_at>?3",params![input.device_id,hash(&input.poll_secret),now()],|r|r.get(0)).optional()?;
    let Some(user) = user else { return Err(ApiError::bad("Sign-in expired or already consumed")); };
    let Some(user) = user else { return Ok(Json(LoginResult { token:None })); };
    let token = secret();
    tx.execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",params![hash(&token),user,now()+30*86400])?;
    tx.execute("DELETE FROM oauth WHERE device_id=?1",[&input.device_id])?;
    tx.execute("DELETE FROM devices WHERE id=?1",[&input.device_id])?;
    tx.commit()?;
    Ok(Json(LoginResult { token:Some(token) }))
}

/// Deliver six random digits, limiting sends across all devices for the same email.
pub async fn email(State(app): State<App>, Json(input): Json<EmailLogin>) -> ApiResult<Json<EmailChallenge>> {
    require_login(&app,&input.device_id,&input.poll_secret)?;
    let email = input.email.trim().to_lowercase();
    if email.len() > 254 || !email.contains('@') || email.chars().any(char::is_whitespace) { return Err(ApiError::bad("Enter a valid email")); }
    let recipient=email.parse::<lettre::Address>().map_err(|_|ApiError::bad("Enter a valid email"))?;
    // Rejection sampling keeps each digit uniform; these UUID bytes come from the OS RNG.
    let mut code=String::new();
    while code.len()<6 {
        for byte in &uuid::Uuid::new_v4().as_bytes()[..6] {
            if *byte<250 && code.len()<6 { code.push(char::from(b'0'+byte%10)); }
        }
    }
    let expires_at;
    {
        let mut conn = app.db.0.lock().unwrap();
        let tx = conn.transaction()?;
        let recent: i64 = tx.query_row("SELECT COUNT(*) FROM devices WHERE email=?1 AND sent_at>?2",params![email,now()-60],|r|r.get(0))?;
        if recent > 0 { return Err(ApiError::busy("Wait one minute before requesting another code")); }
        let changed = tx.execute("UPDATE devices SET email=?1,code_hash=?2,sent_at=?3 WHERE id=?4 AND user_id IS NULL AND attempts<5 AND expires_at>?3",params![email,hash(&format!("{}:{code}",input.device_id)),now(),input.device_id])?;
        if changed != 1 { return Err(ApiError::bad("Start a new sign-in")); }
        expires_at=tx.query_row("SELECT expires_at FROM devices WHERE id=?1",[&input.device_id],|r|r.get(0))?;
        tx.commit()?;
    }
    if !app.config.dev { crate::mail::send_code(&app,recipient,&code).await?; }
    Ok(Json(EmailChallenge { expires_at,development_code:app.config.dev.then_some(code) }))
}

/// Consume verification attempts before comparison and issue the session inside the client flow.
pub async fn verify(State(app): State<App>, Json(input): Json<VerifyEmail>) -> ApiResult<Json<LoginResult>> {
    require_login(&app,&input.device_id,&input.poll_secret)?;
    let code:String=input.code.chars().filter(|c|!c.is_whitespace()).collect();
    let email = {
        let mut conn = app.db.0.lock().unwrap();
        let tx = conn.transaction()?;
        let row: Option<(String,String)> = tx.query_row("SELECT email,code_hash FROM devices WHERE id=?1 AND expires_at>?2 AND attempts<5 AND user_id IS NULL AND code_hash IS NOT NULL",params![input.device_id,now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((email,expected)) = row else { return Err(ApiError::bad("Code expired; request a new code")); };
        tx.execute("UPDATE devices SET attempts=attempts+1 WHERE id=?1",[&input.device_id])?;
        tx.commit()?;
        if code.len()!=6 || !code.bytes().all(|c|c.is_ascii_digit()) || expected != hash(&format!("{}:{code}",input.device_id)) { return Err(ApiError::bad("Incorrect sign-in code")); }
        email
    };
    let user = app.db.user(&email,None,app.config.free_credits)?;
    approve(&app,&input.device_id,user)?;
    poll(State(app),Json(PollLogin { device_id:input.device_id,poll_secret:input.poll_secret })).await
}

/// Email operations require the initiating client's secret as well as its device ID.
fn require_login(app:&App,id:&str,secret:&str)->ApiResult<()> {
    require_device(app,id)?;
    let valid:bool=app.db.0.lock().unwrap().query_row("SELECT poll_hash=?1 FROM devices WHERE id=?2",params![hash(secret),id],|r|r.get(0))?;
    if !valid { return Err(ApiError::bad("Invalid sign-in")); }
    Ok(())
}

/// Bind Google OAuth state to this browser and protect the authorization code with PKCE.
pub async fn google(State(app): State<App>, Path(device): Path<String>) -> ApiResult<Response> {
    require_device(&app,&device)?;
    let client = app.config.google_client_id.as_ref().ok_or_else(||ApiError::bad("Google sign-in is not configured"))?;
    let state = secret();
    let verifier = secret();
    app.db.0.lock().unwrap().execute("INSERT INTO oauth(state_hash,device_id,verifier,expires_at) VALUES(?1,?2,?3,?4)",params![hash(&state),device,verifier,now()+600])?;
    let redirect = format!("{}/auth/google/callback",app.config.public_url);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
    url.query_pairs_mut().extend_pairs([("client_id",client.as_str()),("redirect_uri",&redirect),("response_type","code"),
        ("scope","openid email profile"),("state",&state),("code_challenge",&challenge),("code_challenge_method","S256"),("prompt","select_account")]);
    let mut response = Redirect::to(url.as_str()).into_response();
    let secure = if app.config.dev { "" } else { "; Secure" };
    response.headers_mut().insert(header::SET_COOKIE,format!("assistant_oauth={state}; HttpOnly; SameSite=Lax; Path=/auth/google; Max-Age=600{secure}").parse()?);
    Ok(response)
}

#[derive(Deserialize)]
pub struct Callback { state: String, code: String }

/// Exchange Google's code server-side and use Google's verified, stable subject identity.
pub async fn callback(State(app): State<App>, headers: HeaderMap, Query(input): Query<Callback>) -> ApiResult<Html<String>> {
    let cookie = headers.get(header::COOKIE).and_then(|v|v.to_str().ok()).unwrap_or("");
    let expected = format!("assistant_oauth={}",input.state);
    if !cookie.split(';').any(|part|part.trim() == expected) { return Err(ApiError::bad("OAuth browser state did not match")); }
    let (device,verifier): (String,String) = {
        let mut conn = app.db.0.lock().unwrap();
        let tx = conn.transaction()?;
        let row = tx.query_row("SELECT device_id,verifier FROM oauth WHERE state_hash=?1 AND expires_at>?2",params![hash(&input.state),now()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        tx.execute("DELETE FROM oauth WHERE state_hash=?1",[hash(&input.state)])?;
        tx.commit()?;
        row
    };
    require_device(&app,&device)?;
    let token: Value = app.http.post("https://oauth2.googleapis.com/token").form(&[
        ("client_id",app.config.google_client_id.as_ref().unwrap().as_str()),("client_secret",app.config.google_client_secret.as_ref().unwrap()),
        ("redirect_uri",&format!("{}/auth/google/callback",app.config.public_url)),("code",&input.code),("code_verifier",&verifier),("grant_type","authorization_code")
    ]).send().await?.error_for_status()?.json().await?;
    let access = token["access_token"].as_str().ok_or_else(||ApiError::bad("Google did not issue a token"))?;
    let profile: Value = app.http.get("https://openidconnect.googleapis.com/v1/userinfo").bearer_auth(access).send().await?.error_for_status()?.json().await?;
    if profile["email_verified"] != true { return Err(ApiError::bad("Google email is not verified")); }
    let sub = profile["sub"].as_str().ok_or_else(||ApiError::bad("Google subject missing"))?;
    let email = profile["email"].as_str().ok_or_else(||ApiError::bad("Google email missing"))?.to_lowercase();
    let user = app.db.user(&email,Some(sub),app.config.free_credits)?;
    approve(&app,&device,user)?;
    Ok(Html(document("<h1>Signed in with Google</h1><p>Return to Linux Assistant. You can close this tab.</p>")))
}

/// Validate unpredictable device identifiers before reflecting them into HTML.
fn require_device(app: &App, id: &str) -> ApiResult<()> {
    if id.len()!=64 || !id.bytes().all(|b|b.is_ascii_hexdigit()) { return Err(ApiError::bad("Invalid sign-in")); }
    let exists: bool = app.db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE id=?1 AND expires_at>?2 AND user_id IS NULL)",params![id,now()],|r|r.get(0))?;
    if !exists { return Err(ApiError::bad("Sign-in expired or already approved")); }
    Ok(())
}

/// Approve the device without handing an application session to the browser.
fn approve(app: &App, device: &str, user: i64) -> ApiResult<()> {
    let changed = app.db.0.lock().unwrap().execute("UPDATE devices SET user_id=?1,code_hash=NULL WHERE id=?2 AND expires_at>?3 AND user_id IS NULL",params![user,device,now()])?;
    if changed != 1 { return Err(ApiError::bad("Sign-in expired or already approved")); }
    Ok(())
}

/// These pages contain only server-generated markup and validated opaque identifiers.
pub fn document(body: &str) -> String {
    format!("<!doctype html><html lang='en'><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>Linux Assistant</title><body><main>{body}</main></body></html>")
}
