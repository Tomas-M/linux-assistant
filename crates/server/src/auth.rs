use axum::{extract::State, Json};
pub mod social;
use protocol::{AuthProvider, DeviceLogin, EmailChallenge, EmailLogin, EmailVerification, LoginResult, PollLogin, VerifyEmail};
use rusqlite::{params, OptionalExtension};
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
    let Some(user) = user else {
        let social:Option<(String,Option<String>,Option<String>,Option<String>)>=tx.query_row("SELECT s.provider,s.subject,s.error,d.email FROM social_signins s JOIN devices d ON d.id=s.device_id WHERE s.device_id=?1",[&input.device_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let mut email_verification=None;
        if let Some((provider,subject,error,email))=social {
            if let Some(error)=error { return Err(ApiError::bad(&error)); }
            if subject.is_some() { email_verification=Some(EmailVerification { provider:serde_json::from_value(serde_json::json!(provider))?,email }); }
        }
        return Ok(Json(LoginResult { token:None,email_verification }));
    };
    let token = secret();
    tx.execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",params![hash(&token),user,now()+30*86400])?;
    tx.execute("DELETE FROM oauth WHERE device_id=?1",[&input.device_id])?;
    tx.execute("DELETE FROM devices WHERE id=?1",[&input.device_id])?;
    tx.commit()?;
    Ok(Json(LoginResult { token:Some(token),email_verification:None }))
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
    let identity:Option<(String,String)>=app.db.0.lock().unwrap().query_row("SELECT provider,subject FROM social_signins WHERE device_id=?1 AND subject IS NOT NULL",[&input.device_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let user=if let Some((provider,subject))=identity {
        let provider:AuthProvider=serde_json::from_value(serde_json::json!(provider))?;
        app.db.social_user(&email,provider,&subject,app.config.free_credits)?
    } else { app.db.user(&email,None,app.config.free_credits)? };
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
