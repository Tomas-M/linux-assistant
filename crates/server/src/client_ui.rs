use std::time::SystemTime;
use axum::{body::Bytes,extract::{Query,State},http::{header,HeaderMap,StatusCode},response::{Html,IntoResponse,Response}};
use serde::Deserialize;
use crate::{App,ApiError,ApiResult};

static RELEASE:tokio::sync::Mutex<Option<(SystemTime,u64,Bytes,String)>>=tokio::sync::Mutex::const_new(None);
#[derive(Deserialize)]
pub struct Version { bridge:u32 }

/// Public visitors receive instructions; the functional UI is served only through the local application.
pub async fn landing(State(app):State<App>)->Html<String> {
    let name=app.config().config.client.name.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;");
    Html(format!("<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{name}</title><h1>{name}</h1><p>Open this service through the Linux Assistant application in Slax to use your assistant and work on your computer.</p></html>"))
}

/// Read an atomically published UI release without rebuilding or restarting the backend.
pub async fn bundle(Query(version):Query<Version>,headers:HeaderMap)->ApiResult<Response> {
    if version.bridge!=protocol::BRIDGE_API { return Ok((StatusCode::UPGRADE_REQUIRED,"Update the native assistant application").into_response()); }
    let path=std::env::current_exe()?.with_file_name("client-ui.json");
    let metadata=tokio::fs::metadata(&path).await.map_err(|error| { crate::diagnostics::record("error","client_ui",None,None,&format!("Cannot open {}: {error}",path.display())); ApiError::busy("The client interface is unavailable. Ask the administrator to publish the client UI.") })?;
    if metadata.len()>protocol::MAX_UI_BYTES as u64 { return Err(ApiError::busy("The published client interface is too large")); }
    let modified=metadata.modified()?;
    let mut cache=RELEASE.lock().await;
    let changed=cache.as_ref().is_none_or(|(time,size,_,_)|*time!=modified || *size!=metadata.len());
    if changed {
        let bytes=tokio::fs::read(&path).await?;
        let document:protocol::UiBundle=serde_json::from_slice(&bytes)?;
        if document.bridge_api!=protocol::BRIDGE_API { return Err(ApiError::busy("Published UI uses a different native bridge version")); }
        let tag=format!("\"{}\"",document.version);
        *cache=Some((modified,metadata.len(),Bytes::from(bytes),tag));
    }
    let (_,_,bytes,tag)=cache.as_ref().unwrap();
    if headers.get(header::IF_NONE_MATCH).is_some_and(|value|value.as_bytes()==tag.as_bytes()) { return Ok(StatusCode::NOT_MODIFIED.into_response()); }
    Ok(([(header::CONTENT_TYPE,"application/json"),(header::CACHE_CONTROL,"no-store"),(header::ETAG,tag.as_str())],bytes.clone()).into_response())
}
