use axum::{extract::{Path,Query,State}, http::{header,HeaderMap}, response::{Html,IntoResponse,Redirect,Response}, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD,Engine};
use protocol::{AuthProvider,PollLogin};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use crate::{db::{hash,now,secret},App,ApiError,ApiResult};
#[cfg(test)]
mod tests;

/// Advertise only providers with complete, operator-managed credentials.
pub async fn providers(State(app):State<App>)->Json<Value> {
    let config=app.config();
    Json(json!({"email_enabled":config.email_enabled,"providers":([AuthProvider::Google,AuthProvider::Microsoft,AuthProvider::Github].into_iter().filter(|provider|credentials(&config,*provider).is_some()).collect::<Vec<_>>())}))
}

/// Keep client secrets on the backend, including during device polling.
fn credentials(config:&crate::config::Config,provider:AuthProvider)->Option<(&str,&str)> {
    let (id,secret)=match provider {
        AuthProvider::Google if config.google_enabled=>(&config.google_client_id,&config.google_client_secret),
        AuthProvider::Microsoft if config.microsoft_enabled=>(&config.microsoft_client_id,&config.microsoft_client_secret),
        AuthProvider::Github if config.github_enabled=>(&config.github_client_id,&config.github_client_secret),
        _=>return None,
    };
    Some((id.as_deref()?,secret.as_deref()?))
}

/// Cancel only the device owned by this native client; stale browser tabs cannot cancel newer attempts.
pub async fn cancel(State(app):State<App>,Json(input):Json<PollLogin>)->ApiResult<Json<Value>> {
    let mut conn=app.db.0.lock().unwrap();
    let tx=conn.transaction()?;
    tx.execute("DELETE FROM oauth WHERE device_id IN (SELECT id FROM devices WHERE id=?1 AND poll_hash=?2)",params![input.device_id,hash(&input.poll_secret)])?;
    tx.execute("DELETE FROM devices WHERE id=?1 AND poll_hash=?2",params![input.device_id,hash(&input.poll_secret)])?;
    tx.commit()?;
    Ok(Json(json!({"ok":true})))
}

/// Bind the browser, provider and authorization code to the initiating device with state and PKCE.
pub async fn begin(State(app):State<App>,Path((provider,device)):Path<(AuthProvider,String)>)->ApiResult<Response> {
    let config=app.config();
    super::require_device(&app,&device)?;
    let (client,_)=credentials(&config,provider).ok_or_else(||ApiError::bad("This sign-in provider is not configured"))?;
    let state=secret();
    let verifier=secret();
    {
        let mut conn=app.db.0.lock().unwrap();
        let tx=conn.transaction()?;
        tx.execute("DELETE FROM oauth WHERE device_id=?1",[&device])?;
        tx.execute("INSERT OR REPLACE INTO social_signins(device_id,provider) VALUES(?1,?2)",params![device,provider.key()])?;
        tx.execute("INSERT INTO oauth(state_hash,device_id,verifier,expires_at) VALUES(?1,?2,?3,?4)",params![hash(&format!("{}:{state}",provider.key())),device,verifier,now()+600])?;
        tx.commit()?;
    }
    let endpoint=match provider {
        AuthProvider::Google=>"https://accounts.google.com/o/oauth2/v2/auth",
        AuthProvider::Microsoft=>"https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        AuthProvider::Github=>"https://github.com/login/oauth/authorize",
    };
    let mut url=reqwest::Url::parse(endpoint)?;
    url.query_pairs_mut().extend_pairs([("client_id",client),("redirect_uri",&format!("{}/auth/{}/callback",config.public_url,provider.key())),
        ("response_type","code"),("scope",if provider==AuthProvider::Github { "user:email" } else { "openid email profile" }),
        ("state",&state),("code_challenge",&URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))),("code_challenge_method","S256")]);
    if provider!=AuthProvider::Github { url.query_pairs_mut().append_pair("prompt","select_account"); }
    let mut response=Redirect::to(url.as_str()).into_response();
    let secure=if config.dev { "" } else { "; Secure" };
    response.headers_mut().insert(header::SET_COOKIE,format!("assistant_oauth={state}; HttpOnly; SameSite=Lax; Path=/auth/{}; Max-Age=600{secure}",provider.key()).parse()?);
    Ok(response)
}

#[derive(Deserialize)]
pub struct Callback { state:String,code:Option<String>,error:Option<String> }

/// Consume authorization state once; report failures through the client's private poll channel too.
pub async fn callback(State(app):State<App>,Path(provider):Path<AuthProvider>,headers:HeaderMap,Query(input):Query<Callback>)->ApiResult<Html<String>> {
    let cookie=headers.get(header::COOKIE).and_then(|value|value.to_str().ok()).unwrap_or("");
    if !cookie.split(';').any(|part|part.trim()==format!("assistant_oauth={}",input.state)) { return Err(ApiError::bad("OAuth browser state did not match")); }
    let (device,verifier):(String,String)={
        let mut conn=app.db.0.lock().unwrap();
        let tx=conn.transaction()?;
        let state_hash=hash(&format!("{}:{}",provider.key(),input.state));
        let row=tx.query_row("SELECT o.device_id,o.verifier FROM oauth o JOIN social_signins s ON s.device_id=o.device_id WHERE o.state_hash=?1 AND o.expires_at>?2 AND s.provider=?3",params![state_hash,now(),provider.key()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|ApiError::bad("Sign-in expired or already consumed"))?;
        tx.execute("DELETE FROM oauth WHERE state_hash=?1",[state_hash])?;
        tx.commit()?;
        row
    };
    super::require_device(&app,&device)?;
    let result=if input.error.is_some() { Err(anyhow::anyhow!("Sign-in was declined. Try again or use email.")) }
        else { exchange(&app,provider,input.code.as_deref().unwrap_or(""),&verifier).await };
    let message=match result.and_then(|identity|complete(&app,&device,provider,identity)) {
        Ok(true)=>"Signed in. Return to the assistant; you can close this window.",
        Ok(false)=>"Confirm your email in the assistant to finish signing in. You can close this window.",
        Err(error)=>{
            crate::diagnostics::record("error","social_sign_in",None,None,&format!("{} sign-in failed: {error:#}",provider.key()));
            let message=if input.error.is_some() { "Sign-in was declined. Try again or use email." } else { "Provider sign-in failed. Try again or use email." };
            app.db.0.lock().unwrap().execute("UPDATE social_signins SET error=?1 WHERE device_id=?2",params![message,device])?;
            message
        }
    };
    Ok(Html(super::document(&format!("<h1>Sign-in</h1><p>{message}</p>"))))
}

struct Identity { subject:String,email:Option<String>,trusted_email:bool }

/// Fetch identities from fixed provider endpoints; access tokens are discarded after this exchange.
async fn exchange(app:&App,provider:AuthProvider,code:&str,verifier:&str)->anyhow::Result<Identity> {
    let config=app.config();
    anyhow::ensure!(!code.is_empty(),"Provider did not return an authorization code");
    let (client,secret)=credentials(&config,provider).ok_or_else(||anyhow::anyhow!("Sign-in provider is not configured"))?;
    let token_url=match provider {
        AuthProvider::Google=>"https://oauth2.googleapis.com/token",
        AuthProvider::Microsoft=>"https://login.microsoftonline.com/common/oauth2/v2.0/token",
        AuthProvider::Github=>"https://github.com/login/oauth/access_token",
    };
    let token:Value=app.http.post(token_url).header(header::ACCEPT,"application/json").form(&[("client_id",client),("client_secret",secret),
        ("redirect_uri",&format!("{}/auth/{}/callback",config.public_url,provider.key())),("code",code),("code_verifier",verifier),("grant_type","authorization_code")])
        .send().await?.error_for_status()?.json().await?;
    let access=token["access_token"].as_str().ok_or_else(||anyhow::anyhow!("Provider did not issue an access token"))?;
    let profile_url=match provider {
        AuthProvider::Google=>"https://openidconnect.googleapis.com/v1/userinfo",
        AuthProvider::Microsoft=>"https://graph.microsoft.com/oidc/userinfo",
        AuthProvider::Github=>"https://api.github.com/user",
    };
    let profile:Value=app.http.get(profile_url).bearer_auth(access).header(header::USER_AGENT,"Linux-Assistant").send().await?.error_for_status()?.json().await?;
    let emails=if provider==AuthProvider::Github {
        app.http.get("https://api.github.com/user/emails").bearer_auth(access).header(header::USER_AGENT,"Linux-Assistant").send().await?.error_for_status()?.json().await?
    } else { Value::Null };
    identity(provider,&profile,&emails)
}

/// Microsoft email claims and non-authoritative Google addresses need our own ownership check.
fn identity(provider:AuthProvider,profile:&Value,emails:&Value)->anyhow::Result<Identity> {
    let subject=if provider==AuthProvider::Github { profile["id"].as_u64().map(|id|id.to_string()) } else { profile["sub"].as_str().map(str::to_owned) };
    let subject=subject.filter(|sub|!sub.is_empty()).ok_or_else(||anyhow::anyhow!("Provider did not return a stable subject"))?;
    let address=if provider==AuthProvider::Github {
        emails.as_array().and_then(|list|list.iter().filter(|email|email["verified"]==true).max_by_key(|email|email["primary"]==true)).and_then(|email|email["email"].as_str())
    } else { profile["email"].as_str() };
    let email=address.map(|email|email.trim().to_lowercase()).filter(|email|email.len()<=254 && email.parse::<lettre::Address>().is_ok());
    let trusted_email=match provider {
        AuthProvider::Google=>profile["email_verified"]==true && (email.as_ref().is_some_and(|email|email.ends_with("@gmail.com")) || profile["hd"].as_str().is_some_and(|domain|!domain.is_empty())),
        AuthProvider::Github=>email.is_some(),
        AuthProvider::Microsoft=>false,
    };
    Ok(Identity { subject,email,trusted_email })
}

/// Link verified identities once; subsequent sign-ins use the stable subject without another email code.
fn complete(app:&App,device:&str,provider:AuthProvider,identity:Identity)->anyhow::Result<bool> {
    let config=app.config();
    let known=app.db.identity_user(provider,&identity.subject)?;
    let user=if known.is_some() { known } else if identity.trusted_email {
        identity.email.as_deref().map(|email|app.db.social_user(email,provider,&identity.subject,config.free_credits)).transpose()?
    } else { None };
    if let Some(user)=user {
        super::approve(app,device,user).map_err(|error|anyhow::anyhow!("{}",error.1))?;
        return Ok(true);
    }
    let mut conn=app.db.0.lock().unwrap();
    let tx=conn.transaction()?;
    tx.execute("UPDATE social_signins SET subject=?1 WHERE device_id=?2 AND provider=?3",params![identity.subject,device,provider.key()])?;
    tx.execute("UPDATE devices SET email=?1 WHERE id=?2",params![identity.email,device])?;
    tx.commit()?;
    Ok(false)
}
