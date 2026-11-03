use std::{collections::HashMap,io::{Cursor,Read},path::Path,sync::{Arc,Mutex}};
use anyhow::{ensure,Context,Result};
use base64::Engine;
use serde_json::{json,Value};
use protocol::Attachment;

struct Image { mime:String,width:u32,height:u32,bytes:Vec<u8>,thumbnail:Mutex<Option<Vec<u8>>> }
#[derive(Clone,Default)]
pub struct Media(Arc<Mutex<HashMap<(String,usize),Arc<Image>>>>);
static THUMBNAILS:Mutex<()>=Mutex::new(());

impl Media {
    /// Snapshot original encoded pixels in RAM; later source-file edits do not change the preview.
    pub fn store(&self,scope:&str,index:usize,bytes:Vec<u8>)->Result<()> {
        if self.contains(scope,index) { return Ok(()); }
        let mime=match image::guess_format(&bytes)? { image::ImageFormat::Png=>"image/png",image::ImageFormat::Jpeg=>"image/jpeg",image::ImageFormat::WebP=>"image/webp",_=>anyhow::bail!("Supported images: PNG, JPEG, WebP") };
        let (width,height)=image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?.into_dimensions()?;
        ensure!(width>0 && height>0,"Image dimensions must be positive");
        self.0.lock().unwrap().entry((scope.into(),index)).or_insert_with(||Arc::new(Image { mime:mime.into(),width,height,bytes,thumbnail:Mutex::new(None) }));
        Ok(())
    }
    /// Repeated stream snapshots share the same immutable image reference.
    pub fn contains(&self,scope:&str,index:usize)->bool { self.0.lock().unwrap().contains_key(&(scope.into(),index)) }
    /// A new chat releases the preceding conversation's images.
    pub fn clear(&self) { self.0.lock().unwrap().clear(); }
    /// Store attachment originals separately from lightweight UI state snapshots.
    pub fn cache_attachments(&self,scope:&str,attachments:Option<&[Attachment]>)->Result<()> {
        for (index,file) in attachments.unwrap_or_default().iter().enumerate() {
            if let Attachment::Image { content,.. }=file {
                if self.contains(scope,index) { continue; }
                self.store(scope,index,base64::engine::general_purpose::STANDARD.decode(content.split_once(',').context("Invalid image data URL")?.1)?)?;
            }
        }
        Ok(())
    }
    /// Return original pixels or lazily create a small preview, without filesystem caching.
    pub fn read(&self,scope:&str,index:usize,thumbnail:bool)->Result<(String,Vec<u8>)> {
        let image=self.0.lock().unwrap().get(&(scope.into(),index)).cloned().context("Image is no longer available")?;
        if !thumbnail { return Ok((image.mime.clone(),image.bytes.clone())); }
        let mut preview=image.thumbnail.lock().unwrap();
        if preview.is_none() {
            use image::ImageDecoder;
            let _decoding=THUMBNAILS.lock().unwrap();
            let mut decoder=image::ImageReader::new(Cursor::new(&image.bytes)).with_guessed_format()?.into_decoder()?;
            let orientation=decoder.orientation()?;
            let mut decoded=image::DynamicImage::from_decoder(decoder)?; decoded.apply_orientation(orientation);
            let mut bytes=Cursor::new(Vec::new());
            decoded.thumbnail(192,144).write_to(&mut bytes,image::ImageFormat::Png)?;
            *preview=Some(bytes.into_inner());
        }
        Ok(("image/png".into(),preview.as_ref().unwrap().clone()))
    }
    /// Native image tools return original pixels to AI and a scoped URL to the browser.
    pub fn view(&self,id:uuid::Uuid,path:&Path)->Result<(String,Attachment,Value)> {
        ensure!(path.is_absolute(),"Image paths must be absolute");
        let scope=id.to_string();
        if !self.contains(&scope,0) {
            let limit=protocol::MAX_UPLOAD_BYTES/4*3-128_000;
            let mut bytes=Vec::new(); crate::files::open_file(path)?.take(limit as u64+1).read_to_end(&mut bytes)?;
            ensure!(bytes.len()<=limit,"Image exceeds the 64 MiB JSON transport budget");
            self.store(&scope,0,bytes)?;
        }
        let image=self.0.lock().unwrap().get(&(scope.clone(),0)).cloned().unwrap();
        let name=path.file_name().context("Missing image filename")?.to_string_lossy().into_owned();
        let attachment=Attachment::Image { name:name.clone(),content:format!("data:{};base64,{}",image.mime,base64::engine::general_purpose::STANDARD.encode(&image.bytes)) };
        Ok((format!("Viewed image {} ({} × {} pixels)",path.display(),image.width,image.height),attachment,json!({"kind":"image","name":name,"url":format!("/api/media/{scope}/0")})))
    }
}

/// Produce lightweight attachment metadata without copying image pixels into every event.
pub fn attachments(scope:&str,files:Option<&[Attachment]>)->Value {
    json!(files.unwrap_or_default().iter().enumerate().map(|(index,file)|match file {
        Attachment::Text { name,.. }=>json!({"kind":"text","name":name}),
        Attachment::Image { name,.. }=>json!({"kind":"image","name":name,"url":format!("/api/media/{scope}/{index}")}),
    }).collect::<Vec<_>>())
}

/// Message indexes stay stable until a fresh conversation replaces their UUID scope.
pub fn messages(conversation:uuid::Uuid,messages:&[protocol::Message])->Value {
    json!(messages.iter().enumerate().map(|(index,message)|json!({"role":message.role,"text":message.text,"attachments":attachments(&format!("{conversation}_{index}"),message.attachments.as_deref())})).collect::<Vec<_>>())
}
