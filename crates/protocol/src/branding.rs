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
pub struct Branding { pub name:String,pub default_theme:Theme,pub images:BTreeMap<String,String> }

/// Brand uploads contain passive raster images, with bounded bytes and dimensions before publication.
pub fn decode_brand_image(content:&str)->Result<(&'static str,Vec<u8>)> {
    ensure!(content.len()<=MAX_BRAND_IMAGE_BYTES.div_ceil(3)*4+64,"Use an image up to 2 MiB");
    let (header,data)=content.split_once(',').ok_or_else(||anyhow::anyhow!("Choose a PNG, JPEG or WebP image"))?;
    let (mime,format)=match header {
        "data:image/png;base64"=>("image/png",image::ImageFormat::Png),
        "data:image/jpeg;base64"=>("image/jpeg",image::ImageFormat::Jpeg),
        "data:image/webp;base64"=>("image/webp",image::ImageFormat::WebP),
        _=>anyhow::bail!("Choose a PNG, JPEG or WebP image"),
    };
    let bytes=base64::engine::general_purpose::STANDARD.decode(data)?;
    ensure!(bytes.len()<=MAX_BRAND_IMAGE_BYTES,"Use an image up to 2 MiB");
    let (width,height)=image::ImageReader::with_format(std::io::Cursor::new(&bytes),format).into_dimensions()?;
    ensure!(width>0 && height>0 && width<=4096 && height<=4096,"Use an image of at most 4096 by 4096 pixels");
    Ok((mime,bytes))
}
