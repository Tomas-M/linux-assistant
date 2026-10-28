use std::{path::{Path, PathBuf}, time::Duration, sync::{Arc,atomic::{AtomicBool,Ordering}}};
use anyhow::{bail, ensure, Context, Result};
use protocol::{Account, Character, ChatRequest, ChatResponse, CommandProposal, DeviceLogin, LoginResult, Message, Platform, PollLogin, Role, MAX_REQUEST_BYTES, MAX_MESSAGE_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use crate::{processes::Processes, storage};
mod output;

#[derive(Clone, Deserialize, Serialize)]
pub struct Settings { pub backend: String, pub character: Option<Character>, pub commands_enabled: bool }
#[derive(Clone, Deserialize, Serialize)]
pub struct PendingCommand { pub id: Uuid, pub proposal: CommandProposal }
#[derive(Clone, Deserialize, Serialize)]
pub struct Saved {
    pub conversation_id: Uuid,
    pub settings: Settings,
    pub token: Option<String>,
    pub messages: Vec<Message>,
    pub committed: usize,
    pub pending: Option<PendingCommand>,
    pub request: Option<ChatRequest>,
    pub pending_output: Option<Uuid>,
}
pub struct Engine { pub saved: Saved, pub controls: Arc<crate::controls::Controls>, pub processes: Processes, pub live: crate::stream::Live, pub full_access: Arc<AtomicBool>, platform: Platform, path: PathBuf, http: reqwest::Client, login: Option<DeviceLogin>, account: Option<Account> }

impl Engine {
    /// One daemon owns settings and conversation state for both frontends.
    pub fn open(directory: &Path) -> Result<Self> {
        let platform = match std::env::consts::OS {
            "linux" => Platform::Linux, "windows" => Platform::Windows,
            name => bail!("Unsupported operating system: {name}"),
        };
        let path = directory.join("state.json");
        let saved = if path.exists() { storage::read(&path)? } else {
            Saved { conversation_id:Uuid::new_v4(),settings:Settings { backend:std::env::var("LINUX_ASSISTANT_BACKEND").unwrap_or_else(|_|"http://127.0.0.1:8787".into()),character:None,commands_enabled:false },
                token:None,committed:0,messages:vec![],pending:None,request:None,pending_output:None }
        };
        validate_backend(&saved.settings.backend)?;
        let live=crate::stream::Live::new();
        let controls=crate::controls::Controls::open(directory,live.clone())?;
        let engine=Self { controls,saved,processes:Processes::open(directory)?,live,full_access:Arc::new(AtomicBool::new(false)),platform,path,http:reqwest::Client::builder().timeout(Duration::from_secs(660)).redirect(reqwest::redirect::Policy::none()).build()?,login:None,account:None };
        if let Some(request)=&engine.saved.request { engine.controls.remove(request.request_id)?; }
        engine.live.state(engine.state());
        Ok(engine)
    }

    /// Never expose authentication tokens or pending request internals to browser state.
    pub fn state(&self) -> Value {
        json!({"platform":self.platform,"shell":self.platform.shell(),"settings":self.saved.settings,"full_access":self.full_access.load(Ordering::SeqCst),"authenticated":self.saved.token.is_some(),"messages":self.saved.messages,
            "pending":self.saved.pending,"retry_available":self.saved.request.is_some() || self.saved.pending_output.is_some(),"account":self.account})
    }

    /// Keep all state transitions durable before performing external side effects.
    fn save(&self) -> Result<()> {
        storage::write(&self.path,&self.saved)?;
        self.live.state(self.state());
        Ok(())
    }

    /// Authenticate outbound backend requests with the locally stored session only.
    fn backend(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self.http.request(method,format!("{}{path}",self.saved.settings.backend));
        if let Some(token) = &self.saved.token { request = request.bearer_auth(token); }
        request
    }

    /// Read backend errors while keeping provider credentials entirely server-side.
    async fn response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(status.is_success(),"{}",value["error"].as_str().unwrap_or("Backend request failed"));
        Ok(serde_json::from_value(value)?)
    }

    /// Changing the backend signs out and clears conversation state to prevent cross-server leaks.
    pub fn set_backend(&mut self, backend: String) -> Result<()> {
        let backend = backend.trim_end_matches('/').to_owned();
        validate_backend(&backend)?;
        self.saved.settings.backend = backend;
        self.saved.token = None;
        self.saved.settings.character = None;
        self.saved.settings.commands_enabled = false;
        self.full_access.store(false,Ordering::SeqCst);
        self.account = None;
        self.login = None;
        self.new_chat()
    }

    /// Disabling execution immediately invalidates every unapproved command.
    pub fn settings(&mut self, character: Character, enabled: bool) -> Result<()> {
        self.saved.settings.character = Some(character);
        self.saved.settings.commands_enabled = enabled;
        if !enabled { self.full_access.store(false,Ordering::SeqCst); }
        if !enabled && self.saved.pending.take().is_some() {
            self.saved.messages.push(Message { role:Role::Tool,text:"The user disabled command execution; the pending command was not run.".into() });
        }
        self.save()
    }

    /// Start backend-owned email/Google sign-in; both interfaces open the same hosted page.
    pub async fn login(&mut self) -> Result<String> {
        let login: DeviceLogin = Self::response(self.backend(reqwest::Method::POST,"/v1/auth/device").send().await?).await?;
        let url = login.verification_url.clone();
        self.login = Some(login);
        Ok(url)
    }

    /// Persist the application session once the hosted browser login is approved.
    pub async fn poll_login(&mut self) -> Result<bool> {
        let login = self.login.as_ref().context("Start sign-in first")?;
        let result: LoginResult = Self::response(self.backend(reqwest::Method::POST,"/v1/auth/poll")
            .json(&PollLogin { device_id:login.device_id.clone(),poll_secret:login.poll_secret.clone() }).send().await?).await?;
        if let Some(token) = result.token {
            self.saved.token = Some(token);
            self.login = None;
            self.account = None;
            self.saved.settings.character = None;
            self.saved.settings.commands_enabled = false;
            self.new_chat()?;
            self.refresh_account().await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Query the authoritative balance rather than estimating provider usage locally.
    pub async fn refresh_account(&mut self) -> Result<Value> {
        let response = self.backend(reqwest::Method::GET,"/v1/me").send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.saved.token = None;
            self.account = None;
            self.save()?;
        }
        self.account = Some(Self::response(response).await?);
        self.live.state(self.state());
        Ok(self.state())
    }

    /// Revoke server access before clearing the local session.
    pub async fn logout(&mut self) -> Result<()> {
        let response = self.backend(reqwest::Method::POST,"/v1/logout").send().await?;
        if response.status()!=reqwest::StatusCode::UNAUTHORIZED { Self::response::<Value>(response).await?; }
        self.saved.token = None;
        self.saved.settings.commands_enabled = false;
        self.full_access.store(false,Ordering::SeqCst);
        self.saved.settings.character = None;
        self.account = None;
        self.new_chat()
    }

    /// A fresh chat clears pending approvals and retry state, keeping account settings.
    pub fn new_chat(&mut self) -> Result<()> {
        self.live.0.send_modify(|view| { view["steps"]=json!([]); view["activity"]=Value::Null; view["error"]=Value::Null; });
        self.saved.conversation_id = Uuid::new_v4();
        self.saved.messages.clear();
        self.saved.committed=0;
        self.controls.clear()?;
        self.saved.pending = None;
        self.saved.request = None;
        self.saved.pending_output = None;
        self.save()
    }

    /// Persist user input before dispatch; a lost response can be retried with the same request ID.
    fn prepare_message(&mut self,text:String,cwd:String,id:Uuid)->Result<()> {
        ensure!(self.saved.request.is_none() && self.saved.pending_output.is_none(),"Retry the unfinished request or start a new chat first");
        ensure!(self.saved.pending.is_none(),"Approve or decline the pending command first");
        ensure!(!text.trim().is_empty() && text.len()<=MAX_MESSAGE_BYTES,"Enter a message up to 16,000 bytes");
        self.saved.messages.push(Message { role:Role::User,text });
        if let Err(error)=self.prepare(cwd,id) { self.saved.messages.pop(); return Err(error); }
        Ok(())
    }
    /// Tests and direct engine callers use the same durable input path as queued browser messages.
    #[cfg(test)]
    pub async fn chat(&mut self,text:String,cwd:String)->Result<Value> {
        self.prepare_message(text,cwd,Uuid::new_v4())?;
        self.retry().await
    }
    /// Acknowledge dequeue only after saving its stable ID, making crash recovery unambiguous.
    pub async fn next(&mut self)->Result<bool> {
        let Some(input)=self.controls.next() else { return Ok(false); };
        self.prepare_message(input.text,input.cwd,input.id)?;
        self.controls.remove(input.id)?;
        self.retry().await?;
        Ok(true)
    }

    /// Send only new input after the last committed conversation revision.
    fn prepare(&mut self, cwd: String, id:Uuid) -> Result<()> {
        ensure!(self.saved.token.is_some(),"Sign in first");
        let character = self.saved.settings.character.context("Choose an assistant character first")?;
        let request = ChatRequest { base:self.saved.committed,request_id:id,conversation_id:self.saved.conversation_id,platform:self.platform,character,messages:self.saved.messages[self.saved.committed..].to_vec(),commands_enabled:self.saved.settings.commands_enabled,cwd };
        ensure!(serde_json::to_vec(&request)?.len()<=MAX_REQUEST_BYTES,"New input exceeds the per-request limit");
        self.saved.request = Some(request);
        self.save()
    }

    /// Retain the same request after ambiguous failures; use a fresh ID after a known refund.
    pub async fn retry(&mut self) -> Result<Value> {
        if self.saved.pending_output.is_some() { self.finish_output()?; }
        let request = self.saved.request.as_ref().context("No unfinished request")?;
        let _active=self.controls.active(crate::controls::Remote { backend:self.saved.settings.backend.clone(),token:self.saved.token.clone().unwrap(),request:request.request_id });
        self.live.activity("Waiting for assistant…",Some(Default::default()));
        let response = self.backend(reqwest::Method::POST,"/v1/chat").header(reqwest::header::ACCEPT,"text/event-stream").json(request).send().await?;
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE || status == reqwest::StatusCode::PAYMENT_REQUIRED {
                self.saved.request.as_mut().unwrap().request_id = Uuid::new_v4();
                self.save()?;
            }
            return Self::response::<Value>(response).await;
        }
        let result=self.read_stream(response).await?;
        let expected=self.saved.messages.len()+result.steering.len()+1;
        ensure!(result.revision==expected,"Backend returned an unexpected conversation revision");
        self.saved.messages.extend(result.steering.into_iter().map(|input|Message { role:Role::User,text:input.text }));
        self.saved.messages.push(Message { role:Role::Assistant,text:result.reply.text });
        self.saved.committed=result.revision;
        self.saved.pending = result.reply.command.filter(|_|self.saved.settings.commands_enabled)
            .map(|proposal|PendingCommand { id:Uuid::new_v4(),proposal });
        self.saved.request = None;
        if self.controls.stopped.load(Ordering::SeqCst) && self.saved.pending.take().is_some() {
            self.saved.messages.push(Message { role:Role::Tool,text:"The user stopped the task; the proposed command was not executed.".into() });
        }
        if let Some(account) = &mut self.account { account.credits = result.credits; }
        self.save()?;
        Ok(self.state())
    }

    /// Commit only a terminal, validated reply; an interrupted preview retains its original request ID.
    async fn read_stream(&mut self, mut response: reqwest::Response) -> Result<ChatResponse> {
        ensure!(response.headers().get(reqwest::header::CONTENT_TYPE).is_some_and(|value|value.as_bytes().starts_with(b"text/event-stream")),"Backend does not support streaming; update the backend first");
        let mut decoder=crate::stream::Decoder::default();
        while let Some(bytes)=response.chunk().await? {
            for event in decoder.push(&bytes)? {
                match serde_json::from_value::<protocol::ChatEvent>(event)? {
                    protocol::ChatEvent::Progress { progress } => {
                        self.live.0.send_modify(|view|view["can_steer"]=json!(true));
                        let activity=progress.activity.clone();
                        let label=if let Some(activity)=&activity { activity.as_str() } else if progress.attempt==0 { "Waiting for assistant…" } else if progress.text.is_empty() { "Thinking…" } else { "Responding…" };
                        self.live.activity(label,Some(progress));
                    }
                    protocol::ChatEvent::Complete { response } => return Ok(response),
                    protocol::ChatEvent::Error { message,refunded,steering } => {
                        if refunded {
                            self.saved.messages.extend(steering.into_iter().map(|input|Message { role:Role::User,text:input.text }));
                            let cwd=self.saved.request.as_ref().unwrap().cwd.clone();
                            self.saved.request=None;
                            if !self.controls.stopped.load(Ordering::SeqCst) { self.prepare(cwd,Uuid::new_v4())?; }
                            self.save()?;
                        }
                        bail!("{message}");
                    }
                }
            }
        }
        bail!("Connection ended before completion. Retry to recover the same request.")
    }

    /// Consume the exact displayed proposal once; callers never supply executable text.
    pub async fn decide(&mut self, id: Uuid, approve: bool) -> Result<Value> {
        let pending = self.saved.pending.as_ref().context("No command awaiting approval")?;
        ensure!(pending.id==id,"This approval is stale");
        ensure!(!approve || self.saved.settings.commands_enabled,"Command execution is disabled");
        let pending = self.saved.pending.take().unwrap();
        self.saved.messages.push(Message { role:Role::Tool,text:if approve {
            format!("User approved command: {}\nDirectory: {}\nExecution started. If no result follows, its outcome is unknown; do not automatically rerun.",pending.proposal.command,pending.proposal.cwd)
        } else { "The user declined the command. It was not executed.".into() } });
        if approve {
            self.saved.pending_output=Some(pending.id);
            self.save()?;
            self.live.activity(&format!("Running a local command: {}",pending.proposal.reason),None);
            self.processes.run(pending.id,&pending.proposal).await?;
        } else {
            self.prepare(pending.proposal.cwd,Uuid::new_v4())?;
        }
        if self.controls.stopped.load(Ordering::SeqCst) {
            if self.saved.pending_output.is_some() { self.finish_output()?; }
            self.saved.request=None;
            self.save()?;
            return Ok(self.state());
        }
        self.retry().await
    }

    /// Ask the backend to create a checkout for its configured credit package.
    pub async fn checkout(&self) -> Result<Value> {
        Self::response(self.backend(reqwest::Method::POST,"/v1/checkout").send().await?).await
    }
}

/// Allow unencrypted transport only to loopback development servers.
fn validate_backend(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    let local = matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"));
    if url.scheme()!="https" && !(local && url.scheme()=="http") { bail!("Backend must use HTTPS (HTTP is allowed only on localhost)"); }
    ensure!(url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path()=="/","Backend must be an origin without credentials or a path");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A partial preview never becomes a message or executable command, and retry retains its request ID.
    #[tokio::test]
    async fn interrupted_stream_retries_without_committing_preview() -> Result<()> {
        use axum::{routing::post,Json,Router};
        let requests=Arc::new(std::sync::Mutex::new(Vec::<Uuid>::new()));
        let seen=requests.clone();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(move |Json(request):Json<ChatRequest>| {
            let seen=seen.clone();
            async move {
                let mut requests=seen.lock().unwrap();
                requests.push(request.request_id);
                let event=if requests.len()==1 { protocol::ChatEvent::Progress { progress:protocol::Progress { attempt:1,activity:None,text:"Partial 世界".into(),summaries:vec![] } } }
                    else { protocol::ChatEvent::Complete { response:ChatResponse { reply:protocol::AssistantReply { text:"Complete 世界".into(),command:None },credits:8,revision:request.base+request.messages.len()+1,steering:vec![] } } };
                ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&event).unwrap()))
            }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let directory=tempfile::tempdir()?;
        let mut engine=Engine::open(directory.path())?;
        engine.set_backend(backend)?;
        engine.saved.token=Some("fixture".into());
        engine.settings(Character::Guide,true)?;
        assert!(engine.chat("Hello".into(),directory.path().to_string_lossy().into_owned()).await.is_err());
        assert_eq!(engine.saved.messages.len(),1);
        assert!(engine.saved.pending.is_none());
        assert_eq!(engine.live.0.borrow()["activity"]["progress"]["text"],"Partial 世界");
        engine.retry().await?;
        let requests=requests.lock().unwrap();
        assert_eq!(requests[0],requests[1]);
        assert_eq!(engine.saved.messages.len(),2);
        assert_eq!(engine.saved.messages[1].text,"Complete 世界");
        assert!(engine.saved.request.is_none());
        server.abort();
        Ok(())
    }
    /// Exercise the real shell through the same approval engine used by both frontends.
    #[tokio::test]
    async fn executes_only_current_approved_proposals() -> Result<()> {
        use axum::{routing::post, Json, Router};
        let dir = tempfile::tempdir()?;
        let mut engine = Engine::open(dir.path())?;
        let platform = engine.platform;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        engine.set_backend(format!("http://{}",listener.local_addr()?))?;
        let app = Router::new().route("/v1/chat",post(move |Json(request): Json<ChatRequest>| async move {
            assert_eq!(request.platform,platform);
            let command = (request.messages.last().unwrap().role==Role::User).then(|| CommandProposal { 
                command:match platform {
                    Platform::Linux => "printf 'approved\\n' >> approval.txt",
                    Platform::Windows => "Add-Content -Encoding UTF8 -LiteralPath 'approval.txt' -Value 'approved'",
                }.into(),cwd:request.cwd,reason:"Write a test marker".into(),
            });
            ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete { response:ChatResponse { reply:protocol::AssistantReply { text:"Test reply".into(),command },credits:10,revision:request.base+request.messages.len()+1,steering:vec![] } }).unwrap()))
        }));
        let server = tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        engine.saved.token = Some("test-session".into());
        engine.settings(Character::Guide,false)?;
        let cwd = dir.path().to_string_lossy().into_owned();
        engine.chat("Create marker".into(),cwd.clone()).await?;
        assert!(engine.saved.pending.is_none());
        assert!(!dir.path().join("approval.txt").exists());
        engine.settings(Character::Guide,true)?;
        engine.chat("Create marker".into(),cwd.clone()).await?;
        let id = engine.saved.pending.as_ref().unwrap().id;
        assert!(engine.decide(Uuid::new_v4(),true).await.is_err());
        engine.settings(Character::Guide,false)?;
        assert!(engine.decide(id,true).await.is_err());
        assert!(!dir.path().join("approval.txt").exists());
        engine.settings(Character::Guide,true)?;
        engine.chat("Create marker".into(),cwd.clone()).await?;
        engine.decide(engine.saved.pending.as_ref().unwrap().id,false).await?;
        assert!(!dir.path().join("approval.txt").exists());
        engine.chat("Create marker".into(),cwd).await?;
        let id = engine.saved.pending.as_ref().unwrap().id;
        assert!(!dir.path().join("approval.txt").exists());
        engine.decide(id,true).await?;
        assert!(engine.decide(id,true).await.is_err());
        let written = std::fs::read_to_string(dir.path().join("approval.txt"))?;
        assert_eq!(written.trim_start_matches('\u{feff}').trim(),"approved");
        assert!(engine.saved.messages.iter().any(|m|m.role==Role::Tool && m.text.starts_with("Exit status:")));
        assert_eq!(engine.state()["platform"],std::env::consts::OS);
        server.abort();
        Ok(())
    }

    /// An expired approval cannot run after permission was revoked or a chat was cleared.
    #[test]
    fn disabling_commands_invalidates_proposals() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut engine = Engine::open(dir.path())?;
        engine.saved.pending = Some(PendingCommand { id:Uuid::new_v4(),proposal:CommandProposal {  command:"true".into(),cwd:"/tmp".into(),reason:"test".into() } });
        engine.settings(Character::Guide,false)?;
        assert!(engine.saved.pending.is_none());
        assert!(!engine.state().to_string().contains("poll_secret"));
        assert!(Engine::open(dir.path())?.saved.pending.is_none());
        let conversation=engine.saved.conversation_id;
        assert_eq!(Engine::open(dir.path())?.saved.conversation_id,conversation);
        engine.new_chat()?;
        assert_ne!(engine.saved.conversation_id,conversation);
        assert_eq!(Engine::open(dir.path())?.saved.conversation_id,engine.saved.conversation_id);
        Ok(())
    }
    /// Growing visible history never increases the next request, and queued input survives a dequeue crash.
    #[tokio::test]
    async fn long_transcript_and_queue_recovery()->Result<()> {
        use axum::{routing::post,Json,Router};
        let directory=tempfile::tempdir()?;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(|Json(request):Json<ChatRequest>|async move {
            assert_eq!(request.messages.len(),1);
            assert!(serde_json::to_vec(&request).unwrap().len()<16_000);
            let response=ChatResponse { reply:protocol::AssistantReply { text:"Recorded".into(),command:None },credits:100,revision:request.base+2,steering:vec![] };
            ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete { response }).unwrap()))
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(directory.path())?;
        engine.set_backend(backend)?;
        engine.saved.token=Some("fixture".into());
        engine.settings(Character::Guide,false)?;
        let cwd=directory.path().to_string_lossy().into_owned();
        for _ in 0..30 { engine.chat("世界".repeat(2000),cwd.clone()).await?; }
        assert!(serde_json::to_vec(&engine.saved.messages)?.len()>96_000);
        assert_eq!(engine.saved.committed,60);
        let first=crate::controls::Queued { id:Uuid::new_v4(),text:"First queued message".into(),cwd:cwd.clone() };
        let second=crate::controls::Queued { id:Uuid::new_v4(),text:"Second queued message".into(),cwd:cwd.clone() };
        engine.controls.enqueue(first.clone())?;
        engine.controls.enqueue(second.clone())?;
        engine.prepare_message(first.text,cwd,first.id)?;
        drop(engine);
        let mut engine=Engine::open(directory.path())?;
        assert_eq!(engine.controls.next().unwrap().id,second.id);
        engine.retry().await?;
        assert!(engine.next().await?);
        assert!(engine.controls.next().is_none());
        assert_eq!(engine.saved.messages.len(),64);
        server.abort();
        Ok(())
    }

    /// Session-bearing traffic must never go to arbitrary plaintext origins.
    #[test]
    fn requires_encrypted_remote_backend() {
        assert!(validate_backend("http://example.com").is_err());
        assert!(validate_backend("https://user:password@example.com").is_err());
        assert!(validate_backend("http://127.0.0.1:8787").is_ok());
        assert!(validate_backend("https://assistant.example.com").is_ok());
    }
}
