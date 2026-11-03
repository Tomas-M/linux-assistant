use std::{collections::{BTreeMap,VecDeque},sync::{Arc,RwLock}};
use anyhow::{ensure,Result};
use base64::Engine;
use axum::{extract::{Path,State},http::{header,StatusCode},response::{Html,IntoResponse,Response}};
use sha2::{Digest,Sha256};
use crate::local::Local;

struct Asset { mime:String,bytes:Vec<u8> }
struct Release { version:String,files:BTreeMap<String,Asset> }
#[derive(Default)]
pub struct Store { loading:tokio::sync::Mutex<()>,releases:RwLock<VecDeque<Arc<Release>>> }

impl Store {
    /// Validate and retain a complete release before any browser can load its files.
    pub async fn load(&self,backend:&str,live:&crate::stream::Live)->Result<()> {
        let _loading=self.loading.lock().await;
        let previous=self.releases.read().unwrap().back().map(|release|release.version.clone());
        let mut request=crate::backend::client(60)?.get(format!("{backend}/v1/client-ui?bridge={}",protocol::BRIDGE_API));
        if let Some(version)=&previous { request=request.header(header::IF_NONE_MATCH,format!("\"{version}\"")); }
        let mut response=crate::backend::send(request,live,None).await?;
        if response.status()==reqwest::StatusCode::NOT_MODIFIED { ensure!(previous.is_some(),"Backend returned an empty UI release"); return Ok(()); }
        ensure!(response.status().is_success(),"Cannot load the client interface (HTTP {}). Update the backend and publish its client UI",response.status());
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await? { ensure!(bytes.len()+chunk.len()<=protocol::MAX_UI_BYTES,"Client interface exceeds 8 MiB"); bytes.extend_from_slice(&chunk); }
        let bundle:protocol::UiBundle=serde_json::from_slice(&bytes)?;
        ensure!(bundle.bridge_api==protocol::BRIDGE_API,"This interface requires a native client update");
        ensure!(bundle.files.contains_key("/index.html"),"Client interface has no index page");
        let mut digest=Sha256::new();
        let mut files=BTreeMap::new();
        for (name,asset) in bundle.files {
            ensure!(name.starts_with('/') && !name.contains("..") && !name.contains(['\\','?','#']) && !name.chars().any(char::is_control),"Invalid UI asset path");
            axum::http::HeaderValue::from_str(&asset.content_type)?;
            digest.update(format!("{name}\0{}\0{}\0",asset.content_type,asset.content).as_bytes());
            files.insert(name,Asset { mime:asset.content_type,bytes:base64::engine::general_purpose::STANDARD.decode(asset.content)? });
        }
        ensure!(format!("{:x}",digest.finalize())==bundle.version,"Client interface checksum mismatch");
        let mut releases=self.releases.write().unwrap();
        if releases.back().is_some_and(|release|release.version==bundle.version) { return Ok(()); }
        releases.push_back(Arc::new(Release { version:bundle.version,files }));
        // Loaded pages keep their own scripts; retained releases cover assets requested by other open tabs.
        while releases.len()>8 { releases.pop_front(); }
        Ok(())
    }
}

/// Fetch on page load; an open page remains pinned to its existing scripts throughout the task.
pub(crate) async fn index(State(state):State<Local>)->Response {
    let backend=state.auth.backend.clone();
    if let Err(error)=state.ui.load(&backend,&state.live).await { return unavailable(&error.to_string()); }
    let release=state.ui.releases.read().unwrap().back().cloned().unwrap();
    let Some(asset)=release.files.get("/index.html") else { return unavailable("The client interface has no index page"); };
    let mut html=match String::from_utf8(asset.bytes.clone()) { Ok(html)=>html,Err(error)=>return unavailable(&error.to_string()) };
    let brand=state.brand.metadata();
    let name=brand["name"].as_str().unwrap_or("Assistant").replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('\"',"&quot;").replace('\'',"&#39;");
    html=html.replace("{{APP_NAME}}",&name).replace("{{DEFAULT_THEME}}",brand["default_theme"].as_str().unwrap_or("light")).replace("{{SIGN_IN}}",&state.brand.providers().to_string().replace('<',"\\u003c"));
    for name in release.files.keys().filter(|name|name.as_str()!="/index.html") {
        html=html.replace(&format!("\"{name}\""),&format!("\"/ui/{}{name}\"",release.version));
    }
    Html(html).into_response()
}

/// Versioned URLs never serve a different release's asset, even during concurrent page loads.
pub(crate) async fn asset(State(state):State<Local>,Path((version,path)):Path<(String,String)>)->Response {
    let releases=state.ui.releases.read().unwrap();
    let Some(release)=releases.iter().find(|release|release.version==version) else { return StatusCode::NOT_FOUND.into_response(); };
    let Some(asset)=release.files.get(&format!("/{path}")) else { return StatusCode::NOT_FOUND.into_response(); };
    ([(header::CONTENT_TYPE,asset.mime.as_str())],asset.bytes.clone()).into_response()
}

/// This small native error page is used only when the backend cannot deliver the real interface.
fn unavailable(message:&str)->Response {
    let message=message.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;");
    (StatusCode::SERVICE_UNAVAILABLE,Html(format!(r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Assistant unavailable</title><h1>Cannot connect to your assistant</h1><p>{message}</p><p><a href="/">Reconnect</a></p></html>"#))).into_response()
}
