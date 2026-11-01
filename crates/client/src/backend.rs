use std::{sync::atomic::{AtomicBool,Ordering},time::{Duration,SystemTime,UNIX_EPOCH}};
use anyhow::{bail,ensure,Result};
use serde_json::json;
use crate::stream::Live;
#[cfg(test)]
mod tests;

/// Bound connection attempts separately from long-running streamed responses.
pub fn client(timeout:u64)->Result<reqwest::Client> {
    let mut builder=reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(45)).redirect(reqwest::redirect::Policy::none());
    if timeout>0 { builder=builder.timeout(Duration::from_secs(timeout)); }
    Ok(builder.build()?)
}

// Carry the rejected session internally; formatting must never reveal its bearer token.
#[derive(Clone)]
pub struct SessionExpired(pub String);
impl std::fmt::Debug for SessionExpired {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("SessionExpired") }
}
impl std::fmt::Display for SessionExpired {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("Session expired; sign in again") }
}
impl std::error::Error for SessionExpired {}

struct Connecting<'a> { live:&'a Live,id:String }
impl Drop for Connecting<'_> {
    /// Clear only this request's indicator, including when its future is cancelled.
    fn drop(&mut self) { self.live.0.send_if_modified(|view|view["connections"].as_object_mut().unwrap().remove(&self.id).is_some()); }
}

/// Retry failed connections and safe gateway failures; never replay an ambiguous login or payment POST.
pub async fn send(request:reqwest::RequestBuilder,live:&Live,stopped:Option<&AtomicBool>)->Result<reqwest::Response> {
    let metadata=request.try_clone().unwrap().build()?;
    let replayable=metadata.method()==reqwest::Method::GET || metadata.url().path().starts_with("/v1/chat");
    let connecting=Connecting { live,id:uuid::Uuid::new_v4().to_string() };
    for attempt in 1..=5 {
        ensure!(!stopped.is_some_and(|stop|stop.load(Ordering::SeqCst)),"Stopped by you");
        if attempt>1 { live.0.send_modify(|view|view["connections"][&connecting.id]=json!({"attempt":attempt,"retry_at":null})); }
        match request.try_clone().unwrap().send().await {
            Ok(response)=> {
                // Logout already accepts a revoked session; other authenticated rejections require sign-in.
                if response.status()==reqwest::StatusCode::UNAUTHORIZED && metadata.url().path()!="/v1/logout" {
                    if let Some(token)=metadata.headers().get(reqwest::header::AUTHORIZATION).and_then(|value|value.to_str().ok()).and_then(|value|value.strip_prefix("Bearer ")) {
                        return Err(SessionExpired(token.into()).into());
                    }
                }
                let gateway=matches!(response.status().as_u16(),502|503|504);
                let application_error=response.status().as_u16()==503 && response.headers().get(reqwest::header::CONTENT_TYPE).is_some_and(|value|value.as_bytes().starts_with(b"application/json"));
                if !replayable || !gateway || application_error { return Ok(response); }
            }
            Err(error) if error.is_connect()=> {},
            Err(error)=>return Err(error.into()),
        }
        ensure!(!stopped.is_some_and(|stop|stop.load(Ordering::SeqCst)),"Stopped by you");
        if attempt==5 { bail!("Cannot connect to the backend after 5 attempts. Please try again."); }
        let retry_at=SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64+5000;
        live.0.send_modify(|view|view["connections"][&connecting.id]=json!({"attempt":attempt+1,"retry_at":retry_at}));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    unreachable!()
}
