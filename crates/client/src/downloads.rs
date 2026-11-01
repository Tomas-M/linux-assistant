use std::{collections::HashMap,fs::File,path::{Path,PathBuf},sync::Mutex};
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
            let id=*self.0.lock().unwrap().entry(path).or_insert_with(Uuid::new_v4);
            files.push(json!({"path":source,"name":name,"url":format!("/api/download/{id}")}));
        }
        Ok(json!({"files":files}))
    }

    /// Recheck availability at download time; directories, devices and pipes are never served.
    pub fn open(&self,id:Uuid)->Result<(File,String)> {
        let path=self.0.lock().unwrap().iter().find_map(|(path,key)|(*key==id).then(||path.clone())).context("Unknown file link")?;
        let file=crate::files::open_file(&path)?;
        let name=path.file_name().unwrap().to_string_lossy().into_owned();
        Ok((file,name))
    }
}

/// Force a download, including HTML and executables, and preserve international filenames safely.
pub async fn response(downloads:std::sync::Arc<Downloads>,id:Uuid)->axum::response::Response {
    use axum::{body::Body,http::{header,StatusCode},response::IntoResponse};
    use tokio::io::AsyncReadExt;
    let opened=tokio::task::spawn_blocking(move||downloads.open(id)).await;
    let Ok(Ok((file,name)))=opened else { return (StatusCode::NOT_FOUND,"This file is no longer available.").into_response(); };
    let size=match file.metadata() { Ok(metadata)=>metadata.len(),Err(_)=>return StatusCode::NOT_FOUND.into_response() };
    let filename=name.bytes().map(|byte|if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) { (byte as char).to_string() } else { format!("%{byte:02X}") }).collect::<String>();
    let stream=tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file).take(size));
    ([(header::CONTENT_TYPE,"application/octet-stream".into()),(header::CONTENT_LENGTH,size.to_string()),
        (header::CONTENT_DISPOSITION,format!("attachment; filename=\"download\"; filename*=UTF-8''{filename}"))],Body::from_stream(stream)).into_response()
}
