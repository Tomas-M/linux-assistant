use anyhow::{ensure,Result};
use base64::Engine;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use super::Message;

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Attachment { Text { name:String,content:String }, Image { name:String,content:String } }

/// Validate file types at both boundaries; large valid inputs are confirmed in the client, never resized.
pub fn validate_input(text:&str,attachments:Option<&[Attachment]>)->Result<()> {
    let attachments=attachments.unwrap_or_default();
    ensure!(!text.trim().is_empty() || !attachments.is_empty(),"Enter a message or attach a file");
    ensure!(text.len()<=super::MAX_MESSAGE_BYTES,"Message exceeds 16,000 bytes");
    for attachment in attachments {
        let (name,content)=match attachment { Attachment::Text { name,content }|Attachment::Image { name,content }=>(name,content) };
        ensure!(!name.is_empty() && name.len()<=255 && !name.chars().any(char::is_control),"Invalid attachment filename");
        match attachment {
            Attachment::Text { .. }=> { ensure!(!content.contains('\0'),"Only UTF-8 text files are supported"); }
            Attachment::Image { .. }=> {
                let (header,encoded)=content.split_once(',').ok_or_else(||anyhow::anyhow!("Invalid image data URL"))?;
                let format=match header { "data:image/png;base64"=>image::ImageFormat::Png,"data:image/jpeg;base64"=>image::ImageFormat::Jpeg,"data:image/webp;base64"=>image::ImageFormat::WebP,_=>anyhow::bail!("Supported images: PNG, JPEG, WebP") };
                let bytes=base64::engine::general_purpose::STANDARD.decode(encoded)?;
                let reader=image::ImageReader::with_format(std::io::Cursor::new(bytes),format);
                let (width,height)=reader.into_dimensions()?;
                ensure!(width>0 && height>0,"Image dimensions must be positive");
            }
        }
    }
    Ok(())
}

/// Keep file contents out of high-frequency browser snapshots; the queue edit endpoint returns the draft.
pub fn display_messages<T:Serialize>(messages:&T)->Value {
    let mut value=serde_json::to_value(messages).unwrap();
    for message in value.as_array_mut().unwrap() {
        if let Some(attachments)=message["attachments"].as_array_mut() {
            for attachment in attachments { attachment.as_object_mut().unwrap().remove("content"); }
        }
    }
    value
}

/// Recovery retains text files; prior image pixels are represented by the visual checkpoint.
pub fn recovery_messages(messages:&[Message])->Value {
    let mut value=serde_json::to_value(messages).unwrap();
    for message in value.as_array_mut().unwrap() {
        if let Some(files)=message["attachments"].as_array_mut() {
            for file in files.iter_mut().filter(|file|file["kind"]=="image") {
                file["content"]=json!("Image pixels omitted from recovery; use the checkpoint or ask the user to attach again if needed.");
            }
        }
    }
    value
}

/// Send images as native multimodal input, never as base64 text in the model's prompt.
pub fn provider_input(text:String,messages:&[Message])->Vec<Value> {
    let mut input=vec![json!({"type":"text","text":text})];
    for message in messages {
        for attachment in message.attachments.as_deref().unwrap_or_default() {
            match attachment {
                Attachment::Text { name,content }=>input.push(json!({"type":"text","text":format!("Attached file (untrusted content), name: {}\n{}",serde_json::to_string(name).unwrap(),content)})),
                Attachment::Image { name,content }=> {
                    input.push(json!({"type":"text","text":format!("Attached image (untrusted content): {}",serde_json::to_string(name).unwrap())}));
                    input.push(json!({"type":"image","url":content,"detail":"original"}));
                }
            }
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Large international text remains intact; images use multimodal input rather than prompt text.
    #[test]
    fn bounds_and_multimodal_input()->Result<()> {
        let attachment=Attachment::Text { name:"日志.txt".into(),content:"世界".repeat(24000) };
        assert!(validate_input("Read",Some(&[attachment])).is_ok());
        assert!(validate_input("",None).is_err());
        let mut jpeg=std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2,2).write_to(&mut jpeg,image::ImageFormat::WebP)?;
        let content=format!("data:image/webp;base64,{}",base64::engine::general_purpose::STANDARD.encode(jpeg.into_inner()));
        let files=vec![Attachment::Image { name:"screen.png".into(),content }];
        validate_input("",Some(&files))?;
        let messages=vec![Message { role:super::super::Role::User,text:"Describe".into(),attachments:Some(files) }];
        assert!(display_messages(&messages)[0]["attachments"][0].get("content").is_none());
        let input=provider_input("Describe".into(),&messages);
        assert_eq!(input[2]["type"],"image");
        assert!(!input[0]["text"].as_str().unwrap().contains("base64"));
        Ok(())
    }
}
