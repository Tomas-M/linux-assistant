use std::{collections::HashMap,fs::File,io::{Read,Seek},path::{Path,PathBuf},sync::Mutex};
use anyhow::{Context,Result,ensure};
use serde_json::{json,Value};
use uuid::Uuid;

#[derive(Default)]
pub struct Downloads(Mutex<HashMap<PathBuf,Uuid>>);

impl Downloads {
    /// Resolve only existing regular files; URLs contain opaque IDs rather than filesystem paths.
    pub fn resolve(&self,paths:Vec<String>,home:&Path)->Result<Value> {
        ensure!(paths.len()<=16,"Resolve at most 16 paths per request");
        let mut files=Vec::new();
        for source in paths {
            if source.len()>4096 || source.chars().any(char::is_control) { continue; }
            let path=if let Some(relative)=source.strip_prefix("~/").or_else(||source.strip_prefix("~\\")) { home.join(relative) } else { home.join(&source) };
            #[cfg(windows)]
            if !matches!(path.components().next(),Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(),std::path::Prefix::Disk(_))) { continue; }
            let Ok(path)=path.canonicalize() else { continue; };
            if crate::files::open_file(&path).is_err() { continue; }
            let name=path.file_name().unwrap().to_string_lossy().into_owned();
            let full_path=path.to_string_lossy().into_owned();
            #[cfg(windows)]
            let full_path=full_path.strip_prefix(r"\\?\").unwrap_or(&full_path).to_owned();
            let id=*self.0.lock().unwrap().entry(path).or_insert_with(Uuid::new_v4);
            files.push(json!({"path":source,"full_path":full_path,"name":name,"url":format!("/api/download/{id}")}));
        }
        Ok(json!({"files":files}))
    }

    /// Recheck availability and select a browser preview type without loading the entire file.
    pub fn open(&self,id:Uuid)->Result<(File,String,String)> {
        let path=self.0.lock().unwrap().iter().find_map(|(path,key)|(*key==id).then(||path.clone())).context("Unknown file link")?;
        let mut file=crate::files::open_file(&path)?;
        let name=path.file_name().unwrap().to_string_lossy().into_owned();
        let mut mime=mime_guess::from_path(&path).first_or_octet_stream().essence_str().to_owned();
        if !mime.starts_with("image/") && !matches!(mime.as_str(),"application/pdf"|"text/html"|"application/xhtml+xml") {
            let mut sample=[0;8192]; let count=file.read(&mut sample)?; file.rewind()?;
            let utf8=std::str::from_utf8(&sample[..count]).map_or_else(|error|error.error_len().is_none(),|_|true);
            let text=utf8 && sample[..count].iter().all(|byte|!byte.is_ascii_control() || matches!(*byte,b'\t'|b'\n'|b'\r'));
            mime=if text { "text/plain; charset=utf-8" } else { "application/octet-stream" }.into();
        }
        Ok((file,name,mime))
    }
}

/// Preview files inline; isolate active document formats from the assistant's local API.
pub async fn response(downloads:std::sync::Arc<Downloads>,id:Uuid)->axum::response::Response {
    use axum::{body::Body,http::{header,StatusCode},response::IntoResponse};
    use tokio::io::AsyncReadExt;
    let opened=tokio::task::spawn_blocking(move||downloads.open(id)).await;
    let Ok(Ok((file,name,mime)))=opened else { return (StatusCode::NOT_FOUND,"This file is no longer available.").into_response(); };
    let size=match file.metadata() { Ok(metadata)=>metadata.len(),Err(_)=>return StatusCode::NOT_FOUND.into_response() };
    let filename=name.bytes().map(|byte|if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) { (byte as char).to_string() } else { format!("%{byte:02X}") }).collect::<String>();
    let stream=tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file).take(size));
    let document=matches!(mime.as_str(),"text/html"|"application/xhtml+xml"|"image/svg+xml");
    let mut response=([(header::CONTENT_TYPE,mime),(header::CONTENT_LENGTH,size.to_string()),
        (header::CONTENT_DISPOSITION,format!("inline; filename=\"file\"; filename*=UTF-8''{filename}"))],Body::from_stream(stream)).into_response();
    if document {
        response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,"sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    }
    response
}
