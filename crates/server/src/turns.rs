use std::{collections::HashMap,sync::Mutex};
use axum::{extract::{Path,State},http::{HeaderMap,StatusCode},Json};
use tokio::sync::{mpsc,oneshot};
use uuid::Uuid;
use protocol::Steering;
use crate::{App,ApiError,ApiResult};

pub type Active=Mutex<HashMap<(i64,Uuid),ActiveTurn>>;
#[derive(Clone)]
pub struct ActiveTurn { pub sender:mpsc::Sender<Control>,pub cancel:tokio::sync::watch::Sender<bool> }
pub enum Control {
    Steer(Steering,oneshot::Sender<Result<(),String>>),
    Stop(oneshot::Sender<Result<(),String>>),
}

#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    /// Distinguish an explicit interruption from a provider failure or a quota error.
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { write!(f,"Stopped by user") }
}
impl std::error::Error for Interrupted {}

/// Frame live and recovered steering identically while keeping the stored user text unchanged.
pub fn steering_message(input:Steering)->protocol::Message {
    protocol::Message { role:protocol::Role::User,attachments:input.attachments,
        text:format!("Additional user instructions for the task currently in progress. Supplement the existing objective and keep unfinished requirements; replace or cancel earlier work only when explicitly requested.\n\nUser message:\n{}",input.text) }
}

/// Steering addresses an application request owned by the signed-in user, never an arbitrary Codex turn.
pub async fn steer(State(app):State<App>,headers:HeaderMap,Path(id):Path<Uuid>,Json(input):Json<Steering>)->ApiResult<Json<serde_json::Value>> {
    let user=app.user(&headers)?;
    protocol::validate_input(&input.text,input.attachments.as_deref()).map_err(|error|ApiError::bad(&error.to_string()))?;
    let previous=app.db.steering(user,id)?;
    if let Some(found)=previous.iter().find(|found|found.id==input.id) {
        if found!=&input { return Err(ApiError::bad("Steering ID reused with different text")); }
        return Ok(Json(serde_json::json!({"ok":true})));
    }
    // Replayed acknowledgements remain valid, but adding instructions requires a positive balance.
    if app.db.balance(user)?<=0 { return Err(ApiError(StatusCode::PAYMENT_REQUIRED,"Add credits to steer the conversation".into())); }
    let sender=app.turns.lock().unwrap().get(&(user,id)).cloned().ok_or_else(||ApiError::bad("This AI turn is no longer running"))?;
    let (reply,result)=oneshot::channel();
    sender.sender.send(Control::Steer(input,reply)).await.map_err(|_|ApiError::bad("This AI turn has finished"))?;
    result.await.map_err(|_|ApiError::bad("Steering was not confirmed; retry the same message"))?.map_err(|error|ApiError::bad(&error))?;
    Ok(Json(serde_json::json!({"ok":true})))
}

/// Interrupt the user's current generation; completion remains responsible for settlement.
pub async fn stop(State(app):State<App>,headers:HeaderMap,Path(id):Path<Uuid>)->ApiResult<Json<serde_json::Value>> {
    let user=app.user(&headers)?;
    let sender=app.turns.lock().unwrap().get(&(user,id)).cloned();
    if let Some(sender)=sender {
        let (reply,result)=oneshot::channel();
        let interrupt=async { if sender.sender.send(Control::Stop(reply)).await.is_ok() { let _=result.await; } };
        if tokio::time::timeout(std::time::Duration::from_secs(3),interrupt).await.is_err() { sender.cancel.send_replace(true); }
    }
    Ok(Json(serde_json::json!({"ok":true})))
}
