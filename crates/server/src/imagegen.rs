use std::{future::Future,io::Write,path::{Path,PathBuf},pin::Pin};
use anyhow::{ensure,Context,Result};
use axum::{extract::{Path as UrlPath,State},http::{header,HeaderMap,StatusCode},response::{IntoResponse,Response}};
use base64::{engine::general_purpose::STANDARD,Engine};
use rusqlite::{params,OptionalExtension};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{config::Config,db::{Db,now},usage::Attempt,App,ApiResult};

pub const MAX_BYTES:usize=32*1024*1024;
pub type Generation=Pin<Box<dyn Future<Output=Result<Value>>+Send>>;

/// GPT Image 2's published high-quality 1024-square output estimate: $0.211, before task rounding.
pub fn default_rate()->crate::billing::Rate { crate::billing::Rate(21_100_000) }

/// API accounts use the public image endpoint; subscription accounts retain the upstream native tool.
pub fn api_key(attempt:&Attempt<'_>)->Result<Option<String>> {
    let home:String=attempt.db.0.lock().unwrap().query_row("SELECT account_home FROM usage WHERE id=?1",[attempt.id],|row|row.get(0))?;
    let auth:Value=serde_json::from_slice(&std::fs::read(Path::new(&home).join("auth.json"))?)?;
    Ok(auth["OPENAI_API_KEY"].as_str().filter(|key|!key.is_empty()).map(str::to_owned))
}

/// Keep generation arguments small and refer to existing inputs without repeating their encoded pixels.
pub fn spec()->Value {
    json!({"name":"generate_image","description":"Generate or edit one image using GPT Image 2. Use input_image_ids from image_inputs to edit uploaded or previously generated images; use an empty list for a new image. Images are displayed automatically. Do not repeat a failed request without correcting its cause.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["prompt","transparent_background","input_image_ids"],"properties":{
            "prompt":{"type":"string"},"transparent_background":{"type":"boolean"},"input_image_ids":{"type":"array","items":{"type":"string"},"maxItems":5}}}})
}

/// Image identifiers are scoped to this user's conversation; arbitrary server paths and URLs are never inputs.
pub fn inputs(db:&Db,user:i64,request:&protocol::ChatRequest)->Result<Value> {
    let mut images=Vec::new();
    for message in &request.messages {
        for file in message.attachments.as_deref().unwrap_or_default() {
            if let protocol::Attachment::Image { name,.. }=file { images.push(json!({"id":format!("upload-{}",images.len()),"name":name})); }
        }
    }
    let conn=db.0.lock().unwrap();
    let mut query=conn.prepare("SELECT i.id FROM image_generations i JOIN usage u ON u.id=i.usage_id WHERE u.user_id=?1 AND u.conversation_id=?2 ORDER BY i.created_at DESC LIMIT 20")?;
    for id in query.query_map(params![user,request.conversation_id.to_string()],|row|row.get::<_,String>(0))? {
        images.push(json!({"id":id?,"name":"Previously generated image"}));
    }
    Ok(json!(images))
}

/// Cache originals outside SQLite; immutable billing rows survive expiry of their temporary image files.
fn directory(config:&Config)->PathBuf { config.database.parent().unwrap().join("generated-images") }

/// Save and charge each exact tool call once, even if its completion notification is replayed.
pub async fn save(config:&Config,attempt:&Attempt<'_>,call:&str,bytes:Vec<u8>)->Result<Value> {
    ensure!(bytes.len()<=MAX_BYTES && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),"Image generation returned an invalid or oversized PNG");
    let previous:Option<String>=attempt.db.0.lock().unwrap().query_row("SELECT id FROM image_generations WHERE usage_id=?1 AND call_id=?2",params![attempt.id,call],|row|row.get(0)).optional()?;
    let id=previous.unwrap_or_else(||Uuid::new_v4().to_string());
    let root=directory(config); let path=root.join(format!("{id}.png"));
    tokio::task::spawn_blocking(move||->Result<()> {
        std::fs::create_dir_all(&root)?;
        if !path.exists() {
            let mut staged=tempfile::NamedTempFile::new_in(root)?;
            staged.write_all(&bytes)?; staged.as_file().sync_all()?; staged.persist(path)?;
        }
        Ok(())
    }).await??;
    let mut conn=attempt.db.0.lock().unwrap(); let tx=conn.transaction()?;
    tx.execute("INSERT OR IGNORE INTO image_generations(id,usage_id,call_id,cost,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,attempt.id,call,config.image_credits_per_image.0,now()])?;
    let owner:Option<(i64,String)>=tx.query_row("SELECT user_id,request_id FROM usage WHERE id=?1 AND user_id IS NOT NULL",[attempt.id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    if let Some((user,request))=owner { crate::billing::settle(&tx,user,&request,false,false)?; }
    tx.commit()?;
    Ok(json!({"id":id,"name":format!("generated-{id}.png")}))
}

/// Native events may contain a file-store reference instead of pixels; read only the extension's output folder.
pub async fn native(config:&Config,attempt:&Attempt<'_>,event:&mut Value)->Result<()> {
    if event["method"]!="item/completed" || event["params"]["item"]["type"]!="imageGeneration" || event["params"]["item"]["status"]!="completed" { return Ok(()); }
    let item=&event["params"]["item"]; let call=item["id"].as_str().context("Generated image has no call ID")?;
    let bytes=if let Some(path)=item["savedPath"].as_str() {
        let home:String=attempt.db.0.lock().unwrap().query_row("SELECT account_home FROM usage WHERE id=?1",[attempt.id],|row|row.get(0))?;
        let path=tokio::fs::canonicalize(path).await?;
        let root=tokio::fs::canonicalize(Path::new(&home).join("generated_images")).await?;
        ensure!(path.starts_with(root) && tokio::fs::metadata(&path).await?.len()<=MAX_BYTES as u64,"Invalid generated image location or size");
        tokio::fs::read(path).await?
    } else {
        let encoded=item["result"].as_str().context("Generated image has no downloadable pixels")?;
        ensure!(encoded.len()<=MAX_BYTES.div_ceil(3)*4,"Generated image exceeds 32 MiB");
        STANDARD.decode(encoded)?
    };
    let image=save(config,attempt,call,bytes).await?;
    event["params"]["item"]["assistantImage"]=image;
    // Do not let upstream's inline result escape into SSE snapshots, logs or replay responses.
    event["params"]["item"].as_object_mut().unwrap().remove("result");
    Ok(())
}

/// API image requests run beside the turn loop so Stop, questions and steering remain responsive.
pub fn generate(key:String,arguments:&Value,config:&Config,db:&Db,user:i64,request:&protocol::ChatRequest)->Result<Generation> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input { prompt:String,transparent_background:bool,input_image_ids:Vec<String> }
    let input:Input=serde_json::from_value(arguments.clone())?;
    ensure!(!input.prompt.trim().is_empty() && input.prompt.len()<=32_000 && input.input_image_ids.len()<=5,"Invalid image generation arguments");
    let mut images=Vec::new();
    let uploads=request.messages.iter().flat_map(|message|message.attachments.as_deref().unwrap_or_default()).filter_map(|file|match file { protocol::Attachment::Image { content,.. }=>Some(content),_=>None }).collect::<Vec<_>>();
    for id in &input.input_image_ids {
        let content=if let Some(index)=id.strip_prefix("upload-") {
            uploads.get(index.parse::<usize>()?).context("Unknown uploaded image")?.to_string()
        } else {
            let id=Uuid::parse_str(id)?.to_string();
            let allowed:bool=db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM image_generations i JOIN usage u ON u.id=i.usage_id WHERE i.id=?1 AND u.user_id=?2 AND u.conversation_id=?3)",params![id,user,request.conversation_id.to_string()],|row|row.get(0))?;
            ensure!(allowed,"Unknown image in this conversation");
            let path=directory(config).join(format!("{id}.png"));
            ensure!(std::fs::metadata(&path)?.len()<=MAX_BYTES as u64,"Image exceeds 32 MiB");
            format!("data:image/png;base64,{}",STANDARD.encode(std::fs::read(path)?))
        };
        images.push(json!({"image_url":content}));
    }
    let mut body=json!({"model":"gpt-image-2","prompt":input.prompt,"n":1,"quality":"high","size":"1024x1024","output_format":"png",
        "background":if input.transparent_background { "transparent" } else { "opaque" }});
    let endpoint=if images.is_empty() { "generations" } else { body["images"]=json!(images); "edits" };
    Ok(Box::pin(async move {
        let client=reqwest::Client::builder().timeout(std::time::Duration::from_secs(300)).redirect(reqwest::redirect::Policy::none()).build()?;
        // Never retry a paid generation automatically: a lost response does not prove it was unbilled.
        let mut response=client.post(format!("https://api.openai.com/v1/images/{endpoint}")).bearer_auth(key).json(&body).send().await?;
        let status=response.status();
        let reference=response.headers().get("x-request-id").and_then(|value|value.to_str().ok()).unwrap_or("unavailable").to_owned();
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await? {
            ensure!(bytes.len()+chunk.len()<=MAX_BYTES.div_ceil(3)*4+64_000,"Image response exceeds its transport budget");
            bytes.extend_from_slice(&chunk);
        }
        let value:Value=serde_json::from_slice(&bytes)?;
        ensure!(status.is_success(),"Image API returned {status}; request {reference}; {}",value["error"]["message"].as_str().unwrap_or("No provider details"));
        Ok(value)
    }))
}

/// Only the owning signed-in user can retrieve an original, and no account path is accepted from HTTP.
pub async fn asset(State(app):State<App>,headers:HeaderMap,UrlPath(id):UrlPath<Uuid>)->ApiResult<Response> {
    let user=app.user(&headers)?;
    let allowed:bool=app.db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM image_generations i JOIN usage u ON u.id=i.usage_id WHERE i.id=?1 AND u.user_id=?2)",params![id.to_string(),user],|row|row.get(0))?;
    if !allowed { return Ok(StatusCode::NOT_FOUND.into_response()); }
    match tokio::fs::read(directory(&app.config()).join(format!("{id}.png"))).await {
        Ok(bytes)=>Ok(([(header::CONTENT_TYPE,"image/png"),(header::CONTENT_DISPOSITION,"inline"),(header::CACHE_CONTROL,"private, no-store")],bytes).into_response()),
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(StatusCode::GONE.into_response()),
        Err(error)=>Err(error.into()),
    }
}

/// Bound temporary originals by age and total bytes; small financial rows remain for usage accounting.
pub fn prune(config:&Config)->Result<()> {
    let root=directory(config);
    if !root.try_exists()? { return Ok(()); }
    ensure!(std::fs::symlink_metadata(&root)?.is_dir(),"Generated image cache must be a real directory");
    let mut files=Vec::new(); let mut total=0;
    for entry in std::fs::read_dir(root)? {
        let entry=entry?;
        if !entry.file_type()?.is_file() { continue; }
        let metadata=entry.metadata()?;
        let modified=metadata.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        total+=metadata.len(); files.push((modified,metadata.len(),entry.path()));
    }
    files.sort_by_key(|file|file.0);
    for (modified,bytes,path) in files {
        if modified>now()-300 { continue; }
        if modified>=now()-86400 && total<=256*1024*1024 { break; }
        std::fs::remove_file(path)?; total=total.saturating_sub(bytes);
    }
    Ok(())
}
