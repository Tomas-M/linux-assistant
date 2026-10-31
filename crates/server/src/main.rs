// Match the embedded App Server's limit for deeply nested async types.
#![recursion_limit = "256"]

mod auth;
mod admin;
mod config;
mod settings;
mod db;
mod diagnostics;
mod embedded;
mod mail;
mod payments;
mod provider;
mod usage;
mod billing;
mod turns;
#[cfg(test)]
mod tests;

use std::{fs::OpenOptions, sync::Arc, time::Duration};
use axum::{body::Body, extract::{DefaultBodyLimit, State}, http::{header, HeaderMap, Request, StatusCode}, middleware::{self, Next}, response::{Html, IntoResponse, Response}, routing::{get, post}, Json, Router};
use clap::Parser;
use fs2::FileExt;
use protocol::{Account, ChatEvent, ChatRequest, ChatResponse, MAX_REQUEST_BYTES};
use tokio_stream::{wrappers::WatchStream, StreamExt};
use serde_json::{json, Value};
use crate::db::{hash, Db, Reservation};
#[cfg(test)]
use crate::config::Config;

#[derive(Clone)]
pub struct App { turns: Arc<turns::Active>, settings: Arc<std::sync::RwLock<Arc<settings::Runtime>>>, db: Arc<Db>, http: reqwest::Client, pool: Arc<provider::Pool> }
pub struct ApiError(StatusCode, String);
pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    /// Report input errors without exposing internal service details.
    pub fn bad(message: &str) -> Self { Self(StatusCode::BAD_REQUEST,message.into()) }
    /// Ask callers to retry transient failures.
    pub fn busy(message: &str) -> Self { Self(StatusCode::SERVICE_UNAVAILABLE,message.into()) }
}
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    /// Keep database details and upstream responses out of public error bodies.
    fn from(error: E) -> Self { diagnostics::record("error","backend",None,None,&format!("{:#}",error.into())); Self(StatusCode::INTERNAL_SERVER_ERROR,"Backend operation failed".into()) }
}
impl IntoResponse for ApiError {
    /// Every API failure has the same machine-readable shape.
    fn into_response(self) -> Response { (self.0,Json(json!({"error":self.1}))).into_response() }
}

impl App {
    #[cfg(test)]
    /// Isolate route tests from production accounts, databases and SMTP transports.
    fn testing(config:Config)->anyhow::Result<Self> {
        Ok(Self { turns:Arc::new(Default::default()),pool:Arc::new(provider::Pool::new(&config)?),settings:Arc::new(std::sync::RwLock::new(Arc::new(settings::Runtime { config,mail:None }))),
            db:Arc::new(Db::open(std::path::Path::new(":memory:"))?),http:reqwest::Client::new() })
    }

    /// Each request keeps a coherent settings and SMTP snapshot while admin changes affect subsequent requests.
    fn config(&self)->Arc<settings::Runtime> { self.settings.read().unwrap().clone() }

    /// Authenticate every paid or account-specific operation at the backend boundary.
    fn user(&self, headers: &HeaderMap) -> ApiResult<i64> {
        let token = headers.get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer "))
            .ok_or_else(||ApiError(StatusCode::UNAUTHORIZED,"Sign in first".into()))?;
        self.db.authenticate(token).map_err(|_|ApiError(StatusCode::UNAUTHORIZED,"Session expired; sign in again".into()))
    }
}

/// Use upstream's runtime and stack sizing only inside isolated provider workers.
fn main() -> anyhow::Result<()> {
    if std::env::args_os().any(|arg|arg=="--provider-worker") {
        return codex_arg0::arg0_dispatch_or_else(embedded::run);
    }
    run().map_err(|error| {
        let message=diagnostics::clean(&format!("{error:#}"));
        diagnostics::record("error","backend_runtime",None,None,&message);
        anyhow::anyhow!(message)
    })
}

/// Run one backend process per SQLite database so startup recovery is unambiguous.
#[tokio::main]
async fn run() -> anyhow::Result<()> {
    let args=config::Args::parse();
    anyhow::ensure!(args.listen.is_none_or(|address|address.ip().is_loopback()),"Development mode must bind to loopback");
    if args.codex_version { println!("Codex App Server {}",env!("CODEX_VERSION")); return Ok(()); }
    if args.third_party_notices {
        println!("{}\n{}",include_str!("../../../vendor/codex/LICENSE"),include_str!("../../../vendor/codex/NOTICE"));
        return Ok(());
    }
    let database=args.database.unwrap_or_else(config::database_path);
    std::fs::create_dir_all(database.parent().unwrap())?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(database.parent().unwrap(),std::fs::Permissions::from_mode(0o700))?;
    }
    let lock=OpenOptions::new().create(true).truncate(false).read(true).write(true).open(database.with_extension("lockfile"))?;
    lock.try_lock_exclusive().map_err(|_|anyhow::anyhow!("Another backend is using this database"))?;
    let db=Arc::new(Db::open(&database)?);
    db.recover()?;
    let mut config=settings::load(&db)?;
    config.database=database;
    config.dev=args.dev;
    config.listen=args.listen;
    let listener=tokio::net::TcpListener::bind(config.listen.unwrap_or_else(||"127.0.0.1:8787".parse().unwrap())).await?;
    if config.dev { config.public_url=format!("http://{}",listener.local_addr()?); }
    diagnostics::init(&config)?;
    let pool=Arc::new(provider::Pool::new(&config)?);
    let app=App { turns:Arc::new(Default::default()),pool,settings:Arc::new(std::sync::RwLock::new(Arc::new(settings::Runtime::new(config)?))),db,
        http:reqwest::Client::builder().timeout(Duration::from_secs(30)).redirect(reqwest::redirect::Policy::none()).build()? };
    println!("Linux Assistant backend on {}{}",listener.local_addr()?,if args.dev { " (DEVELOPMENT, simulated AI)" } else { "" });
    diagnostics::record("info","startup",None,None,&format!("Backend started; Codex {}; configure at /admin",env!("CODEX_VERSION")));
    if !args.dev { tokio::spawn(app.pool.clone().monitor(app.db.clone())); }
    axum::serve(listener,router(app)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}

/// Register hosted authentication and authenticated application APIs in one server.
fn router(app: App) -> Router {
    Router::new().merge(admin::router(app.clone())).route("/health",get(||async { Json(json!({"ok":true})) }))
        .route("/v1/auth/device",post(auth::start)).route("/v1/auth/poll",post(auth::poll))
        .route("/v1/auth/email",post(auth::email)).route("/v1/auth/verify",post(auth::verify))
        .route("/v1/auth/providers",get(auth::social::providers)).route("/v1/auth/cancel",post(auth::social::cancel))
        .route("/auth/{provider}/{device}",get(auth::social::begin)).route("/auth/{provider}/callback",get(auth::social::callback))
        .route("/auth/complete.js",get(||async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("auth/complete.js")) }))
        .route("/v1/me",get(me)).route("/v1/logout",post(logout)).route("/v1/chat",post(chat).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES)))
        .route("/v1/chat/{id}/steer",post(turns::steer).layer(DefaultBodyLimit::max(protocol::MAX_UPLOAD_BYTES))).route("/v1/chat/{id}/stop",post(turns::stop))
        .route("/v1/credit-packs",get(payments::packs)).route("/v1/checkout",post(payments::checkout)).route("/v1/purchases",get(payments::purchases)).route("/webhooks/stripe",post(payments::webhook))
        .route("/payment/success",get(||async { Html(auth::document("<h1>Payment submitted</h1><p>Credits appear after the payment is confirmed. Return to Linux Assistant and refresh your balance.</p>")) }))
        .route("/payment/cancel",get(||async { Html(auth::document("<h1>Checkout cancelled</h1><p>Return to Linux Assistant to continue.</p>")) }))
        .layer(DefaultBodyLimit::max(256*1024)).layer(middleware::from_fn_with_state(app.clone(),browser_security)).with_state(app)
}

/// Public bearer-token APIs allow any origin; cookie-based admin and OAuth routes stay same-origin.
async fn browser_security(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    let config=app.config();
    let admin=request.uri().path().starts_with("/admin");
    let callback=request.uri().path().starts_with("/auth/") && request.uri().path().ends_with("/callback");
    let public=request.uri().path().starts_with("/v1/") || request.uri().path()=="/health";
    if !public {
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            let origin=origin.to_str().unwrap_or("");
            let allowed=if config.public_url.is_empty() {
                // First-run setup has no configured origin yet; require the browser's origin to match the request host.
                reqwest::Url::parse(origin).is_ok_and(|url|url.origin().ascii_serialization()==format!("{}://{}",url.scheme(),request.headers().get(header::HOST).and_then(|value|value.to_str().ok()).unwrap_or("")))
            } else { origin==config.public_url };
            if !allowed { return StatusCode::FORBIDDEN.into_response(); }
        }
    }
    let mut response = if public && request.method()==axum::http::Method::OPTIONS { StatusCode::NO_CONTENT.into_response() } else { next.run(request).await };
    if public {
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN,"*".parse().unwrap());
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_METHODS,"GET, POST, OPTIONS".parse().unwrap());
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_HEADERS,"authorization, content-type, accept".parse().unwrap());
    }
    response.headers_mut().insert(header::CACHE_CONTROL,"no-store".parse().unwrap());
    response.headers_mut().insert("Referrer-Policy","same-origin".parse().unwrap());
    response.headers_mut().insert("X-Content-Type-Options","nosniff".parse().unwrap());
    let policy=if admin { "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'" }
        else if callback { "default-src 'none'; script-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'" }
        else { "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'" };
    response.headers_mut().insert("Content-Security-Policy",policy.parse().unwrap());
    response
}

/// Return an authoritative balance; the client never computes or deducts credits.
async fn me(State(app): State<App>, headers: HeaderMap) -> ApiResult<Json<Account>> {
    let config=app.config();
    let user = app.user(&headers)?;
    let email = app.db.0.lock().unwrap().query_row("SELECT email FROM users WHERE id=?1",[user],|r|r.get(0))?;
    Ok(Json(Account { email,credits:app.db.balance(user)?,development:config.dev,input_credits_per_million:config.pricing.input_credits_per_million.0 as f64/1_000_000.0 }))
}

/// Revoke the current session rather than merely forgetting its client-side copy.
async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    app.user(&headers)?;
    let token = headers[header::AUTHORIZATION].to_str().unwrap().strip_prefix("Bearer ").unwrap();
    app.db.0.lock().unwrap().execute("DELETE FROM sessions WHERE token_hash=?1",[hash(token)])?;
    Ok(Json(json!({"ok":true})))
}

/// Reserve, generate, and settle even when the HTTP caller disconnects mid-request.
async fn chat(State(app): State<App>, headers: HeaderMap, Json(input): Json<ChatRequest>) -> ApiResult<Response> {
    let config=app.config();
    let user = app.user(&headers)?;
    let bytes = serde_json::to_vec(&input)?;
    if input.messages.is_empty() || bytes.len()>protocol::MAX_UPLOAD_BYTES || protocol::display_messages(&input.messages).to_string().len()>MAX_REQUEST_BYTES || input.cwd.len()>4096 {
        return Err(ApiError::bad("New input is empty or exceeds the per-request limit"));
    }
    for message in &input.messages {
        if message.role==protocol::Role::User { protocol::validate_input(&message.text,message.attachments.as_deref()).map_err(|error|ApiError::bad(&error.to_string()))?; }
        else if message.attachments.is_some() { return Err(ApiError::bad("Only user messages can include attachments")); }
    }
    if input.messages.iter().any(|m|m.text.len()>64_000) {
        return Err(ApiError::bad("Message is too long"));
    }
    let request_id=input.request_id;
    let id = request_id.to_string();
    let reservation = app.db.reserve(user,&input,&hash(std::str::from_utf8(&bytes).unwrap()),&config.pricing)?;
    let streaming=headers.get(header::ACCEPT).is_some_and(|value|value=="text/event-stream");
    let (events,receiver)=tokio::sync::watch::channel(ChatEvent::Progress { progress:Default::default() });
    let reply: db::Completed = match reservation {
        Reservation::Cached(value) => serde_json::from_str(&value)?,
        Reservation::Failed(error) => {
            if !streaming { return Err(ApiError::busy(&error)); }
            events.send_replace(ChatEvent::Error { task_credits:Some(app.db.task_credits(user,request_id)?), progress:None,message:error,settled:true,credits:Some(app.db.balance(user)?),steering:app.db.steering(user,input.request_id)? });
            return Ok(chat_stream(receiver));
        },
        Reservation::Pending => return Err(ApiError(StatusCode::CONFLICT,"Request is still running; retry the same request shortly".into())),
        Reservation::Insufficient => return Err(ApiError(StatusCode::PAYMENT_REQUIRED,"No application credits remaining".into())),
        Reservation::Stale => return Err(ApiError::bad("Conversation revision changed; start a new chat after a server reset")),
        Reservation::New => {
            let (controls,mut control_receiver)=tokio::sync::mpsc::channel(16);
            let (cancel,mut cancellation)=tokio::sync::watch::channel(false);
            app.turns.lock().unwrap().insert((user,input.request_id),turns::ActiveTurn { sender:controls,cancel });
            let task_app = app.clone();
            let task_events=events.clone();
            let task=tokio::spawn(async move {
                let result = tokio::select! {
                    answer=task_app.pool.answer_stream(&config,&task_app.db,user,&input,streaming.then_some(&task_events),Some(&mut control_receiver))=>answer,
                    _=cancellation.changed()=>Err(anyhow::Error::from(turns::Interrupted)),
                }.and_then(|answer| {
                    task_app.db.complete(user,&input,&answer.reply,answer.account_home.as_deref(),answer.thread_id.as_deref(),answer.checkpoint.as_deref())
                });
                task_app.turns.lock().unwrap().remove(&(user,input.request_id));
                let result=match result {
                    Ok(reply) => Ok(reply),
                    Err(error) => {
                        diagnostics::record("error","chat",Some(&id),None,&format!("{error:#}"));
                        let message = if error.downcast_ref::<turns::Interrupted>().is_some() { "Stopped by user. Recorded token usage was charged.".to_owned() } else { format!("AI request failed. Recorded usage was charged; retry shortly. Reference: {id}") };
                        if let Err(error)=task_app.db.settle(user,&id,Err(message.clone())) {
                            task_events.send_replace(ChatEvent::Error { task_credits:None, progress:None,message:"Could not confirm settlement. Retry the same request.".into(),settled:false,credits:None,steering:vec![] });
                            return Err(ApiError::from(error));
                        }
                        Err(ApiError::busy(&message))
                    }
                };
                let progress=match &*task_events.borrow() { ChatEvent::Progress { progress }=>Some(progress.clone()),_=>None };
                match &result {
                    Ok(reply) => {
                        match task_app.db.balance(user).and_then(|credits|Ok((credits,task_app.db.task_credits(user,request_id)?))) {
                            Ok((credits,task_credits))=> { task_events.send_replace(ChatEvent::Complete { response:ChatResponse { task_credits:Some(task_credits), progress:progress.clone(), reply:reply.reply.clone(),credits,revision:reply.revision,steering:reply.steering.clone() } }); }
                            Err(error)=> {
                                task_events.send_replace(ChatEvent::Error { task_credits:None, progress:None,message:"Could not read billing details. Retry the same request.".into(),settled:false,credits:None,steering:vec![] });
                                return Err(ApiError::from(error));
                            }
                        }
                    }
                    Err(error)=> {
                        match task_app.db.steering(user,input.request_id).and_then(|steering|Ok((steering,task_app.db.balance(user)?,task_app.db.task_credits(user,request_id)?))) {
                            Ok((steering,credits,task_credits))=> { task_events.send_replace(ChatEvent::Error { task_credits:Some(task_credits), progress:progress.clone(),message:error.1.clone(),settled:true,credits:Some(credits),steering }); }
                            Err(error)=> {
                                task_events.send_replace(ChatEvent::Error { task_credits:None, progress:None,message:"Could not recover settlement details. Retry the same request.".into(),settled:false,credits:None,steering:vec![] });
                                return Err(ApiError::from(error));
                            }
                        }
                    }
                }
                result
            });
            if streaming { return Ok(chat_stream(receiver)); }
            task.await??
        }
    };
    let response=ChatResponse { task_credits:Some(app.db.task_credits(user,request_id)?), progress:None, reply:reply.reply,credits:app.db.balance(user)?,revision:reply.revision,steering:reply.steering };
    if streaming {
        events.send_replace(ChatEvent::Complete { response });
        Ok(chat_stream(receiver))
    } else { Ok(Json(response).into_response()) }
}

/// Heartbeats keep idle proxy connections open; close the body after the terminal snapshot.
fn chat_stream(receiver: tokio::sync::watch::Receiver<ChatEvent>) -> Response {
    use axum::response::sse::{Event,KeepAlive,Sse};
    let stream=WatchStream::new(receiver).map(|event|Ok::<_,std::convert::Infallible>(Event::default().json_data(event).unwrap()));
    let mut response=Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))).into_response();
    response.headers_mut().insert("x-accel-buffering","no".parse().unwrap());
    response
}
