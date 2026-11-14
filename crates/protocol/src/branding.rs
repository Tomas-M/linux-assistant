use std::collections::BTreeMap;
use anyhow::{ensure,Result};
use base64::Engine;
use serde::{Deserialize,Serialize};

pub const MAX_BRAND_IMAGE_BYTES:usize=2*1024*1024;
pub const BRAND_IMAGES:[&str;4]=["assistant-image","logo-image","logo-full","favicon"];

#[derive(Clone,Copy,Default,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum Theme { #[default] Light,Dark }

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branding {
    pub name:String,pub default_theme:Theme,pub images:BTreeMap<String,String>,
    pub model_catalog:crate::ModelCatalog,
    #[serde(default)]
    pub quick_actions:Vec<QuickAction>,
}

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuickAction { pub label:String,pub prompt:String }

/// Brand uploads contain passive raster images, with bounded bytes and dimensions before publication.
pub fn decode_brand_image(content:&str,allow_icon:bool)->Result<(&'static str,Vec<u8>)> {
    ensure!(content.len()<=MAX_BRAND_IMAGE_BYTES.div_ceil(3)*4+64,"Use an image up to 2 MiB");
    let formats=if allow_icon { "Choose a PNG, JPEG, WebP or ICO image" } else { "Choose a PNG, JPEG or WebP image" };
    let (header,data)=content.split_once(',').ok_or_else(||anyhow::anyhow!(formats))?;
    let (mime,format)=match header {
        "data:image/png;base64"=>("image/png",image::ImageFormat::Png),
        "data:image/jpeg;base64"=>("image/jpeg",image::ImageFormat::Jpeg),
        "data:image/webp;base64"=>("image/webp",image::ImageFormat::WebP),
        "data:image/x-icon;base64"|"data:image/vnd.microsoft.icon;base64" if allow_icon=>("image/vnd.microsoft.icon",image::ImageFormat::Ico),
        _=>anyhow::bail!(formats),
    };
    let bytes=base64::engine::general_purpose::STANDARD.decode(data)?;
    ensure!(bytes.len()<=MAX_BRAND_IMAGE_BYTES,"Use an image up to 2 MiB");
    // ICO is an opaque multi-image container: validate its directory, never decode or select a frame.
    if format==image::ImageFormat::Ico {
        ensure!(bytes.len()>=6 && bytes[..4]==[0,0,1,0],"Choose a valid ICO file");
        let count=u16::from_le_bytes([bytes[4],bytes[5]]) as usize;
        let end=6+count*16;
        ensure!(count>0 && end<=bytes.len(),"ICO directory is incomplete");
        for entry in bytes[6..end].chunks_exact(16) {
            let size=u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;
            let offset=u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64;
            ensure!(size>0 && offset>=end as u64 && offset+size<=bytes.len() as u64,"ICO image data is incomplete");
        }
        return Ok((mime,bytes));
    }
    let (width,height)=image::ImageReader::with_format(std::io::Cursor::new(&bytes),format).into_dimensions()?;
    ensure!(width>0 && height>0 && width<=4096 && height<=4096,"Use an image of at most 4096 by 4096 pixels");
    Ok((mime,bytes))
}
