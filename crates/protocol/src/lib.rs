pub const ICONS_SVG:&str=include_str!("../assets/icons.svg");
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 96_000;
pub const MAX_MESSAGE_BYTES: usize = 16_000;
pub const MAX_UPLOAD_BYTES: usize = 64*1024*1024;
mod bridge;
pub use bridge::{Bridge,UiBundle,UiAsset,BRIDGE_API,MAX_UI_BYTES};
mod branding;
pub use branding::{Branding,QuickAction,Theme,BRAND_IMAGES,MAX_BRAND_IMAGE_BYTES,decode_brand_image};
mod attachments;
pub use attachments::{Attachment,validate_input,provider_input,display_messages,recovery_messages};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Platform { Linux, Windows }

impl Platform {
    /// Describe the client's shell, regardless of where the backend runs.
    pub fn shell(self) -> &'static str {
        match self { Self::Linux => "POSIX /bin/sh", Self::Windows => "Windows PowerShell 5.1" }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role { User, Assistant, Tool }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Message { pub role: Role, pub text: String, pub attachments: Option<Vec<Attachment>> }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatRequest {
    pub bridge:Bridge,
    pub task_id: Uuid,
    pub base: usize,
    pub request_id: Uuid,
    pub conversation_id: Uuid,
    pub platform: Platform,
    pub messages: Vec<Message>,
    pub commands_enabled: bool,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandProposal { pub command: String, pub cwd: String, pub reason: String, pub files: Option<Vec<String>> }

#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(tag="type",rename_all="snake_case",deny_unknown_fields)]
pub enum LocalAction {
    ViewImage { path:String,reason:String },
    GetMetadata { path:String,follow_symlinks:bool,reason:String },
    CreateDirectory { path:String,recursive:bool,reason:String },
    CopyPath { source:String,destination:String,recursive:bool,reason:String },
    MovePath { source:String,destination:String,reason:String },
    RemovePath { path:String,recursive:bool,reason:String },
    CreateSymlink { path:String,target:String,directory:bool,reason:String },
    SetPermissions { path:String,mode:String,reason:String },
    ReadFile { path:String,offset:u64,limit:u64,reason:String },
    ListDirectory { path:String,offset:usize,limit:usize,reason:String },
    SearchFiles { path:String,pattern:String,case_sensitive:bool,recursive:bool,reason:String },
    ApplyPatch { path:String,old_text:String,new_text:String,reason:String },
    WriteFile { path:String,content:String,reason:String },
    ExecCommand { cmd:String,workdir:String,reason:String,files:Vec<String>,yield_time_ms:u64,timeout_ms:Option<u64> },
    WriteStdin { session_id:Uuid,chars:String,close_stdin:bool,yield_time_ms:u64,reason:String },
    StopProcess { session_id:Uuid,reason:String },
}

impl LocalAction {
    /// Show the exact proposed input in the permission UI before it can be delivered.
    pub fn description(&self)->String {
        match self {
            Self::ViewImage { path,.. }=>format!("View image {path}"),
            Self::GetMetadata { path,.. }=>format!("Inspect metadata for {path}"),
            Self::CreateDirectory { path,recursive,.. }=>format!("Create directory {path} (parents: {recursive})"),
            Self::CopyPath { source,destination,recursive,.. }=>format!("Copy {source} to {destination} (recursive: {recursive}; no overwrite)"),
            Self::MovePath { source,destination,.. }=>format!("Move {source} to {destination} (no overwrite)"),
            Self::RemovePath { path,recursive,.. }=>format!("Delete {path} (recursive: {recursive})"),
            Self::CreateSymlink { path,target,.. }=>format!("Create symbolic link {path} pointing to {target}"),
            Self::SetPermissions { path,mode,.. }=>format!("Set permissions of {path} to {mode}"),
            Self::ReadFile { path,offset,limit,.. }=>format!("Read {path}, bytes {offset}..{}",offset.saturating_add(*limit)),
            Self::ListDirectory { path,.. }=>format!("List {path}"),
            Self::SearchFiles { path,pattern,.. }=>format!("Search {path} for {pattern}"),
            Self::ApplyPatch { path,old_text,new_text,.. }=>format!("Edit {path}\n\nReplace:\n{old_text}\n\nWith:\n{new_text}"),
            Self::WriteFile { path,content,.. }=>format!("Create {path}\n\n{content}"),
            Self::ExecCommand { cmd,.. }=>cmd.clone(),
            Self::WriteStdin { session_id,chars,close_stdin,.. }=> {
                if self.is_poll() { format!("Wait for session {session_id}") }
                else { format!("Send input to session {session_id}:\n{chars}{}",if *close_stdin { "\n[Close stdin after delivery]" } else { "" }) }
            }
            Self::StopProcess { session_id,.. }=>format!("Stop session {session_id}"),
        }
    }
    /// Polling only reads an already-authorized local session and needs no new execution approval.
    pub fn is_poll(&self)->bool { matches!(self,Self::WriteStdin { chars,close_stdin:false,.. } if chars.is_empty()) }
    /// Recovery may safely repeat reads and idempotent Stop, but never stdin delivery or filesystem writes.
    pub fn replay_safe(&self)->bool { self.is_poll() || matches!(self,Self::ViewImage { .. }|Self::GetMetadata { .. }|Self::ReadFile { .. }|Self::ListDirectory { .. }|Self::SearchFiles { .. }|Self::StopProcess { .. }) }
    /// Capture content changes only on explicitly affected paths, never an implicit directory scan.
    pub fn affected_paths(&self)->Vec<String> {
        match self {
            Self::ApplyPatch { path,.. }|Self::WriteFile { path,.. }|Self::RemovePath { path,.. }=>vec![path.clone()],
            Self::CopyPath { destination,.. }=>vec![destination.clone()],
            Self::MovePath { source,destination,.. }=>vec![source.clone(),destination.clone()],
            _=>vec![],
        }
    }
    /// Session identities are native-owned UUIDs; an execution's approval ID becomes its session ID.
    pub fn session_id(&self,id:Uuid)->Uuid { match self { Self::ExecCommand { .. }=>id,Self::WriteStdin { session_id,.. }|Self::StopProcess { session_id,.. }=>*session_id,_=>id } }
    /// Keep the permission UI and provider validation on the same explanation.
    pub fn reason(&self)->&str { match self { Self::ExecCommand { reason,.. }|Self::WriteStdin { reason,.. }|Self::StopProcess { reason,.. }|Self::ReadFile { reason,.. }|Self::ListDirectory { reason,.. }|Self::SearchFiles { reason,.. }|Self::ApplyPatch { reason,.. }|Self::WriteFile { reason,.. }|Self::ViewImage { reason,.. }|Self::GetMetadata { reason,.. }|Self::CreateDirectory { reason,.. }|Self::CopyPath { reason,.. }|Self::MovePath { reason,.. }|Self::RemovePath { reason,.. }|Self::CreateSymlink { reason,.. }|Self::SetPermissions { reason,.. }=>reason } }
    /// Validate operation-specific limits before either frontend can cause local side effects.
    pub fn validate(&self)->anyhow::Result<()> {
        anyhow::ensure!(serde_json::to_vec(self)?.len()<=128_000,"Local operation arguments exceed 128 KB");
        anyhow::ensure!(self.reason().len()<=4_000,"Command instructions are too long");
        match self {
            Self::ExecCommand { cmd,workdir,files,timeout_ms,.. }=> {
                anyhow::ensure!(!cmd.trim().is_empty() && cmd.len()<=16_000 && !cmd.contains('\0'),"Invalid command proposal");
                anyhow::ensure!(!workdir.is_empty() && workdir.len()<=4096 && !workdir.contains('\0'),"Invalid command directory");
                anyhow::ensure!(files.len()<=16 && files.iter().all(|path|!path.is_empty() && path.len()<=4096 && !path.contains('\0')),"Invalid affected file paths");
                anyhow::ensure!(timeout_ms.is_none_or(|timeout|timeout>0),"Execution timeout must be positive or null");
            }
            Self::WriteStdin { chars,.. }=>anyhow::ensure!(chars.len()<=16_000,"Session input is too long"),
            Self::ReadFile { path,.. }|Self::ListDirectory { path,.. }|Self::SearchFiles { path,.. }|Self::ApplyPatch { path,.. }|Self::WriteFile { path,.. }|Self::ViewImage { path,.. }|Self::GetMetadata { path,.. }|Self::CreateDirectory { path,.. }|Self::RemovePath { path,.. }|Self::CreateSymlink { path,.. }|Self::SetPermissions { path,.. }=>anyhow::ensure!(!path.is_empty() && path.len()<=4096 && !path.contains('\0'),"Invalid file path"),
            Self::CopyPath { source,destination,.. }|Self::MovePath { source,destination,.. }=> {
                for path in [source,destination] { anyhow::ensure!(!path.is_empty() && path.len()<=4096 && !path.contains('\0'),"Invalid source or destination path"); }
            }
            Self::StopProcess { .. }=>{},
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantReply { pub text: String }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatResponse { pub reply: AssistantReply, pub credits: i64, pub task_credits:Option<i64>, pub revision: usize, pub steering: Vec<Steering>, pub progress:Option<Progress> }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Steering { pub id: Uuid, pub text: String, pub attachments: Option<Vec<Attachment>> }

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Progress {
    pub attempt: usize,
    pub activity: Option<String>,
    pub summaries: Vec<ProgressText>,
    pub text: String,
    pub compacted: Option<Vec<String>>,
    pub requests: Vec<Interaction>,
    pub credits:Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgressText { pub id:String,pub item_id:String,pub kind:ProgressKind,pub completed:bool,pub text:String,pub data:Option<serde_json::Value> }
#[derive(Clone,Copy,Debug,Deserialize,Serialize,PartialEq)]
#[serde(rename_all="snake_case")]
pub enum ProgressKind { Reasoning,Commentary,Action,Plan,WebSearch }
mod tools;
pub use tools::*;

/// Snapshots let slow or reconnecting consumers catch up without an unbounded event queue.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag="type",rename_all="snake_case")]
pub enum ChatEvent {
    Progress { progress: Progress },
    Complete { response: ChatResponse },
    Error { message: String, settled: bool, credits: Option<i64>, task_credits:Option<i64>, steering: Vec<Steering>, progress:Option<Progress> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Account { pub email: String, pub credits: i64, pub development: bool, pub input_credits_per_million:f64 }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceLogin {
    pub device_id: String,
    pub poll_secret: String,
    pub expires_at: i64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PollLogin { pub device_id: String, pub poll_secret: String }

#[derive(Deserialize, Serialize)]
pub struct LoginResult { pub token:Option<String>, pub email_verification:Option<EmailVerification> }

#[derive(Clone,Copy,Debug,Deserialize,Serialize,PartialEq)]
#[serde(rename_all="lowercase")]
pub enum AuthProvider { Google,Microsoft,Github }
impl AuthProvider {
    /// Use canonical provider keys in configuration, URLs and stable identity records.
    pub fn key(self)->&'static str { match self { Self::Google=>"google",Self::Microsoft=>"microsoft",Self::Github=>"github" } }
}

#[derive(Clone,Deserialize,Serialize)]
pub struct EmailVerification { pub provider:AuthProvider, pub email:Option<String> }

#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreditPack { pub price_id:String,pub usd:String,pub credits:i64 }
impl CreditPack {
    /// Convert a configured USD price to cents exactly, without floating-point rounding.
    pub fn cents(&self)->anyhow::Result<i64> {
        let (whole,fraction)=self.usd.split_once('.').unwrap_or((&self.usd,""));
        anyhow::ensure!(!whole.is_empty() && whole.bytes().all(|byte|byte.is_ascii_digit()),"USD price must be a positive decimal");
        anyhow::ensure!(fraction.len()<=2 && fraction.bytes().all(|byte|byte.is_ascii_digit()),"USD price supports at most two decimal places");
        let cents=whole.parse::<i64>()?.checked_mul(100).and_then(|value|value.checked_add(format!("{fraction:0<2}").parse::<i64>().ok()?));
        Ok(cents.filter(|value|*value>0).ok_or_else(||anyhow::anyhow!("USD price is zero or too large"))?)
    }
}

#[derive(Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRequest { pub price_id:String }

#[derive(Deserialize, Serialize)]
pub struct EmailLogin { pub device_id: String, pub poll_secret: String, pub email: String }

#[derive(Deserialize, Serialize)]
pub struct VerifyEmail { pub device_id: String, pub poll_secret: String, pub code: String }

#[derive(Deserialize, Serialize)]
pub struct EmailChallenge { pub expires_at: i64, pub development_code: Option<String> }

/// Only the final answer uses the output schema; actions run through native dynamic tools.
pub fn reply_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"required":["text"],"properties":{"text":{"type":"string"}}})
}
