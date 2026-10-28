use std::{collections::HashMap,sync::Mutex};
use axum::{extract::{Path,State},http::HeaderMap,Json};
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

/// Steering addresses an application request owned by the signed-in user, never an arbitrary Codex turn.
pub async fn steer(State(app):State<App>,headers:HeaderMap,Path(id):Path<Uuid>,Json(input):Json<Steering>)->ApiResult<Json<serde_json::Value>> {
    let user=app.user(&headers)?;
    if input.text.trim().is_empty() || input.text.len()>protocol::MAX_MESSAGE_BYTES { return Err(ApiError::bad("Enter a message up to 16,000 bytes")); }
    let previous=app.db.steering(user,id)?;
    if let Some(found)=previous.iter().find(|found|found.id==input.id) {
        if found!=&input { return Err(ApiError::bad("Steering ID reused with different text")); }
        return Ok(Json(serde_json::json!({"ok":true})));
    }
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
