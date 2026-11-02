use std::collections::BTreeMap;
use base64::Engine;
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use crate::{config::FieldError,App,ApiResult};

#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    pub name:String,pub default_theme:protocol::Theme,
    pub assistant_image:Option<String>,pub logo_image:Option<String>,pub logo_full:Option<String>,pub favicon:Option<String>,
}
impl Default for Appearance {
    /// Keep the supplied assistant available on a fresh backend; all images are editable and included in backups.
    fn default()->Self {
        Self { name:"Slax Assistant".into(),default_theme:protocol::Theme::Light,
            assistant_image:Some(format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(include_bytes!("../assets/assistant.png")))),
            logo_image:None,logo_full:None,favicon:None }
    }
}
impl Appearance {
    /// Pair public asset names with their persisted base64 image data.
    pub fn images(&self)->[(&'static str,&Option<String>);4] {
        [("assistant-image",&self.assistant_image),("logo-image",&self.logo_image),("logo-full",&self.logo_full),("favicon",&self.favicon)]
    }
    /// Return field-specific errors without including image contents in diagnostics.
    pub fn validate(&self)->Result<(),FieldError> {
        if self.name.trim().is_empty() || self.name.len()>100 || self.name.chars().any(char::is_control) { return Err(FieldError::new("client.name","Enter an assistant name of 1 to 100 UTF-8 bytes")); }
        for (key,image) in self.images() {
            if let Some(image)=image { protocol::decode_brand_image(image,key=="favicon").map_err(|error|FieldError::new(&format!("client.{}",key.replace('-',"_")),&error.to_string()))?; }
        }
        Ok(())
    }
    /// Decode once per immutable settings snapshot, keeping large images out of manifest and chat updates.
    pub fn assets(&self)->anyhow::Result<BTreeMap<String,(&'static str,Vec<u8>,String)>> {
        self.images().into_iter().filter_map(|(key,image)|image.as_ref().map(|image|(key,image))).map(|(key,image)| {
            let (mime,bytes)=protocol::decode_brand_image(image,key=="favicon")?;
            let hash=format!("{:x}",Sha256::digest(&bytes));
            Ok((key.into(),(mime,bytes,hash)))
        }).collect()
    }
}

/// The client needs branding before any user session has been created.
pub async fn manifest(axum::extract::State(app):axum::extract::State<App>)->axum::Json<protocol::Branding> { axum::Json(app.config().branding.clone()) }

/// Public immutable URLs let all clients cache the same images without including base64 in every manifest.
pub async fn asset(axum::extract::State(app):axum::extract::State<App>,axum::extract::Path(key):axum::extract::Path<String>,axum::extract::Query(query):axum::extract::Query<BTreeMap<String,String>>)->ApiResult<axum::response::Response> {
    use axum::{http::{header,StatusCode},response::IntoResponse};
    let config=app.config();
    let Some((mime,bytes,hash))=config.images.get(&key) else { return Ok(StatusCode::NOT_FOUND.into_response()); };
    if query.get("v")!=Some(hash) { return Ok(StatusCode::NOT_FOUND.into_response()); }
    Ok(([(header::CONTENT_TYPE,*mime),(header::ETAG,format!("\"{hash}\"").as_str()),(header::CACHE_CONTROL,"public, max-age=31536000, immutable")],bytes.clone()).into_response())
}
