// Match the embedded App Server's limit for deeply nested async types.
#![recursion_limit = "256"]

mod auth;
mod admin;
mod config;
mod db;
mod embedded;
mod mail;
mod payments;
mod provider;
mod usage;
mod turns;
#[cfg(test)]
mod tests;

use std::{fs::OpenOptions, sync::Arc, time::Duration};
use axum::{body::Body, extract::{DefaultBodyLimit, State}, http::{header, HeaderMap, Request, StatusCode}, middleware::{self, Next}, response::{Html, IntoResponse, Response}, routing::{get, post}, Json, Router};
use clap::Parser;
use fs2::FileExt;
use protocol::{Account, ChatEvent, ChatRequest, ChatResponse, MAX_REQUEST_BYTES, MAX_MESSAGE_BYTES};
use tokio_stream::{wrappers::WatchStream, StreamExt};
use serde_json::{json, Value};
use crate::{config::Config, db::{hash, Db, Reservation}};

#[derive(Clone)]
pub struct App { turns: Arc<turns::Active>, config: Arc<Config>, db: Arc<Db>, http: reqwest::Client, pool: Arc<provider::Pool>, mail: Option<lettre::AsyncSmtpTransport<lettre::Tokio1Executor>> }
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
    fn from(error: E) -> Self { eprintln!("Backend error: {}",error.into()); Self(StatusCode::INTERNAL_SERVER_ERROR,"Backend operation failed".into()) }
}
impl IntoResponse for ApiError {
    /// Every API failure has the same machine-readable shape.
    fn into_response(self) -> Response { (self.0,Json(json!({"error":self.1}))).into_response() }
}

impl App {
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
    run()
}

/// Run one backend process per SQLite database so startup recovery is unambiguous.
#[tokio::main]
async fn run() -> anyhow::Result<()> {
    let mut config = Config::parse();
    if config.codex_version { println!("Codex App Server {}",env!("CODEX_VERSION")); return Ok(()); }
    if let Some(home) = config.login_account { return provider::login(home).await; }
    if config.third_party_notices {
        println!("{}\n{}",include_str!("../../../vendor/codex/LICENSE"),include_str!("../../../vendor/codex/NOTICE"));
        return Ok(());
    }
    config.public_url = config.public_url.trim_end_matches('/').to_owned();
    config.validate()?;
    let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(config.database.with_extension("lockfile"))?;
    lock.try_lock_exclusive().map_err(|_|anyhow::anyhow!("Another backend is using this database"))?;
    let db = Arc::new(Db::open(&config.database)?);
    db.recover()?;
    let mail = if config.dev { None } else { Some(mail::transport(&config)?.build()) };
    let app = App { turns:Arc::new(Default::default()),pool:Arc::new(provider::Pool::new(&config)?),config:Arc::new(config),db,mail,
        http:reqwest::Client::builder().timeout(Duration::from_secs(30)).redirect(reqwest::redirect::Policy::none()).build()? };
    let listener = tokio::net::TcpListener::bind(app.config.listen).await?;
    println!("Linux Assistant backend on {}{}",listener.local_addr()?,if app.config.dev { " (DEVELOPMENT, simulated AI)" } else { "" });
    if app.config.admin_password.is_some() && !app.config.dev { tokio::spawn(app.pool.clone().monitor(app.db.clone())); }
    axum::serve(listener,router(app)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}

/// Register hosted authentication and authenticated application APIs in one server.
fn router(app: App) -> Router {
    Router::new().merge(admin::router(app.clone())).route("/health",get(||async { Json(json!({"ok":true})) }))
        .route("/v1/auth/device",post(auth::start)).route("/v1/auth/poll",post(auth::poll))
        .route("/v1/auth/email",post(auth::email)).route("/v1/auth/verify",post(auth::verify))
        .route("/auth/google/{device}",get(auth::google)).route("/auth/google/callback",get(auth::callback))
        .route("/v1/me",get(me)).route("/v1/logout",post(logout)).route("/v1/chat",post(chat))
        .route("/v1/chat/{id}/steer",post(turns::steer)).route("/v1/chat/{id}/stop",post(turns::stop))
        .route("/v1/checkout",post(payments::checkout)).route("/webhooks/stripe",post(payments::webhook))
        .route("/payment/success",get(||async { Html(auth::document("<h1>Payment submitted</h1><p>Credits appear after the payment is confirmed. Return to Linux Assistant and refresh your balance.</p>")) }))
        .route("/payment/cancel",get(||async { Html(auth::document("<h1>Checkout cancelled</h1><p>Return to Linux Assistant to continue.</p>")) }))
        .layer(DefaultBodyLimit::max(256*1024)).layer(middleware::from_fn_with_state(app.clone(),browser_security)).with_state(app)
}

/// Public bearer-token APIs allow any origin; cookie-based admin and OAuth routes stay same-origin.
async fn browser_security(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    let admin=request.uri().path().starts_with("/admin");
    let public=request.uri().path().starts_with("/v1/") || request.uri().path()=="/health";
    if !public {
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            if origin.to_str().ok() != Some(app.config.public_url.as_str()) { return StatusCode::FORBIDDEN.into_response(); }
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
        else { "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'" };
    response.headers_mut().insert("Content-Security-Policy",policy.parse().unwrap());
    response
}

/// Return an authoritative balance; the client never computes or deducts credits.
async fn me(State(app): State<App>, headers: HeaderMap) -> ApiResult<Json<Account>> {
    let user = app.user(&headers)?;
    let email = app.db.0.lock().unwrap().query_row("SELECT email FROM users WHERE id=?1",[user],|r|r.get(0))?;
    Ok(Json(Account { email,credits:app.db.balance(user)?,development:app.config.dev }))
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
    let user = app.user(&headers)?;
    let bytes = serde_json::to_vec(&input)?;
    if input.messages.is_empty() || bytes.len()>MAX_REQUEST_BYTES || input.cwd.len()>4096 {
        return Err(ApiError::bad("New input is empty or exceeds the per-request limit"));
    }
    if input.messages.iter().any(|m|m.text.len()>64_000) || input.messages.last().is_some_and(|m|m.role==protocol::Role::User && m.text.len()>MAX_MESSAGE_BYTES) {
        return Err(ApiError::bad("Message is too long"));
    }
    let id = input.request_id.to_string();
    let reservation = app.db.reserve(user,&id,&hash(std::str::from_utf8(&bytes).unwrap()),&input.conversation_id.to_string())?;
    let streaming=headers.get(header::ACCEPT).is_some_and(|value|value=="text/event-stream");
    let (events,receiver)=tokio::sync::watch::channel(ChatEvent::Progress { progress:Default::default() });
    let reply: db::Completed = match reservation {
        Reservation::Cached(value) => serde_json::from_str(&value)?,
        Reservation::Failed(error) => {
            if !streaming { return Err(ApiError::busy(&error)); }
            events.send_replace(ChatEvent::Error { message:error,refunded:true,steering:app.db.steering(user,input.request_id)? });
            return Ok(chat_stream(receiver));
        },
        Reservation::Pending => return Err(ApiError(StatusCode::CONFLICT,"Request is still running; retry the same request shortly".into())),
        Reservation::Insufficient => return Err(ApiError(StatusCode::PAYMENT_REQUIRED,"No application credits remaining".into())),
        Reservation::New => {
            let history = app.db.conversation(user,input.conversation_id)?.messages;
            if input.base!=history.len() {
                app.db.settle(user,&id,Err("Conversation revision changed".into()))?;
                return Err(ApiError::bad("Conversation revision changed; start a new chat after a server reset"));
            }
            let (controls,mut control_receiver)=tokio::sync::mpsc::channel(16);
            let (cancel,mut cancellation)=tokio::sync::watch::channel(false);
            app.turns.lock().unwrap().insert((user,input.request_id),turns::ActiveTurn { sender:controls,cancel });
            let task_app = app.clone();
            let task_events=events.clone();
            let task=tokio::spawn(async move {
                let result = tokio::select! {
                    answer=task_app.pool.answer_stream(&task_app.config,&task_app.db,user,&input,streaming.then_some(&task_events),Some(&mut control_receiver))=>answer,
                    _=cancellation.changed()=>Err(anyhow::Error::from(turns::Interrupted)),
                }.and_then(|answer| {
                    task_app.db.complete(user,&input,&answer.reply,answer.account_home.as_deref(),answer.thread_id.as_deref(),answer.checkpoint.as_deref())
                });
                task_app.turns.lock().unwrap().remove(&(user,input.request_id));
                let result=match result {
                    Ok(reply) => Ok(reply),
                    Err(error) => {
                        eprintln!("AI request failed: {error}");
                        let message = if error.downcast_ref::<turns::Interrupted>().is_some() { "Stopped by user. This request was refunded." } else { "AI is unavailable or at its usage limit. This request was refunded; retry shortly." };
                        if let Err(error)=task_app.db.settle(user,&id,Err(message.into())) {
                            task_events.send_replace(ChatEvent::Error { message:"Could not confirm settlement. Retry the same request.".into(),refunded:false,steering:vec![] });
                            return Err(ApiError::from(error));
                        }
                        Err(ApiError::busy(message))
                    }
                };
                match &result {
                    Ok(reply) => {
                        match task_app.db.balance(user) {
                            Ok(credits)=> { task_events.send_replace(ChatEvent::Complete { response:ChatResponse { reply:reply.reply.clone(),credits,revision:reply.revision,steering:reply.steering.clone() } }); }
                            Err(error)=> {
                                task_events.send_replace(ChatEvent::Error { message:"Could not read the balance. Retry the same request.".into(),refunded:false,steering:vec![] });
                                return Err(ApiError::from(error));
                            }
                        }
                    }
                    Err(error)=> {
                        match task_app.db.steering(user,input.request_id) {
                            Ok(steering)=> { task_events.send_replace(ChatEvent::Error { message:error.1.clone(),refunded:true,steering }); }
                            Err(error)=> {
                                task_events.send_replace(ChatEvent::Error { message:"Could not recover accepted steering. Retry the same request.".into(),refunded:false,steering:vec![] });
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
    let response=ChatResponse { reply:reply.reply,credits:app.db.balance(user)?,revision:reply.revision,steering:reply.steering };
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
