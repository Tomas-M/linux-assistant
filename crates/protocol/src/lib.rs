pub const ICONS_SVG:&str=include_str!("../assets/icons.svg");
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 96_000;
pub const MAX_MESSAGE_BYTES: usize = 16_000;
pub const MAX_UPLOAD_BYTES: usize = 64*1024*1024;
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantReply { pub text: String, pub command: Option<CommandProposal> }

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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgressText { pub id: String, pub text: String }

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

/// Constrain model output to an answer and at most one proposed local command.
pub fn reply_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object", "additionalProperties":false, "required":["text","command"],
        "properties":{
            "text":{"type":"string"},
            "command":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,
                "required":["command","cwd","reason","files"],"properties":{
                    "command":{"type":"string"},"cwd":{"type":"string"},"reason":{"type":"string"},
                    "files":{"type":"array","items":{"type":"string"},"description":"Explicit paths this command may create, edit or delete; empty for read-only commands. No globs or directories."}
                }}]}
        }
    })
}
