use super::LocalAction;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;

pub const TOOL_OUTPUT_BYTES:usize=8_000;

#[derive(Clone,Debug,Deserialize,Serialize)]
pub struct Interaction { pub id:Uuid,pub item_id:String,#[serde(flatten)] pub kind:InteractionKind }
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(tag="type",rename_all="snake_case")]
pub enum InteractionKind { Tool { action:LocalAction }, Questions { questions:Vec<Question> } }
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase")]
pub struct Question { pub id:String,pub header:String,pub question:String,pub options:Option<Vec<QuestionOption>>,pub is_other:bool,pub is_secret:bool }
#[derive(Clone,Debug,Deserialize,Serialize)]
pub struct QuestionOption { pub label:String,pub description:String }
#[derive(Clone,Debug,Deserialize,Serialize,PartialEq)]
#[serde(tag="type",rename_all="snake_case")]
pub enum InteractionAnswer {
    Tool { output:String,success:bool,#[serde(skip_serializing_if="Option::is_none")] image:Option<super::Attachment> },
    Questions { answers:std::collections::HashMap<String,Vec<String>> },
    Approval { approve:bool },
}
#[derive(Clone,Debug,Deserialize,Serialize)]
pub struct InteractionResponse { pub id:Uuid,pub answer:InteractionAnswer }

/// Register only tools implemented by our Rust client, never the backend host's filesystem or shell.
pub fn tool_specs()->Vec<Value> {
    let text=json!({"type":"string"}); let number=json!({"type":"integer","minimum":0}); let boolean=json!({"type":"boolean"});
    vec![
        ("exec_command","Run a local shell command; yield returns a session without killing it.",json!({"cmd":text,"workdir":text,"reason":text,"files":{"type":"array","items":text},"yield_time_ms":number,"timeout_ms":{"anyOf":[number,{"type":"null"}]}})),
        ("write_stdin","Poll a local session with empty chars, or deliver exact stdin and optional EOF.",json!({"session_id":text,"chars":text,"close_stdin":boolean,"yield_time_ms":number,"reason":text})),
        ("stop_process","Stop a local command and its tracked process tree.",json!({"session_id":text,"reason":text})),
        ("view_image","Inspect a local PNG, JPEG or WebP image. Returns its original pixels as native image input, not text. Use this to examine screenshots and images on disk.",json!({"path":text,"reason":text})),
        ("get_metadata","Inspect file, directory or symlink type, size, timestamps and permissions. Set follow_symlinks to inspect its target instead.",json!({"path":text,"follow_symlinks":boolean,"reason":text})),
        ("create_directory","Create a directory; recursive also creates missing parents.",json!({"path":text,"recursive":boolean,"reason":text})),
        ("copy_path","Copy a file, symlink or directory tree without overwriting. Directories require recursive=true; symlinks are copied without following targets.",json!({"source":text,"destination":text,"recursive":boolean,"reason":text})),
        ("move_path","Move or rename a file, directory or symlink without overwriting. Source and destination must be on the same filesystem.",json!({"source":text,"destination":text,"reason":text})),
        ("remove_path","Delete a file or symlink, or a directory. Nonempty directories require recursive=true. Never follows symlink targets. Deletion is permanent.",json!({"path":text,"recursive":boolean,"reason":text})),
        ("create_symlink","Create a symbolic link without overwriting. Target can be relative; directory selects the link type on Windows, where OS privileges may be required.",json!({"path":text,"target":text,"directory":boolean,"reason":text})),
        ("set_permissions","Set POSIX permissions on Linux using an octal string such as 644 or 755. On Windows use mode readonly or writable; ACL management requires an explicit command.",json!({"path":text,"mode":text,"reason":text})),
        ("read_file","Read a UTF-8 file by byte offset, at most 24000 bytes; output reports the next offset.",json!({"path":text,"offset":number,"limit":number,"reason":text})),
        ("list_directory","List a local directory in sorted, paginated order; limit at most 200.",json!({"path":text,"offset":number,"limit":number,"reason":text})),
        ("search_files","Search UTF-8 files for literal text, optionally recursively; bounded results report truncation. Symlinks are not followed.",json!({"path":text,"pattern":text,"case_sensitive":boolean,"recursive":boolean,"reason":text})),
        ("apply_patch","Replace one exact, unique old_text occurrence with new_text in an existing UTF-8 file. Preserves all other bytes. Read the file first.",json!({"path":text,"old_text":text,"new_text":text,"reason":text})),
        ("write_file","Create a new UTF-8 file; refuses to overwrite an existing path. Use apply_patch for edits.",json!({"path":text,"content":text,"reason":text})),
    ].into_iter().map(|(name,description,properties)| {
        let required=properties.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        json!({"type":"function","name":format!("local_{name}"),"description":description,"inputSchema":{"type":"object","additionalProperties":false,"required":required,"properties":properties}})
    }).collect()
}

/// Decode a known native tool using the same strict argument validation as local execution.
pub fn tool_action(name:&str,mut arguments:Value)->anyhow::Result<LocalAction> {
    let kind=name.strip_prefix("local_").ok_or_else(||anyhow::anyhow!("Unknown client tool"))?;
    arguments.as_object_mut().ok_or_else(||anyhow::anyhow!("Tool arguments must be an object"))?.insert("type".into(),json!(kind));
    let action:LocalAction=serde_json::from_value(arguments)?;
    action.validate()?;
    Ok(action)
}

/// Accept one nonempty answer per advertised question and reject invented IDs or oversized custom text.
pub fn validate_answers(questions:&[Question],answers:&std::collections::HashMap<String,Vec<String>>)->anyhow::Result<()> {
    anyhow::ensure!(answers.len()==questions.len(),"Answer every question");
    for question in questions {
        let answer=answers.get(&question.id).ok_or_else(||anyhow::anyhow!("Missing answer for {}",question.header))?;
        anyhow::ensure!(answer.len()==1 && !answer[0].trim().is_empty() && answer[0].len()<=8000,"Enter one answer per question, up to 8000 bytes");
        if !question.is_other {
            if let Some(options)=question.options.as_ref().filter(|options|!options.is_empty()) { anyhow::ensure!(options.iter().any(|option|option.label==answer[0]),"Choose an advertised answer"); }
        }
    }
    Ok(())
}
