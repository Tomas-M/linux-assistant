use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 96_000;
pub const MAX_MESSAGE_BYTES: usize = 16_000;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Platform { Linux, Windows }

impl Platform {
    /// Describe the client's shell, regardless of where the backend runs.
    pub fn shell(self) -> &'static str {
        match self { Self::Linux => "POSIX /bin/sh", Self::Windows => "Windows PowerShell 5.1" }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Character { #[default] Guide, Companion, Specialist }

impl Character {
    /// Keep personality separate from permissions and tool behavior.
    pub fn instruction(self) -> &'static str {
        match self {
            Self::Guide => "Be patient, clear, and explain useful steps in plain language.",
            Self::Companion => "Be warm and conversational, with concise practical answers.",
            Self::Specialist => "Be direct and precise. Prefer concise technical explanations.",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role { User, Assistant, Tool }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Message { pub role: Role, pub text: String }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatRequest {
    pub base: usize,
    pub request_id: Uuid,
    pub conversation_id: Uuid,
    pub platform: Platform,
    pub character: Character,
    pub messages: Vec<Message>,
    pub commands_enabled: bool,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandProposal { pub command: String, pub cwd: String, pub reason: String }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantReply { pub text: String, pub command: Option<CommandProposal> }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatResponse { pub reply: AssistantReply, pub credits: i64, pub revision: usize, pub steering: Vec<Steering> }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Steering { pub id: Uuid, pub text: String }

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Progress {
    pub attempt: usize,
    pub activity: Option<String>,
    pub summaries: Vec<ProgressText>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgressText { pub id: String, pub text: String }

/// Snapshots let slow or reconnecting consumers catch up without an unbounded event queue.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag="type",rename_all="snake_case")]
pub enum ChatEvent {
    Progress { progress: Progress },
    Complete { response: ChatResponse },
    Error { message: String, refunded: bool, steering: Vec<Steering> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Account { pub email: String, pub credits: i64, pub development: bool }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceLogin {
    pub device_id: String,
    pub poll_secret: String,
    pub verification_url: String,
    pub expires_at: i64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PollLogin { pub device_id: String, pub poll_secret: String }

#[derive(Deserialize, Serialize)]
pub struct LoginResult { pub token: Option<String> }

/// Constrain model output to an answer and at most one proposed local command.
pub fn reply_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object", "additionalProperties":false, "required":["text","command"],
        "properties":{
            "text":{"type":"string"},
            "command":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,
                "required":["command","cwd","reason"],"properties":{
                    "command":{"type":"string"},"cwd":{"type":"string"},"reason":{"type":"string"}
                }}]}
        }
    })
}
