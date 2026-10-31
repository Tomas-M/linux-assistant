use std::{io::{Cursor,Read,Write},path::{Path,PathBuf}};
use anyhow::{ensure,Context,Result};
use base64::Engine;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use protocol::Attachment;

#[derive(Serialize,Deserialize)]
struct Image { hash:String,mime:String,width:u32,height:u32 }
static THUMBNAILS:std::sync::Mutex<()>=std::sync::Mutex::new(());

/// Address only private cached snapshots, never a caller-supplied filesystem path.
fn reference(directory:&Path,scope:&str,index:usize)->Result<PathBuf> {
    ensure!(!scope.is_empty() && scope.len()<=80 && scope.bytes().all(|byte|byte.is_ascii_hexdigit() || byte==b'-' || byte==b'_'),"Invalid image reference");
    Ok(directory.join("media").join(format!("{scope}-{index}.json")))
}

/// Snapshot original encoded pixels once; the reference stays valid after the source file changes.
pub fn store(directory:&Path,scope:&str,index:usize,bytes:&[u8])->Result<()> {
    let path=reference(directory,scope,index)?;
    if path.exists() { return Ok(()); }
    let mime=match image::guess_format(bytes)? { image::ImageFormat::Png=>"image/png",image::ImageFormat::Jpeg=>"image/jpeg",image::ImageFormat::WebP=>"image/webp",_=>anyhow::bail!("Supported images: PNG, JPEG, WebP") };
    let (width,height)=image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.into_dimensions()?;
    ensure!(width>0 && height>0,"Image dimensions must be positive");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let hash=format!("{:x}",Sha256::digest(bytes)); let original=directory.join("media").join(format!("{hash}.image"));
    if !original.exists() {
        let mut staged=tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        staged.write_all(bytes)?; staged.as_file().sync_all()?; staged.persist(&original)?;
    }
    crate::storage::write(&path,&Image { hash,mime:mime.into(),width,height })
}

/// Cache attachment pixels separately so frequent UI snapshots contain only small local URLs.
pub fn cache_attachments(directory:&Path,scope:&str,attachments:Option<&[Attachment]>)->Result<()> {
    for (index,file) in attachments.unwrap_or_default().iter().enumerate() {
        if let Attachment::Image { content,.. }=file {
            if reference(directory,scope,index)?.exists() { continue; }
            let encoded=content.split_once(',').context("Invalid image data URL")?.1;
            store(directory,scope,index,&base64::engine::general_purpose::STANDARD.decode(encoded)?)?;
        }
    }
    Ok(())
}

/// Produce lightweight attachment metadata without serializing and then discarding image pixels.
pub fn attachments(scope:&str,files:Option<&[Attachment]>)->Value {
    json!(files.unwrap_or_default().iter().enumerate().map(|(index,file)|match file {
        Attachment::Text { name,.. }=>json!({"kind":"text","name":name}),
        Attachment::Image { name,.. }=>json!({"kind":"image","name":name,"url":format!("/api/media/{scope}/{index}")}),
    }).collect::<Vec<_>>())
}

/// The same message index is stable throughout a conversation; a new chat has a distinct scope.
pub fn messages(conversation:uuid::Uuid,messages:&[protocol::Message])->Value {
    json!(messages.iter().enumerate().map(|(index,message)|json!({"role":message.role,"text":message.text,"attachments":attachments(&format!("{conversation}_{index}"),message.attachments.as_deref())})).collect::<Vec<_>>())
}

/// Return original pixels or lazily create a small UI preview; AI input is never resized.
pub fn read(directory:&Path,scope:&str,index:usize,thumbnail:bool)->Result<(String,Vec<u8>)> {
    let image:Image=crate::storage::read(&reference(directory,scope,index)?)?;
    let original=directory.join("media").join(format!("{}.image",image.hash));
    if !thumbnail { return Ok((image.mime,std::fs::read(original)?)); }
    let preview=directory.join("media").join(format!("{}.thumbnail.png",image.hash));
    // A gallery must not allocate several large decoded pixel buffers at the same time.
    let _decoding=THUMBNAILS.lock().unwrap();
    if !preview.exists() {
        use image::ImageDecoder;
        let mut decoder=image::ImageReader::open(original)?.with_guessed_format()?.into_decoder()?;
        let orientation=decoder.orientation()?;
        let mut image=image::DynamicImage::from_decoder(decoder)?; image.apply_orientation(orientation);
        let image=image.thumbnail(192,144);
        let mut staged=tempfile::NamedTempFile::new_in(preview.parent().unwrap())?;
        image.write_to(staged.as_file_mut(),image::ImageFormat::Png)?; staged.persist(&preview)?;
    }
    Ok(("image/png".into(),std::fs::read(preview)?))
}

/// A native image result contains the original data URL, while the UI gets only its cached reference.
pub fn view(directory:&Path,id:uuid::Uuid,path:&Path)->Result<(String,Attachment,Value)> {
    ensure!(path.is_absolute(),"Image paths must be absolute");
    let scope=id.to_string();
    if !reference(directory,&scope,0)?.exists() {
        let mut bytes=Vec::new(); crate::files::open_file(path)?.take((protocol::MAX_UPLOAD_BYTES/4*3-128_000) as u64+1).read_to_end(&mut bytes)?;
        ensure!(bytes.len()<=protocol::MAX_UPLOAD_BYTES/4*3-128_000,"Image exceeds the 64 MiB JSON transport budget");
        store(directory,&scope,0,&bytes)?;
    }
    let (mime,bytes)=read(directory,&scope,0,false)?;
    let name=path.file_name().context("Missing image filename")?.to_string_lossy().into_owned();
    let image:Image=crate::storage::read(&reference(directory,&scope,0)?)?;
    let attachment=Attachment::Image { name:name.clone(),content:format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)) };
    Ok((format!("Viewed image {} ({} × {} pixels)",path.display(),image.width,image.height),attachment,json!({"kind":"image","name":name,"url":format!("/api/media/{scope}/0")})))
}
