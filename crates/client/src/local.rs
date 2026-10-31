use std::{fs::OpenOptions, future::IntoFuture, path::{Path, PathBuf}, sync::Arc, time::Duration};
use anyhow::{bail, Context, Result};
use axum::{body::Body, extract::{DefaultBodyLimit, State}, http::{header, Request, StatusCode}, middleware::{self, Next}, response::{Html, IntoResponse, Response}, routing::{get, post}, Json, Router};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use crate::{engine::Engine, processes::Processes, storage};

#[derive(Clone, Serialize, Deserialize)]
pub struct Connection { pub url: String }
#[derive(Clone)]
struct Local { brand:Arc<crate::config::Brand>, session_path:PathBuf, controls:Arc<crate::controls::Controls>, live: crate::stream::Live, full_access: Arc<std::sync::atomic::AtomicBool>, engine: Arc<Mutex<Engine>>, processes: Processes, connection: Connection, stop: Arc<Notify> }
struct LocalError(anyhow::Error);
type LocalResult = std::result::Result<Json<Value>,LocalError>;

impl<E: Into<anyhow::Error>> From<E> for LocalError {
    /// Surface local errors to the user without logging session-bearing request bodies.
    fn from(value: E) -> Self { Self(value.into()) }
}
impl IntoResponse for LocalError {
    /// Both frontends consume the same error envelope.
    fn into_response(self) -> Response { (StatusCode::BAD_REQUEST,Json(json!({"error":self.0.to_string()}))).into_response() }
}

impl Connection {
    /// Send requests only to the exact loopback origin.
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
        if !matches!(path,"/api/command"|"/api/chat"|"/api/retry") { request=request.timeout(Duration::from_secs(1800)); }
        if let Some(body) = body { request = request.json(&body); }
        let response = request.send().await?;
        let status = response.status();
        let body: Value = response.json().await?;
        anyhow::ensure!(status.is_success(),"{}",body["error"].as_str().unwrap_or("Local request failed"));
        Ok(body)
    }
}

/// Reuse a live daemon, or detach this executable and wait for its private discovery file.
pub async fn connect(directory: &Path, args:&crate::Args, brand:&crate::config::Brand) -> Result<Connection> {
    let backend=args.backend.as_str();
    let path = directory.join("daemon.json");
    if let Ok(connection) = storage::read::<Connection>(&path) {
        if tokio::time::timeout(Duration::from_secs(2),connection.get("/api/health")).await.is_ok_and(|r|r.is_ok()) {
            anyhow::ensure!(connection.get("/api/state").await?["settings"]["backend"]==backend,"A client is already running for a different backend; stop it before changing servers");
            anyhow::ensure!(connection.get("/api/branding").await?==brand.metadata(),"A client is already running with different branding; stop it before changing the name or logo");
            return Ok(connection);
        }
    }
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let log=OpenOptions::new().create(true).append(true).mode(0o600).open(directory.join("daemon.log"))?;
        let mut command=std::process::Command::new(std::env::current_exe()?);
        command.args(args.daemon_args()).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
        unsafe { command.pre_exec(|| { if libc::setsid()==-1 { return Err(std::io::Error::last_os_error()); } Ok(()) }); }
        command.spawn()?;
    }
    #[cfg(windows)]
    crate::windows_launcher::spawn(&args.daemon_args())?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(connection) = storage::read::<Connection>(&path) {
            if tokio::time::timeout(Duration::from_millis(500),connection.get("/api/health")).await.is_ok_and(|r|r.is_ok()) {
                anyhow::ensure!(connection.get("/api/state").await?["settings"]["backend"]==backend,"A client started for a different backend; stop it before changing servers");
                anyhow::ensure!(connection.get("/api/branding").await?==brand.metadata(),"A client started with different branding; stop it before changing the name or logo");
                return Ok(connection);
            }
        }
    }
    bail!("Local daemon did not start; inspect {}",directory.join("daemon.log").display())
}

/// Bind loopback only and hold an exclusive lock for this entire daemon lifetime.
pub async fn serve(directory: PathBuf, backend:&str,brand:crate::config::Brand) -> Result<()> {
    let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(directory.join("daemon.lockfile"))?;
    if lock.try_lock_exclusive().is_err() { return Ok(()); }
    let engine = Engine::open(&directory,backend).await?;
    // Retain the local address across restarts so existing browser tabs can simply refresh.
    let discovery=directory.join("daemon.json");
    let port=if discovery.exists() { reqwest::Url::parse(&storage::read::<Connection>(&discovery)?.url)?.port().context("Local client URL is missing its port")? } else { 0 };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1",port)).await?;
    let connection = Connection { url:format!("http://{}",listener.local_addr()?) };
    storage::write(&discovery,&connection)?;
    engine.live.0.send_modify(|view|view["brand"]=brand.metadata());
    let processes=engine.processes.clone();
    let state = Local { brand:Arc::new(brand),session_path:directory.join("state.json"),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),engine:Arc::new(Mutex::new(engine)),processes:processes.clone(),connection,stop:Arc::new(Notify::new()) };
    let stop = state.stop.clone();
    tokio::select! {
        result=axum::serve(listener,router(state)).into_future() => result?,
        _=async { tokio::select! { _=stop.notified()=>{}, _=tokio::signal::ctrl_c()=>{} } } => {
            processes.stop_all();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}

/// Embedded assets make the browser mode independent of installed files or Node.js.
fn router(state: Local) -> Router {
    Router::new()
        .route("/",get(|State(state):State<Local>|async move {
            // Brand the initial document too, before scripts or account data have loaded.
            let name=state.brand.name.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&#39;");
            Html(include_str!("../web/index.html").replace("{{APP_NAME}}",&name).replace("{{ICONS}}",protocol::ICONS_SVG))
        }))
        .route("/assistant.png",get(||async { ([(header::CONTENT_TYPE,"image/png")],include_bytes!("../web/assistant.png").as_slice()) }))
        .route("/attachments.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/attachments.js")) }))
        .route("/markdown-it.min.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/vendor/markdown-it.min.js")) }))
        .route("/markdown.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/markdown.js")) }))
        .route("/app.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/app.js")) }))
        .route("/style.css",get(||async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/style.css")) }))
        .route("/logo.png",get(|State(state):State<Local>|async move { match &state.brand.logo { Some(bytes)=>([(header::CONTENT_TYPE,"image/png")],bytes.clone()).into_response(),None=>StatusCode::NOT_FOUND.into_response() } }))
        .route("/api/branding",get(|State(state):State<Local>|async move { Json(state.brand.metadata()) }))
        .route("/api/health",get(||async { Json(json!({"ok":true})) }))
        .route("/api/events",get(events)).route("/api/access",post(access))
        .route("/api/state",get(state_view)).route("/api/account",post(account))
        .route("/api/settings",post(settings))
        .route("/api/login",post(login)).route("/api/login/verify",post(verify_login)).route("/api/logout",post(logout))
        .route("/api/login/providers",get(auth_providers)).route("/api/login/social",post(social_start))
        .route("/api/login/poll",post(social_poll)).route("/api/login/cancel",post(social_cancel))
        .route("/api/chat",post(chat).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES))).route("/api/steer",post(steer).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES))).route("/api/interrupt",post(interrupt)).route("/api/queue/{id}/remove",post(remove_queued)).route("/api/retry",post(retry)).route("/api/new",post(new_chat))
        .route("/api/queue/{id}/edit",post(edit_queued)).route("/api/queue/{id}/steer",post(steer_queued))
        .route("/api/command",post(decide)).route("/api/credit-packs",get(credit_packs)).route("/api/checkout",post(checkout)).route("/api/purchases",get(purchases)).route("/api/stop",post(stop))
        .route("/api/processes",get(process_list)).route("/api/processes/{id}",get(process_detail))
        .route("/api/processes/{id}/stop",post(process_stop))
        .layer(DefaultBodyLimit::max(128*1024)).layer(middleware::from_fn_with_state(state.clone(),security)).with_state(state)
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
    for (key,value) in [("cache-control","no-store"),("referrer-policy","no-referrer"),("x-content-type-options","nosniff"),
        ("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")] {
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
#[derive(Deserialize)]
struct Decision { id: Uuid, approve: bool }

/// Read the same assistant state in either interface.
async fn state_view(State(state): State<Local>) -> Json<Value> { Json(state.live.0.borrow()["state"].clone()) }

/// Public sign-in discovery never locks the conversation or blocks account refreshes.
async fn auth_providers(State(state):State<Local>)->LocalResult {
    let backend=state.live.0.borrow()["state"]["settings"]["backend"].as_str().unwrap().to_owned();
    let request=crate::backend::client(60)?.get(format!("{backend}/v1/auth/providers"));
    Ok(Json(Engine::response(crate::backend::send(request,&state.live,None).await?).await?))
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
    let mut pending=None;
    if enabled {
        let engine=state.engine.try_lock().context("Wait for the current operation before enabling full access")?;
        if engine.saved.token.is_none() || !engine.saved.settings.commands_enabled { return Err(anyhow::anyhow!("Enable local commands first").into()); }
        pending=engine.saved.pending.as_ref().map(|pending|pending.id);
    }
    state.full_access.store(enabled,std::sync::atomic::Ordering::SeqCst);
    state.live.0.send_modify(|view|view["state"]["full_access"]=json!(enabled));
    if let Some(id)=pending { return run(state,Operation::Decide(id,true)).await; }
    Ok(Json(state.live.0.borrow()["state"].clone()))
}
/// Persist execution permission shared by the browser and terminal.
async fn settings(State(state): State<Local>, Json(input): Json<SettingsInput>) -> LocalResult {
    let mut engine = state.engine.try_lock().context("Assistant is busy")?;
    engine.settings(input.commands_enabled)?;
    Ok(Json(engine.state()))
}
/// Refresh when idle; an active operation will publish updated state when it completes.
async fn account(State(state): State<Local>) -> LocalResult {
    let Ok(mut engine)=state.engine.try_lock() else { return Ok(Json(state.live.0.borrow()["state"].clone())); };
    Ok(Json(engine.refresh_account().await?))
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
    if state.live.0.borrow()["busy"]==true || !state.live.0.borrow()["state"]["pending"].is_null() { return Ok(Json(state.live.0.borrow()["state"].clone())); }
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
/// Execute only the exact proposal identified by the one-use approval ID.
async fn decide(State(state): State<Local>, Json(input): Json<Decision>) -> LocalResult {
    run(state,Operation::Decide(input.id,input.approve)).await
}
/// Steering bypasses the engine lock and uses the backend's active-turn protocol.
async fn steer(State(state):State<Local>,Json(input):Json<protocol::Steering>)->LocalResult {
    let result=state.controls.steer(&input).await?;
    record_steering(&state,&input);
    wake_queue(state);
    Ok(Json(result))
}
/// Show accepted live input from either the composer or queue without waiting for the turn to finish.
fn record_steering(state:&Local,input:&protocol::Steering) {
    state.live.0.send_modify(|view| {
        if view["busy"]==true {
            let inputs=view["steering"].as_array_mut().unwrap();
            if !inputs.iter().any(|old|old["id"]==input.id.to_string()) { inputs.push(protocol::display_messages(&[input])[0].clone()); }
        }
    });
}
/// Stop the current task and its owned command; queued messages remain paused.
async fn interrupt(State(state):State<Local>)->LocalResult {
    state.full_access.store(false,std::sync::atomic::Ordering::SeqCst);
    state.live.0.send_modify(|view|view["state"]["full_access"]=json!(false));
    state.processes.stop_all();
    state.controls.interrupt().await?;
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
    let waiting=view["busy"]==true || !view["state"]["pending"].is_null() || view["state"]["retry_available"]==true;
    if waiting || state.controls.stopped.load(std::sync::atomic::Ordering::SeqCst) { return; }
    tokio::spawn(async move { let _=run(state,Operation::Next).await; });
}
enum Operation { Next, Retry, Decide(Uuid,bool) }

/// Own the task independently of browser connections; consume every approved proposal exactly once.
async fn run(state: Local, operation: Operation) -> LocalResult {
    let mut engine=match state.engine.clone().try_lock_owned() {
        Ok(engine)=>engine,
        Err(_) if matches!(operation,Operation::Next)=>return Ok(Json(state.live.0.borrow()["state"].clone())),
        Err(error)=>return Err(anyhow::anyhow!(error).into()),
    };
    state.controls.stopped.store(false,std::sync::atomic::Ordering::SeqCst);
    state.live.0.send_modify(|view| { view["busy"]=json!(true); view["error"]=Value::Null; view["activity"]=Value::Null; view["steering"]=json!([]); });
    Ok(Json(tokio::spawn(async move {
        let result=async {
            match operation {
                Operation::Next=> { engine.next().await?; }
                Operation::Retry=> { engine.retry().await?; }
                Operation::Decide(id,approve)=> { engine.decide(id,approve).await?; }
            }
            loop {
                if state.controls.stopped.load(std::sync::atomic::Ordering::SeqCst) { break; }
                if let Some(pending)=engine.saved.pending.clone() {
                    if !state.full_access.load(std::sync::atomic::Ordering::SeqCst) { break; }
                    engine.decide(pending.id,true).await?;
                    continue;
                }
                if !engine.next().await? { break; }
            }
            Ok::<_,anyhow::Error>(engine.state())
        }.await;
        let presentation=if result.is_err() || state.controls.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            engine.finish_activity(result.as_ref().err().map(ToString::to_string))
        } else { Ok(()) };
        let result=result.and_then(|_|presentation.map(|_|engine.state()));
        state.live.state(engine.state());
        state.live.0.send_modify(|view| {
            view["busy"]=json!(false);
            view["steering"]=json!([]);
            view["error"]=json!(result.as_ref().err().map(ToString::to_string));
            if result.is_ok() { view["activity"]=Value::Null; }
        });
        result
    }).await??))
}

/// Process polling and stopping never wait for the assistant or an AI request.
async fn process_list(State(state): State<Local>) -> Json<Value> { Json(state.processes.list()) }
/// Return captured streams only when the user expands a process record.
async fn process_detail(State(state): State<Local>, axum::extract::Path(id): axum::extract::Path<Uuid>) -> LocalResult {
    Ok(Json(serde_json::to_value(state.processes.get(id)?)?))
}
/// Stop only a daemon-owned process tree identified by its opaque execution ID.
async fn process_stop(State(state): State<Local>, axum::extract::Path(id): axum::extract::Path<Uuid>) -> LocalResult {
    state.full_access.store(false,std::sync::atomic::Ordering::SeqCst);
    state.live.0.send_modify(|view|view["state"]["full_access"]=json!(false));
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
/// Forward account requests with durable authentication without exposing the token to the browser.
async fn account_request(state:&Local,method:reqwest::Method,path:&str,body:Option<Value>)->LocalResult {
    // Durable authentication can be read while a task holds the engine, without exposing it to the browser.
    let saved:crate::engine::Saved=storage::read(&state.session_path)?;
    let mut request=crate::backend::client(60)?.request(method,format!("{}{path}",saved.settings.backend)).bearer_auth(saved.token.context("Sign in first")?);
    if let Some(body)=body { request=request.json(&body); }
    let response=crate::backend::send(request,&state.live,None).await?;
    let status=response.status();
    let value:Value=response.json().await?;
    if !status.is_success() { return Err(anyhow::anyhow!("{}",value["error"].as_str().unwrap_or("Account request failed")).into()); }
    Ok(Json(value))
}
/// Stop explicitly; closing a browser tab leaves the assistant available to the CLI.
async fn stop(State(state): State<Local>) -> Json<Value> { state.stop.notify_one(); Json(json!({"ok":true})) }

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    /// A disconnected caller cannot repeat a command; revoking full access works while the shell runs.
    #[tokio::test]
    async fn streaming_task_survives_disconnect_and_revokes_automatic_execution() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let provider=Router::new().route("/v1/chat",post(|Json(request):Json<protocol::ChatRequest>|async move {
            let proposal=protocol::CommandProposal { files:None, cwd:request.cwd,reason:"Write one marker".into(),
                command:if cfg!(windows) { "Add-Content -Encoding UTF8 -LiteralPath 'runs.txt' -Value 'once'; Start-Sleep -Milliseconds 700" }
                    else { "printf 'once\\n' >> runs.txt; sleep 0.7" }.into() };
            let response=protocol::ChatResponse { progress:None, reply:protocol::AssistantReply { text:"A command proposal".into(),command:Some(proposal) },credits:10,revision:request.base+request.messages.len()+1,steering:vec![] };
            ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete { response }).unwrap()))
        }));
        let provider=tokio::spawn(async move { axum::serve(listener,provider).await.unwrap(); });
        let mut engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        engine.set_backend(backend.clone()).await?;
        engine.saved.token=Some("fixture".into());
        engine.settings(true)?;
        engine.full_access.store(true,std::sync::atomic::Ordering::SeqCst);
        let live=engine.live.clone();
        let processes=engine.processes.clone();
        let app=router(Local { brand:Arc::new(crate::config::Brand::default()),session_path:directory.path().join("state.json"),controls:engine.controls.clone(),full_access:engine.full_access.clone(),live:live.clone(),processes:processes.clone(),engine:Arc::new(Mutex::new(engine)),
            connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) });
        let request=Request::builder().method("POST").uri("/api/chat").header("host","127.0.0.1:1234").header("content-type","application/json")
            .body(Body::from(json!({"text":"Write a marker","cwd":directory.path()}).to_string()))?;
        let caller=tokio::spawn(app.clone().oneshot(request));
        tokio::time::timeout(Duration::from_secs(10),async {
            while processes.list().as_array().unwrap().is_empty() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await?;
        caller.abort();
        for (method,path,body) in [("POST","/api/access","{\"enabled\":false}"),("GET","/api/state",""),("GET","/api/events","")] {
            let request=Request::builder().method(method).uri(path).header("host","127.0.0.1:1234").header("content-type","application/json").body(Body::from(body))?;
            assert_eq!(tokio::time::timeout(Duration::from_secs(1),app.clone().oneshot(request)).await??.status(),StatusCode::OK);
        }
        tokio::time::timeout(Duration::from_secs(10),async {
            while live.0.borrow()["busy"]==true { tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await?;
        assert!(live.0.borrow()["error"].is_null(),"{}",*live.0.borrow());
        assert_eq!(std::fs::read_to_string(directory.path().join("runs.txt"))?.lines().count(),1);
        assert_eq!(processes.list().as_array().unwrap().len(),1);
        assert!(!live.0.borrow()["state"]["pending"].is_null());
        assert_eq!(live.0.borrow()["state"]["full_access"],false);
        provider.abort();
        Ok(())
    }

    /// Process polling and tree cancellation must work while a command holds the conversation lock.
    #[tokio::test]
    async fn controls_processes_while_conversation_is_busy() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        let processes=engine.processes.clone();
        let live=engine.live.clone();
        let full_access=engine.full_access.clone();
        let controls=engine.controls.clone();
        let engine=Arc::new(Mutex::new(engine));
        let local=Local { brand:Arc::new(crate::config::Brand::default()),session_path:directory.path().join("state.json"),controls,live,full_access,engine:engine.clone(),processes:processes.clone(),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
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
        let recovered=Processes::open(directory.path())?.get(id)?;
        assert_eq!(recovered.stdout,record.stdout);
        assert_eq!(recovered.stderr,record.stderr);
        Ok(())
    }
    /// Account requests use current private saved authentication even while a task owns the engine lock.
    #[tokio::test]
    async fn checkout_remains_available_during_a_task()->Result<()> {
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
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.saved.token=Some("checkout-fixture".into());
        engine.settings(false)?;
        let local=Local { brand:Arc::new(crate::config::Brand::default()),session_path:directory.path().join("state.json"),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let _busy=local.engine.lock().await;
        let result=tokio::time::timeout(Duration::from_secs(1),checkout(State(local.clone()),Json(protocol::CheckoutRequest { price_id:"price_fixture".into() }))).await?.map_err(|error|error.0)?;
        assert_eq!(result.0["url"],"https://checkout.stripe.com/fixture");
        let result=purchases(State(local.clone())).await.map_err(|error|error.0)?;
        assert_eq!(result.0["purchases"][0]["credits"],100);
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
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.saved.token=Some("fixture".into()); engine.settings(true)?;
        let local=Local { brand:Arc::new(crate::config::Brand::default()),session_path:directory.path().join("state.json"),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
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
        let engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        let local = Local { brand:Arc::new(crate::config::Brand { name:"Slax 世界 & <AI>".into(),logo:None }),session_path:directory.path().join("state.json"),controls:engine.controls.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),processes:engine.processes.clone(),engine:Arc::new(Mutex::new(engine)),connection:Connection { url:"http://127.0.0.1:1234".into() },stop:Arc::new(Notify::new()) };
        let app = router(local);
        let page=app.clone().oneshot(Request::builder().uri("/").header(header::HOST,"127.0.0.1:1234").body(Body::empty())?).await?;
        let html=String::from_utf8(axum::body::to_bytes(page.into_body(),256*1024).await?.to_vec())?;
        assert!(html.contains("<title>Slax 世界 &amp; &lt;AI&gt;</title>"));
        assert!(!html.contains("{{APP_NAME}}") && !html.contains("<title>Linux Assistant</title>"));
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
