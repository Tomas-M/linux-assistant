use std::{future::IntoFuture, sync::Arc, time::Duration};
use anyhow::{bail, Context, Result};
use axum::{body::Body, extract::{DefaultBodyLimit, State}, http::{header, Request, StatusCode}, middleware::{self, Next}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use crate::{engine::Engine, processes::Processes};

#[derive(Clone, Serialize, Deserialize)]
pub struct Connection { pub url: String }
#[derive(Clone)]
pub(crate) struct Local { account:Arc<std::sync::RwLock<Option<protocol::Account>>>, downloads:Arc<crate::downloads::Downloads>, pub(crate) brand:Arc<crate::config::Brand>,pub(crate) ui:Arc<crate::ui::Store>,pub(crate) auth:crate::session::Credentials,media:crate::media::Media, controls:Arc<crate::controls::Controls>, pub(crate) live: crate::stream::Live, full_access: Arc<std::sync::atomic::AtomicBool>, engine: Arc<Mutex<Engine>>, processes: Processes, connection: Connection, stop: Arc<Notify> }
struct LocalError(anyhow::Error);
type LocalResult = std::result::Result<Json<Value>,LocalError>;

impl<E: Into<anyhow::Error>> From<E> for LocalError {
    /// Surface local errors to the user without logging session-bearing request bodies.
    fn from(value: E) -> Self { Self(value.into()) }
}
impl IntoResponse for LocalError {
    /// Both frontends consume the same error envelope.
    fn into_response(self) -> Response {
        let expired=self.0.downcast_ref::<crate::backend::SessionExpired>().cloned();
        let status=if expired.is_some() { StatusCode::UNAUTHORIZED } else { StatusCode::BAD_REQUEST };
        let mut response=(status,Json(json!({"error":self.0.to_string()}))).into_response();
        if let Some(expired)=expired { response.extensions_mut().insert(expired); }
        response
    }
}

impl Connection {
    /// Send requests only to the exact loopback origin.
    #[cfg(test)]
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::POST,path,Some(body)).await
    }
    /// Read shared daemon state from terminal clients.
    pub async fn get(&self, path: &str) -> Result<Value> { self.request(reqwest::Method::GET,path,None).await }
    /// Refuse discovery files that point outside the local computer.
    async fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = reqwest::Url::parse(&self.url)?;
        anyhow::ensure!(url.scheme()=="http" && url.host_str()==Some("127.0.0.1") && url.username().is_empty() && url.password().is_none(),"Invalid local daemon address");
        let mut request = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build()?
            .request(method,format!("{}{path}",self.url));
        // Approved commands can run indefinitely while the terminal waits for their result.
        if !matches!(path,"/api/chat"|"/api/retry") { request=request.timeout(Duration::from_secs(if path=="/api/health" { 1 } else { 1800 })); }
        if let Some(body) = body { request = request.json(&body); }
        let response = request.send().await?;
        let status = response.status();
        let body: Value = response.json().await?;
        anyhow::ensure!(status.is_success(),"{}",body["error"].as_str().unwrap_or("Local request failed"));
        Ok(body)
    }
}

/// Find the live local service through its socket, without any discovery files.
pub async fn connect(args:&crate::Args) -> Result<Connection> {
    let connection=Connection { url:"http://127.0.0.1:56592".into() };
    if let Ok(health)=connection.get("/api/health").await {
        anyhow::ensure!(health["application"]=="linux-assistant" && health["bridge"]==protocol::BRIDGE_API,"Port 56592 belongs to another application or client version");
        anyhow::ensure!(health["backend"]==args.backend,"A client is already running for a different backend; stop it before changing servers");
        return Ok(connection);
    }
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let mut command=std::process::Command::new(std::env::current_exe()?);
        command.args(args.daemon_args()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        unsafe { command.pre_exec(|| { if libc::setsid()==-1 { return Err(std::io::Error::last_os_error()); } Ok(()) }); }
        command.spawn()?;
    }
    #[cfg(windows)]
    crate::windows_launcher::spawn(&args.daemon_args())?;
    for _ in 0..1800 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Ok(health)=connection.get("/api/health").await {
            anyhow::ensure!(health["application"]=="linux-assistant" && health["bridge"]==protocol::BRIDGE_API && health["backend"]==args.backend,"Another local application is using port 56592");
            return Ok(connection);
        }
    }
    bail!("Local assistant did not start. Run with --daemon to see the startup error")
}

/// Hold the loopback port for this process; all session data and downloaded UI stay in RAM.
pub async fn serve(backend:&str) -> Result<()> {
    let listener=tokio::net::TcpListener::bind(("127.0.0.1",56592)).await?;
    let connection=Connection { url:format!("http://{}",listener.local_addr()?) };
    let engine=Engine::open(backend).await?;
    let brand=Arc::new(crate::config::Brand::default());
    let ui=Arc::new(crate::ui::Store::default());
    // Complete startup downloads before the launcher can obtain a ready health response and print its URL.
    crate::config::load(backend.into(),brand.clone(),engine.live.clone()).await;
    if let Err(error)=ui.load(backend,&engine.live).await { engine.live.0.send_modify(|view|view["backend_error"]=json!(error.to_string())); }
    let processes=engine.processes.clone();
    let state=Local { account:engine.account.clone(),downloads:Arc::new(crate::downloads::Downloads::default()),brand,ui,auth:engine.auth.clone(),media:engine.media.clone(),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),engine:Arc::new(Mutex::new(engine)),processes:processes.clone(),connection,stop:Arc::new(Notify::new()) };
    let stop=state.stop.clone();
    let controls=state.controls.clone();
    let signal=async {
        #[cfg(unix)] {
            let mut terminate=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! { _=terminate.recv()=>{},_=tokio::signal::ctrl_c()=>{} }
        }
        #[cfg(not(unix))]
        { let _=tokio::signal::ctrl_c().await; }
    };
    tokio::select! {
        result=axum::serve(listener,router(state)).into_future()=>result?,
        _=async { tokio::select! { _=stop.notified()=>{},_=signal=>{} } }=> {
            controls.cancel_local();
            processes.stop_foreground(None);
            let _=tokio::time::timeout(Duration::from_secs(3),controls.interrupt()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}

/// The backend owns every browser asset; the native server exposes the stable execution bridge.
fn router(state: Local) -> Router {
    Router::new()
        .route("/",get(crate::ui::index)).route("/ui/{version}/{*path}",get(crate::ui::asset))
        .route("/branding/{key}",get(brand_image))
        .route("/api/configuration/retry",post(retry_configuration))
        .route("/api/bootstrap",get(|State(state):State<Local>|async move { Json(json!({"bridge":protocol::Bridge::current(),"client_version":env!("CARGO_PKG_VERSION"),"brand":state.brand.metadata(),"providers":state.brand.providers()})) }))
        .route("/api/branding",get(|State(state):State<Local>|async move { Json(state.brand.metadata()) }))
        .route("/api/media/{scope}/{index}",get(|State(state):State<Local>,axum::extract::Path((scope,index)):axum::extract::Path<(String,usize)>|async move { media_response(state,scope,index,false).await }))
        .route("/api/media/{scope}/{index}/thumbnail",get(|State(state):State<Local>,axum::extract::Path((scope,index)):axum::extract::Path<(String,usize)>|async move { media_response(state,scope,index,true).await }))
        .route("/api/health",get(|State(state):State<Local>|async move { Json(json!({"ok":true,"application":"linux-assistant","bridge":protocol::BRIDGE_API,"backend":state.auth.backend})) }))
        .route("/api/files",post(resolve_files)).route("/api/download/{id}",get(download_file))
        .route("/api/events",get(events)).route("/api/access",post(access))
        .route("/api/state",get(state_view)).route("/api/account",post(account))
        .route("/api/settings",post(settings))
        .route("/api/login",post(login)).route("/api/login/verify",post(verify_login)).route("/api/logout",post(logout))
        .route("/api/login/providers",get(auth_providers)).route("/api/login/social",post(social_start))
        .route("/api/login/poll",post(social_poll)).route("/api/login/cancel",post(social_cancel))
        .route("/api/chat",post(chat).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES))).route("/api/steer",post(steer).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES))).route("/api/interrupt",post(interrupt)).route("/api/queue/{id}/remove",post(remove_queued)).route("/api/retry",post(retry)).route("/api/new",post(new_chat))
        .route("/api/queue/{id}/edit",post(edit_queued)).route("/api/queue/{id}/steer",post(steer_queued))
        .route("/api/memories",get(memories).post(save_memories)).route("/api/respond",post(respond)).route("/api/credit-packs",get(credit_packs)).route("/api/checkout",post(checkout)).route("/api/purchases",get(purchases)).route("/api/purchases/{id}",get(purchase_status)).route("/api/stop",post(stop))
        .route("/api/processes",get(process_list)).route("/api/processes/{id}",get(process_detail))
        .route("/api/processes/{id}/stop",post(process_stop))
        .layer(DefaultBodyLimit::max(128*1024)).layer(middleware::from_fn_with_state(state.clone(),security)).with_state(state)
}

/// Serve only previously captured images without waiting for the conversation engine lock.
async fn media_response(state:Local,scope:String,index:usize,thumbnail:bool)->Response {
    match tokio::task::spawn_blocking(move||state.media.read(&scope,index,thumbnail)).await {
        Ok(Ok((mime,bytes)))=>([(header::CONTENT_TYPE,mime)],bytes).into_response(),
        _=>StatusCode::NOT_FOUND.into_response(),
    }
}

/// Resolve paths independently of a running AI turn, using the native client's filesystem permissions.
async fn resolve_files(State(state):State<Local>,Json(paths):Json<Vec<String>>)->LocalResult {
    let home=dirs::home_dir().context("Home directory not found")?;
    Ok(Json(tokio::task::spawn_blocking(move||state.downloads.resolve(paths,&home)).await??))
}

/// File IDs reveal no paths and use the same loopback origin checks as every local API route.
async fn download_file(State(state):State<Local>,axum::extract::Path(id):axum::extract::Path<Uuid>)->Response {
    crate::downloads::response(state.downloads,id).await
}

/// Proxy only manifest-owned assets through loopback, including when browser and assistant use different OS users.
async fn brand_image(State(state):State<Local>,axum::extract::Path(key):axum::extract::Path<String>,axum::extract::Query(query):axum::extract::Query<std::collections::HashMap<String,String>>)->Result<Response,LocalError> {
    let manifest=state.brand.metadata();
    let Some(hash)=manifest["images"][&key].as_str() else { return Ok(StatusCode::NOT_FOUND.into_response()); };
    if query.get("v").map(String::as_str)!=Some(hash) { return Ok(StatusCode::NOT_FOUND.into_response()); }
    let backend=state.live.0.borrow()["state"]["settings"]["backend"].as_str().unwrap().to_owned();
    let mut response=crate::backend::send(crate::backend::client(60)?.get(format!("{backend}/v1/client-assets/{key}?v={hash}")),&state.live,None).await?;
    if !response.status().is_success() { return Err(anyhow::anyhow!("Cannot load the configured image").into()); }
    let mime=response.headers().get(header::CONTENT_TYPE).context("Image content type is missing")?.to_str()?.to_owned();
    let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await? { if bytes.len()+chunk.len()>protocol::MAX_BRAND_IMAGE_BYTES { return Err(anyhow::anyhow!("Brand image exceeds 2 MiB").into()); } bytes.extend_from_slice(&chunk); }
    Ok(([(header::CONTENT_TYPE,mime.as_str()),(header::CACHE_CONTROL,"no-store")],bytes).into_response())
}

/// An explicit reconnect retries an unsuccessful startup without reloading settings already in use.
async fn retry_configuration(State(state):State<Local>)->Json<Value> {
    let backend=state.live.0.borrow()["state"]["settings"]["backend"].as_str().unwrap().to_owned();
    crate::config::load(backend,state.brand.clone(),state.live).await;
    Json(json!({"providers":state.brand.providers()}))
}

/// Keep the local API accessible to local users while rejecting unrelated websites and DNS rebinding.
async fn security(State(state): State<Local>, request: Request<Body>, next: Next) -> Response {
    let host = state.connection.url.strip_prefix("http://").unwrap();
    if request.headers().get(header::HOST).and_then(|v|v.to_str().ok()) != Some(host) { return StatusCode::FORBIDDEN.into_response(); }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if origin.to_str().ok()!=Some(state.connection.url.as_str()) { return StatusCode::FORBIDDEN.into_response(); }
    }
    if request.uri().path().starts_with("/api/") && request.headers().get("sec-fetch-site").is_some_and(|site|site!="same-origin" && site!="none") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    // Handle expired sessions for every browser API, including controls that bypass the engine lock.
    if let Some(expired)=response.extensions_mut().remove::<crate::backend::SessionExpired>() {
        if let Err(error)=crate::session::expire(&state.engine,&state.controls,&state.auth,&expired).await { response=LocalError(error).into_response(); }
    }
    for (key,value) in [("cache-control","no-store"),("referrer-policy","no-referrer"),("x-content-type-options","nosniff"),
        ("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")] {
        // Inline file documents provide their own sandbox policy.
        if matches!(key,"content-security-policy"|"cache-control") && response.headers().contains_key(key) { continue; }
        response.headers_mut().insert(axum::http::HeaderName::from_static(key),value.parse().unwrap());
    }
    response
}

#[derive(Deserialize)]
struct SettingsInput { commands_enabled: bool }
#[derive(Deserialize)]
struct EmailInput { email: String }
#[derive(Deserialize)]
struct CodeInput { code: String }
#[derive(Deserialize)]
struct SocialInput { provider:protocol::AuthProvider }
#[derive(Deserialize)]
struct LoginId { id:String }
#[derive(Deserialize)]
struct ChatInput { text: String, cwd: Option<String>, id: Option<Uuid>, attachments:Option<Vec<protocol::Attachment>> }

/// Read the same assistant state in either interface.
async fn state_view(State(state): State<Local>) -> Json<Value> { Json(state.live.0.borrow()["state"].clone()) }

/// Public sign-in discovery never locks the conversation or blocks account refreshes.
async fn auth_providers(State(state):State<Local>)->LocalResult {
    Ok(Json(state.brand.providers()))
}
/// Create a native-owned device challenge before navigating the provider popup.
async fn social_start(State(state):State<Local>,Json(input):Json<SocialInput>)->LocalResult { Ok(Json(state.engine.try_lock().context("Assistant is busy")?.social_start(input.provider).await?)) }
/// Exchange an approved challenge without exposing its secret or session token.
async fn social_poll(State(state):State<Local>,Json(input):Json<LoginId>)->LocalResult { Ok(Json(state.engine.try_lock().context("Assistant is busy")?.social_poll(input.id).await?)) }
/// A cancellation affects only the matching challenge, including when multiple tabs are open.
async fn social_cancel(State(state):State<Local>,Json(input):Json<LoginId>)->LocalResult { state.engine.try_lock().context("Assistant is busy")?.social_cancel(input.id).await?; Ok(Json(json!({"ok":true}))) }
/// Reconnect without waiting for the engine or restarting its current operation.
async fn events(State(state): State<Local>) -> Response { state.live.response() }
/// Revoking automatic execution is immediate, even during a long command or provider request.
async fn access(State(state): State<Local>, Json(input): Json<Value>) -> LocalResult {
    let enabled=input["enabled"].as_bool().context("Expected an execution permission")?;
    if enabled {
        let view=state.live.0.borrow();
        if view["state"]["authenticated"]!=true || view["state"]["settings"]["commands_enabled"]!=true { return Err(anyhow::anyhow!("Enable local access first").into()); }
    }
    state.full_access.store(enabled,std::sync::atomic::Ordering::SeqCst);
    state.live.0.send_modify(|view|view["state"]["full_access"]=json!(enabled));
    if enabled { state.controls.approve_waiting(); }
    Ok(Json(state.live.0.borrow()["state"].clone()))
}
/// Publish execution permission shared by the browser and terminal.
async fn settings(State(state): State<Local>, Json(input): Json<SettingsInput>) -> LocalResult {
    let mut engine = state.engine.try_lock().context("Assistant is busy")?;
    engine.settings(input.commands_enabled)?;
    Ok(Json(engine.state()))
}
/// Refresh authoritative credits even while generation owns the engine lock.
async fn account(State(state): State<Local>) -> LocalResult {
    if let Ok(mut engine)=state.engine.try_lock() { return Ok(Json(engine.refresh_account().await?)); }
    let account=crate::session::account(&state.auth,&state.live,reqwest::Method::GET,"/v1/me",None).await?;
    let updated:protocol::Account=serde_json::from_value(account.clone())?;
    let mut current=state.account.write().unwrap();
    // A delayed response must not replace an account that signed out or changed during the request.
    if current.as_ref().is_some_and(|current|current.email==updated.email) {
        *current=Some(updated);
        state.live.0.send_modify(|view|view["state"]["account"]=account);
    }
    drop(current);
    Ok(Json(state.live.0.borrow()["state"].clone()))
}
/// Send a code while keeping login input inside the client.
async fn login(State(state): State<Local>,Json(input):Json<EmailInput>) -> LocalResult { Ok(Json(serde_json::to_value(state.engine.try_lock().context("Assistant is busy")?.login(input.email).await?)?)) }
/// Exchange the code without exposing session credentials to browser JavaScript.
async fn verify_login(State(state): State<Local>,Json(input):Json<CodeInput>) -> LocalResult { Ok(Json(state.engine.try_lock().context("Assistant is busy")?.verify_login(input.code).await?)) }
/// Revoke this session and clear its local conversation.
async fn logout(State(state): State<Local>) -> LocalResult {
    let mut engine = state.engine.try_lock().context("Assistant is busy")?;
    engine.logout().await?;
    Ok(Json(engine.state()))
}
/// Accept only text and a working directory, never client-supplied tool definitions.
async fn chat(State(state): State<Local>, Json(input): Json<ChatInput>) -> LocalResult {
    let cwd = match input.cwd { Some(path) => path,None => dirs::home_dir().context("Home directory not found")?.to_string_lossy().into_owned() };
    state.controls.enqueue(crate::controls::Queued { attachments:input.attachments, id:input.id.unwrap_or_else(Uuid::new_v4),text:input.text,cwd,editing:false }).await?;
    if state.live.0.borrow()["busy"]==true { return Ok(Json(state.live.0.borrow()["state"].clone())); }
    run(state,Operation::Next).await
}
/// Retry the stored idempotent request after a transport or provider failure.
async fn retry(State(state): State<Local>) -> LocalResult { run(state,Operation::Retry).await }
/// Reset conversation and invalidate all stale approvals.
async fn new_chat(State(state): State<Local>) -> LocalResult {
    let mut engine = state.engine.try_lock().context("Assistant is busy")?;
    engine.new_chat().await?;
    Ok(Json(engine.state()))
}
/// Answer a native question or approve its exact displayed local operation without locking the engine.
async fn respond(State(state):State<Local>,Json(input):Json<protocol::InteractionResponse>)->LocalResult {
    state.controls.respond(input)?;
    Ok(Json(json!({"ok":true})))
}
/// Steering bypasses the engine lock and uses the backend's active-turn protocol.
async fn steer(State(state):State<Local>,Json(input):Json<protocol::Steering>)->LocalResult {
    let result=state.controls.steer(&input).await?;
    record_steering(&state,&input);
    wake_queue(state);
    Ok(Json(result))
}
/// Show accepted live input from either the composer or queue without waiting for the turn to finish.
fn record_steering(state:&Local,input:&protocol::Steering) { crate::session::record_steering(&state.live,input); }
/// Stop the current task and its owned command, preserving the Full access preference.
async fn interrupt(State(state):State<Local>)->LocalResult {
    crate::session::interrupt(&state.controls,&state.live,&state.processes).await?;
    Ok(Json(json!({"ok":true})))
}
/// Removing queued input never sends it to the model.
async fn remove_queued(State(state):State<Local>,axum::extract::Path(id):axum::extract::Path<Uuid>)->LocalResult {
    state.controls.remove(id).await?;
    wake_queue(state);
    Ok(Json(json!({"ok":true})))
}
/// Editing reserves the original position and prevents that message from being dispatched.
async fn edit_queued(State(state):State<Local>,axum::extract::Path(id):axum::extract::Path<Uuid>,Json(input):Json<Value>)->LocalResult {
    let editing=input["editing"].as_bool().context("Expected an edit state")?;
    let input=state.controls.edit(id,editing).await?;
    if !editing { wake_queue(state); }
    Ok(Json(json!(input)))
}
/// Atomically steer a queued message so it cannot also be dispatched as the next turn.
async fn steer_queued(State(state):State<Local>,axum::extract::Path(id):axum::extract::Path<Uuid>)->LocalResult {
    let input=state.controls.steer_queued(id).await?;
    record_steering(&state,&input);
    wake_queue(state);
    Ok(Json(json!({"ok":true})))
}
/// Continue after a held queue entry is released, but respect explicit stops and pending approvals.
fn wake_queue(state:Local) {
    let view=state.live.0.borrow().clone();
    let waiting=view["busy"]==true || view["state"]["retry_available"]==true;
    if waiting || state.controls.stopped.load(std::sync::atomic::Ordering::SeqCst) { return; }
    tokio::spawn(async move { let _=run(state,Operation::Next).await; });
}
enum Operation { Next, Retry }

/// Own the task independently of browser connections; consume every approved proposal exactly once.
async fn run(state: Local, operation: Operation) -> LocalResult {
    let result=crate::session::run(state.engine,matches!(operation,Operation::Retry)).await?;
    Ok(Json(if result.is_null() { state.live.0.borrow()["state"].clone() } else { result }))
}

/// Process polling and stopping never wait for the assistant or an AI request.
async fn process_list(State(state): State<Local>) -> Json<Value> { Json(state.processes.list()) }
/// Return captured streams only when the user expands a process record.
async fn process_detail(State(state): State<Local>, axum::extract::Path(id): axum::extract::Path<Uuid>) -> LocalResult {
    Ok(Json(serde_json::to_value(state.processes.get(id)?)?))
}
/// Stop a daemon-owned process tree without changing the Full access preference.
async fn process_stop(State(state): State<Local>, axum::extract::Path(id): axum::extract::Path<Uuid>) -> LocalResult {
    state.processes.stop(id)?;
    Ok(Json(json!({"ok":true})))
}
/// Return a backend-created payment URL without handling card information locally.
async fn checkout(State(state): State<Local>,Json(input):Json<protocol::CheckoutRequest>) -> LocalResult {
    account_request(&state,reqwest::Method::POST,"/v1/checkout",Some(serde_json::to_value(input)?)).await
}
/// Load current purchase options without waiting for the conversation engine.
async fn credit_packs(State(state):State<Local>)->LocalResult { account_request(&state,reqwest::Method::GET,"/v1/credit-packs",None).await }
/// Read only this user's completed top-ups, including while a conversation owns the engine lock.
async fn purchases(State(state): State<Local>) -> LocalResult {
    account_request(&state,reqwest::Method::GET,"/v1/purchases",None).await
}
/// Watch a single owned purchase without waiting for an AI task or exposing credentials.
async fn purchase_status(State(state):State<Local>,axum::extract::Path(id):axum::extract::Path<String>)->LocalResult {
    if id.len()!=64 || !id.bytes().all(|byte|byte.is_ascii_hexdigit()) { return Err(anyhow::anyhow!("Invalid purchase reference").into()); }
    account_request(&state,reqwest::Method::GET,&format!("/v1/purchases/{id}"),None).await
}
/// Memories remain available while the conversation owns the engine lock.
async fn memories(State(state):State<Local>)->LocalResult { account_request(&state,reqwest::Method::GET,"/v1/memories",None).await }
/// Forward the user's revision so concurrent assistant writes cannot overwrite a draft silently.
async fn save_memories(State(state):State<Local>,Json(input):Json<Value>)->LocalResult { account_request(&state,reqwest::Method::POST,"/v1/memories",Some(input)).await }

/// Forward account requests with in-memory authentication without exposing the token to the browser.
async fn account_request(state:&Local,method:reqwest::Method,path:&str,body:Option<Value>)->LocalResult {
    Ok(Json(crate::session::account(&state.auth,&state.live,method,path,body).await?))
}
/// Stop the browser service explicitly; closing its tab leaves the local engine running.
async fn stop(State(state): State<Local>) -> Json<Value> { state.stop.notify_one(); Json(json!({"ok":true})) }

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    /// Process polling and tree cancellation must work while a command holds the conversation lock.
    #[tokio::test]
    async fn controls_processes_while_conversation_is_busy() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let engine=Engine::open("http://127.0.0.1:8787").await?;
        let processes=engine.processes.clone();
        let live=engine.live.clone();
        let full_access=engine.full_access.clone();
        let controls=engine.controls.clone();
        let account=engine.account.clone();
        let auth=engine.auth.clone(); let media=engine.media.clone();
        let engine=Arc::new(Mutex::new(engine));
        let local=Local { account, downloads:Arc::new(crate::downloads::Downloads::default()), brand:Arc::new(crate::config::Brand::default()),ui:Arc::new(crate::ui::Store::default()),auth,media,controls,live,full_access,engine:engine.clone(),processes:processes.clone(),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let app=router(local);
        let id=Uuid::new_v4();
        let proposal=protocol::CommandProposal { files:None, reason:"test".into(),cwd:directory.path().to_string_lossy().into_owned(),
            command:if cfg!(windows) { "[Console]::Out.WriteLine('live 世界'); [Console]::Error.WriteLine('problem'); Start-Sleep 20" } else { "printf 'live 世界\\n'; printf 'problem\\n' >&2; sleep 20" }.into() };
        let runner=processes.clone();
        let task=tokio::spawn(async move { runner.run(id,&proposal).await });
        let _busy=engine.lock().await;
        tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                if processes.get(id).is_ok_and(|r|r.stdout.contains("live 世界") && r.stderr.contains("problem")) { break; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await?;
        for (method,path) in [("GET","/api/processes".into()),("GET",format!("/api/processes/{id}")),("POST",format!("/api/processes/{id}/stop"))] {
            let request=Request::builder().method(method).uri(path).header(header::HOST,"127.0.0.1:1234").body(Body::empty())?;
            let response=tokio::time::timeout(Duration::from_secs(1),app.clone().oneshot(request)).await??;
            assert_eq!(response.status(),StatusCode::OK);
        }
        let record=tokio::time::timeout(Duration::from_secs(5),task).await???;
        assert!(record.pid.is_some() && !record.arguments.is_empty());
        assert!(record.finished_at.is_some() && record.error.as_deref().unwrap().contains("Stopped by user"));
        assert!(processes.stop(id).is_err());
        assert!(Processes::open()?.get(id).is_err());
        Ok(())
    }
    /// Account requests use current private in-memory authentication even while a task owns the engine lock.
    #[tokio::test]
    async fn account_requests_remain_available_during_a_task()->Result<()> {
        let directory=tempfile::tempdir()?;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/checkout",post(|headers:axum::http::HeaderMap,Json(input):Json<protocol::CheckoutRequest>|async move {
            assert_eq!(input.price_id,"price_fixture");
            assert_eq!(headers[header::AUTHORIZATION],"Bearer checkout-fixture");
            Json(json!({"url":"https://checkout.stripe.com/fixture"}))
        })).route("/v1/purchases",get(|headers:axum::http::HeaderMap|async move {
            assert_eq!(headers[header::AUTHORIZATION],"Bearer checkout-fixture");
            Json(json!({"purchases":[{"credits":100,"paid_at":123}]}))
        })).route("/v1/memories",get(|headers:axum::http::HeaderMap|async move {
            assert_eq!(headers[header::AUTHORIZATION],"Bearer checkout-fixture");
            Json(json!({"text":"Earlier result 世界","revision":"memory-1"}))
        }).post(|headers:axum::http::HeaderMap,Json(input):Json<Value>|async move {
            assert_eq!(headers[header::AUTHORIZATION],"Bearer checkout-fixture");
            assert_eq!(input,json!({"text":"Updated result 世界","revision":"memory-1"}));
            Json(json!({"text":input["text"],"revision":"memory-2"}))
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(&backend).await?;
        *engine.auth.token.write().unwrap()=Some("checkout-fixture".into());
        engine.settings(false)?;
        let local=Local { account:engine.account.clone(), downloads:Arc::new(crate::downloads::Downloads::default()), brand:Arc::new(crate::config::Brand::default()),ui:Arc::new(crate::ui::Store::default()),auth:engine.auth.clone(),media:engine.media.clone(),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let _busy=local.engine.lock().await;
        let result=tokio::time::timeout(Duration::from_secs(1),checkout(State(local.clone()),Json(protocol::CheckoutRequest { price_id:"price_fixture".into() }))).await?.map_err(|error|error.0)?;
        assert_eq!(result.0["url"],"https://checkout.stripe.com/fixture");
        let result=purchases(State(local.clone())).await.map_err(|error|error.0)?;
        assert_eq!(result.0["purchases"][0]["credits"],100);
        let result=tokio::time::timeout(Duration::from_secs(1),memories(State(local.clone()))).await?.map_err(|error|error.0)?;
        assert_eq!(result.0,json!({"text":"Earlier result 世界","revision":"memory-1"}));
        let result=tokio::time::timeout(Duration::from_secs(1),save_memories(State(local.clone()),Json(json!({"text":"Updated result 世界","revision":"memory-1"})))).await?.map_err(|error|error.0)?;
        assert_eq!(result.0,json!({"text":"Updated result 世界","revision":"memory-2"}));
        server.abort();
        Ok(())
    }

    /// Slow startup metadata must not collide with balance refreshes or report a busy assistant.
    #[tokio::test]
    async fn startup_metadata_does_not_lock_the_assistant()->Result<()> {
        let directory=tempfile::tempdir()?;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let entered=Arc::new(Notify::new()); let release=Arc::new(Notify::new());
        let (started,ready)=(entered.clone(),release.clone());
        let app=Router::new().route("/v1/auth/providers",get(move|| {
            let (started,ready)=(started.clone(),ready.clone());
            async move { started.notify_one(); ready.notified().await; Json(json!({"providers":[],"email_enabled":true})) }
        })).route("/v1/me",get(||async { Json(json!({"email":"fixture@example.test","credits":22,"development":true,"input_credits_per_million":200})) }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(&backend).await?;
        *engine.auth.token.write().unwrap()=Some("fixture".into()); engine.settings(true)?;
        let local=Local { account:engine.account.clone(), downloads:Arc::new(crate::downloads::Downloads::default()), brand:Arc::new(crate::config::Brand::default()),ui:Arc::new(crate::ui::Store::default()),auth:engine.auth.clone(),media:engine.media.clone(),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let metadata=local.clone();
        let discovery=tokio::spawn(async move { auth_providers(State(metadata)).await.map_err(|error|error.0) });
        tokio::time::timeout(Duration::from_secs(1),entered.notified()).await?;
        let result=tokio::time::timeout(Duration::from_secs(1),account(State(local.clone()))).await?.map_err(|error|error.0)?;
        assert_eq!(result.0["account"]["credits"],22);
        release.notify_one();
        assert_eq!(discovery.await??.0["email_enabled"],true);
        let _busy=local.engine.lock().await;
        let cached=tokio::time::timeout(Duration::from_secs(1),account(State(local.clone()))).await?.map_err(|error|error.0)?;
        assert_eq!(cached.0["account"]["credits"],22);
        release.notify_one();
        assert_eq!(tokio::time::timeout(Duration::from_secs(1),auth_providers(State(local.clone()))).await?.map_err(|error|error.0)?.0["email_enabled"],true);
        server.abort();
        Ok(())
    }

    /// Plain URLs work without tokens; branding is immediate and unrelated websites remain blocked.
    #[tokio::test]
    async fn protects_local_api() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let engine=Engine::open("http://127.0.0.1:8787").await?;
        let local = Local { account:engine.account.clone(), downloads:Arc::new(crate::downloads::Downloads::default()), brand:Arc::new(crate::config::Brand::default()),ui:Arc::new(crate::ui::Store::default()),auth:engine.auth.clone(),media:engine.media.clone(),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let app = router(local);
        for (host,origin,site,status) in [
            ("127.0.0.1:1234",None,None,StatusCode::OK),
            ("evil.example",None,None,StatusCode::FORBIDDEN),
            ("127.0.0.1:1234",Some("https://evil.example"),None,StatusCode::FORBIDDEN),
            ("127.0.0.1:1234",None,Some("cross-site"),StatusCode::FORBIDDEN),
            ("127.0.0.1:1234",None,Some("same-origin"),StatusCode::OK),
        ] {
            let mut request = Request::builder().uri("/api/state").header(header::HOST,host);
            if let Some(origin) = origin { request = request.header(header::ORIGIN,origin); }
            if let Some(site) = site { request = request.header("sec-fetch-site",site); }
            assert_eq!(app.clone().oneshot(request.body(Body::empty())?).await?.status(),status);
        }
        Ok(())
    }
}
