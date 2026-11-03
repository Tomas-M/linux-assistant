use std::{path::{Path, PathBuf}, sync::{Arc,atomic::{AtomicBool,Ordering}}};
use anyhow::{bail, ensure, Context, Result};
use protocol::{Account, EmailChallenge, EmailLogin, VerifyEmail, ChatRequest, ChatResponse, DeviceLogin, LoginResult, Message, Platform, Role, MAX_REQUEST_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use crate::{processes::Processes, storage};
pub(crate) mod output;
mod presentation;
mod generated;
mod social;

#[derive(Clone, Deserialize, Serialize)]
pub struct Settings { pub backend: String, pub commands_enabled: bool }
#[derive(Clone, Deserialize, Serialize)]
pub struct Saved {
    pub conversation_id: Uuid,
    pub task_id: Option<Uuid>,
    pub settings: Settings,
    pub token: Option<String>,
    pub messages: Vec<Message>,
    pub committed: usize,
    pub request: Option<ChatRequest>,
}
pub struct Engine { pub saved: Saved, pub controls: Arc<crate::controls::Controls>, pub processes: Processes, pub live: crate::stream::Live, pub full_access: Arc<AtomicBool>, platform: Platform, path: PathBuf, http: reqwest::Client, login: Option<social::LoginAttempt>, pub account: Arc<std::sync::RwLock<Option<Account>>> }

impl Engine {
    /// One interface owns the saved settings and conversation state at a time.
    pub async fn open(directory: &Path, backend:&str) -> Result<Self> {
        let platform = match std::env::consts::OS {
            "linux" => Platform::Linux, "windows" => Platform::Windows,
            name => bail!("Unsupported operating system: {name}"),
        };
        let path = directory.join("state.json");
        let saved = if path.exists() { storage::read(&path)? } else {
            Saved { conversation_id:Uuid::new_v4(),settings:Settings { backend:backend.into(),commands_enabled:true },
                task_id:None,token:None,committed:0,messages:vec![],request:None }
        };
        validate_backend(&saved.settings.backend)?;
        for (index,message) in saved.messages.iter().enumerate() { crate::media::cache_attachments(directory,&format!("{}_{index}",saved.conversation_id),message.attachments.as_deref())?; }
        let live=crate::stream::Live::new();
        let presentation=directory.join("presentation.json");
        if presentation.exists() {
            let presentation:Value=storage::read(&presentation)?;
            if presentation["conversation_id"]==saved.conversation_id.to_string() {
                ensure!(presentation["tool_results"].is_object(),"Saved chat format changed; stop the client and run it once with --reset-chat (sign-in and settings are retained)");
                live.0.send_modify(|view| { for key in ["steps","runs","commands","tool_results"] { view[key]=presentation[key].clone(); } });
            }
        }
        let controls=crate::controls::Controls::open(directory,live.clone())?;
        let mut engine=Self { controls,processes:Processes::open(directory)?,live,full_access:Arc::new(AtomicBool::new(saved.settings.commands_enabled)),saved,platform,path,http:crate::backend::client(0)?,login:None,account:Arc::new(std::sync::RwLock::new(None)) };
        if let Some(request)=&engine.saved.request { engine.controls.remove(request.request_id).await?; }
        if engine.saved.settings.backend!=backend { engine.set_backend(backend.into()).await?; }
        engine.live.state(engine.state());
        Ok(engine)
    }

    /// Never expose authentication tokens or pending request internals to browser state.
    pub fn state(&self) -> Value {
        json!({"platform":self.platform,"shell":self.platform.shell(),"settings":self.saved.settings,"full_access":self.full_access.load(Ordering::SeqCst),"authenticated":self.saved.token.is_some(),"messages":crate::media::messages(self.saved.conversation_id,&self.saved.messages),
            "sign_in":self.login.as_ref().filter(|login|login.provider.is_some()).map(|login|json!({"id":login.device.device_id,"provider":login.provider,"expires_at":login.device.expires_at})),
            "retry_available":self.saved.request.is_some(),"account":&*self.account.read().unwrap()})
    }

    /// Keep all state transitions durable before performing external side effects.
    fn save(&self) -> Result<()> {
        for (index,message) in self.saved.messages.iter().enumerate() { crate::media::cache_attachments(self.path.parent().unwrap(),&format!("{}_{index}",self.saved.conversation_id),message.attachments.as_deref())?; }
        storage::write(&self.path,&self.saved)?;
        let view=self.live.0.borrow();
        storage::write(&self.path.with_file_name("presentation.json"),&json!({"conversation_id":self.saved.conversation_id,"steps":view["steps"],"runs":view["runs"],"commands":view["commands"],"tool_results":view["tool_results"]}))?;
        drop(view);
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
    pub(super) async fn response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(status.is_success(),"{}",value["error"].as_str().unwrap_or("Backend request failed"));
        Ok(serde_json::from_value(value)?)
    }

    /// Changing the backend signs out and clears conversation state to prevent cross-server leaks.
    pub async fn set_backend(&mut self, backend: String) -> Result<()> {
        let backend = backend.trim_end_matches('/').to_owned();
        validate_backend(&backend)?;
        self.saved.settings.backend = backend;
        self.saved.token = None;
        self.saved.settings.commands_enabled = true;
        self.full_access.store(true,Ordering::SeqCst);
        *self.account.write().unwrap()=None;
        self.login = None;
        self.new_chat().await
    }

    /// Disabling execution immediately invalidates every unapproved command.
    pub fn settings(&mut self, enabled: bool) -> Result<()> {
        self.saved.settings.commands_enabled = enabled;
        if !enabled { self.full_access.store(false,Ordering::SeqCst); }
        self.save()
    }

    /// Request a code without opening a browser or exposing the device secret to the UI.
    pub async fn login(&mut self,email:String) -> Result<EmailChallenge> {
        let login=if let Some(login)=self.login.as_ref().filter(|login|login.provider.is_some()) { login.device.clone() }
            else { Self::response::<DeviceLogin>(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/device"),&self.live,None).await?).await? };
        let input=EmailLogin { device_id:login.device_id.clone(),poll_secret:login.poll_secret.clone(),email };
        let challenge=Self::response(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/email").json(&input),&self.live,None).await?).await?;
        let provider=self.login.as_ref().and_then(|login|login.provider);
        self.login=Some(social::LoginAttempt { device:login,provider });
        Ok(challenge)
    }

    /// Verify inside the client and persist the session before loading account details.
    pub async fn verify_login(&mut self,code:String) -> Result<Value> {
        let login=&self.login.as_ref().context("Request a sign-in code first")?.device;
        let input=VerifyEmail { device_id:login.device_id.clone(),poll_secret:login.poll_secret.clone(),code };
        let result:LoginResult=Self::response(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/verify").json(&input),&self.live,None).await?).await?;
        self.finish_login(result).await
    }

    /// Finish email and provider sign-ins through the same durable session transition.
    async fn finish_login(&mut self,result:LoginResult)->Result<Value> {
        self.saved.token=Some(result.token.context("Backend did not issue a session")?);
        self.login=None;
        *self.account.write().unwrap()=None;
        self.saved.settings.commands_enabled=true;
        self.full_access.store(true,Ordering::SeqCst);
        self.new_chat().await?;
        self.refresh_account().await
    }

    /// Forget only the rejected session, leaving newer logins and the access preference untouched.
    pub async fn expire_session(&mut self,expired:&crate::backend::SessionExpired)->Result<()> {
        if self.saved.token.as_deref()!=Some(expired.0.as_str()) { return Ok(()); }
        self.controls.cancel_local();
        self.saved.token=None;
        self.login=None;
        *self.account.write().unwrap()=None;
        self.new_chat().await
    }

    /// Apply authentication failures to the durable session before publishing another state snapshot.
    async fn send_authenticated(&mut self,request:reqwest::RequestBuilder,interruptible:bool)->Result<reqwest::Response> {
        let result=crate::backend::send(request,&self.live,interruptible.then_some(self.controls.stopped.as_ref())).await;
        if let Err(error)=&result {
            if let Some(expired)=error.downcast_ref::<crate::backend::SessionExpired>() { self.expire_session(expired).await?; }
        }
        result
    }

    /// Query the authoritative balance rather than estimating provider usage locally.
    pub async fn refresh_account(&mut self) -> Result<Value> {
        let response=self.send_authenticated(self.backend(reqwest::Method::GET,"/v1/me"),false).await?;
        let account=Self::response(response).await?;
        *self.account.write().unwrap()=Some(account);
        self.live.state(self.state());
        Ok(self.state())
    }

    /// Revoke server access before clearing the local session.
    pub async fn logout(&mut self) -> Result<()> {
        let response = crate::backend::send(self.backend(reqwest::Method::POST,"/v1/logout"),&self.live,None).await?;
        if response.status()!=reqwest::StatusCode::UNAUTHORIZED { Self::response::<Value>(response).await?; }
        self.saved.token = None;
        self.saved.settings.commands_enabled = false;
        self.full_access.store(false,Ordering::SeqCst);
        *self.account.write().unwrap()=None;
        self.new_chat().await
    }

    /// A fresh chat clears pending approvals and retry state, keeping account settings.
    pub async fn new_chat(&mut self) -> Result<()> {
        self.live.0.send_modify(|view| { for key in ["steps","runs","commands","steering","interactions"] { view[key]=json!([]); } view["tool_results"]=json!({}); view["activity"]=Value::Null; view["error"]=Value::Null; });
        self.saved.conversation_id = Uuid::new_v4();
        self.saved.task_id = None;
        self.saved.messages.clear();
        self.saved.committed=0;
        self.controls.clear().await?;
        self.saved.request = None;
        self.save()
    }

    /// Persist user input before dispatch; a lost response can be retried with the same request ID.
    fn prepare_message(&mut self,text:String,cwd:String,id:Uuid,attachments:Option<Vec<protocol::Attachment>>)->Result<()> {
        ensure!(self.saved.request.is_none(),"Retry the unfinished request or start a new chat first");
        protocol::validate_input(&text,attachments.as_deref())?;
        self.saved.messages.push(Message { attachments, role:Role::User,text });
        self.saved.task_id=Some(id);
        if let Err(error)=self.prepare(cwd,id) { self.saved.messages.pop(); self.saved.task_id=None; return Err(error); }
        self.begin_activity(self.saved.messages.len()-1);
        self.save()?;
        Ok(())
    }
    /// Tests and direct engine callers use the same durable input path as queued browser messages.
    #[cfg(test)]
    pub async fn chat(&mut self,text:String,cwd:String)->Result<Value> {
        self.prepare_message(text,cwd,Uuid::new_v4(),None)?;
        self.retry().await
    }
    /// Acknowledge dequeue only after saving its stable ID, making crash recovery unambiguous.
    pub async fn next(&mut self)->Result<bool> {
        if !self.controls.clone().start_next(|input|self.prepare_message(input.text,input.cwd,input.id,input.attachments)).await? { return Ok(false); }
        self.retry().await?;
        Ok(true)
    }

    /// Send only new input after the last committed conversation revision.
    fn prepare(&mut self, cwd: String, id:Uuid) -> Result<()> {
        ensure!(self.saved.token.is_some(),"Sign in first");
        let request = ChatRequest { task_id:*self.saved.task_id.get_or_insert(id),base:self.saved.committed,request_id:id,conversation_id:self.saved.conversation_id,platform:self.platform,messages:self.saved.messages[self.saved.committed..].to_vec(),commands_enabled:self.saved.settings.commands_enabled,cwd };
        ensure!(serde_json::to_vec(&request)?.len()<=protocol::MAX_UPLOAD_BYTES && protocol::display_messages(&request.messages).to_string().len()<=MAX_REQUEST_BYTES,"New input exceeds the per-request limit");
        self.saved.request = Some(request);
        self.save()
    }

    /// Retain ambiguous requests for idempotent recovery; start a new task after a settled failure.
    pub async fn retry(&mut self) -> Result<Value> {
        let request = self.saved.request.as_ref().context("No unfinished request")?;
        self.begin_activity(self.saved.messages.len());
        self.record_credits(None);
        let _active=self.controls.active(crate::controls::Remote { backend:self.saved.settings.backend.clone(),token:self.saved.token.clone().unwrap(),request:request.request_id });
        self.live.activity("Waiting for assistant…",Some(Default::default()));
        let response=self.send_authenticated(self.backend(reqwest::Method::POST,"/v1/chat").header(reqwest::header::ACCEPT,"text/event-stream").json(request),true).await?;
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE || status == reqwest::StatusCode::PAYMENT_REQUIRED {
                let id=Uuid::new_v4();
                self.saved.task_id=Some(id);
                self.saved.request.as_mut().unwrap().request_id=id;
                self.saved.request.as_mut().unwrap().task_id=id;
                self.save()?;
            }
            return Self::response::<Value>(response).await;
        }
        let result=self.read_stream(response).await?;
        let expected=self.saved.messages.len()+result.steering.len()+1;
        ensure!(result.revision==expected,"Backend returned an unexpected conversation revision");
        // Completion is authoritative even if the separate steering acknowledgement was lost.
        let steering_ids:Vec<_>=result.steering.iter().map(|input|input.id).collect();
        let steering_start=self.saved.messages.len();
        for id in &steering_ids { self.controls.remove(*id).await?; }
        self.saved.messages.extend(result.steering.into_iter().map(|input|Message { attachments:input.attachments, role:Role::User,text:input.text }));
        self.saved.messages.push(Message { attachments:None, role:Role::Assistant,text:result.reply.text });
        self.saved.committed=result.revision;
        self.saved.request = None;
        self.saved.task_id=None;
        if let Some(account)=self.account.write().unwrap().as_mut() { account.credits=result.credits; }
        if !steering_ids.is_empty() { self.live.committed_steering(self.state(),&steering_ids,steering_start); }
        self.finish_activity(None)?;
        Ok(self.state())
    }

    /// Commit only a terminal, validated reply; an interrupted preview retains its original request ID.
    async fn read_stream(&mut self, mut response: reqwest::Response) -> Result<ChatResponse> {
        ensure!(response.headers().get(reqwest::header::CONTENT_TYPE).is_some_and(|value|value.as_bytes().starts_with(b"text/event-stream")),"Backend does not support streaming; update the backend first");
        let mut decoder=crate::stream::Decoder::default();
        let mut failures=self.controls.failures.subscribe();
        let mut reconnects=0;
        loop {
            // A control request can fail before the stream subscribes, so also inspect the current value.
            let failure=failures.borrow_and_update().clone();
            if let Some(error)=failure {
                if let Some(expired)=error.downcast_ref::<crate::backend::SessionExpired>() {
                    self.expire_session(expired).await?;
                    return Err(expired.clone().into());
                }
                bail!("{error:#}");
            }
            let bytes=tokio::select! {
                chunk=response.chunk()=>chunk,
                _=failures.changed()=>continue,
            };
            let bytes=match bytes {
                Ok(Some(bytes))=>bytes,
                _=> {
                    // Reattach to the same paid request; never create or execute a replacement task.
                    loop {
                        ensure!(reconnects<5,"Connection lost after 5 reconnect attempts. Retry to recover the same request.");
                        reconnects+=1;
                        self.live.0.send_modify(|view|view["connections"]["stream"]=json!({"attempt":reconnects,"retry_at":null}));
                        let result=async {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            ensure!(!self.controls.stopped.load(Ordering::SeqCst),"Stopped by you");
                            let request=self.saved.request.as_ref().context("No unfinished request")?;
                            let response=self.backend(reqwest::Method::POST,"/v1/chat").header(reqwest::header::ACCEPT,"text/event-stream").json(request).send().await?;
                            if response.status()==reqwest::StatusCode::UNAUTHORIZED {
                                let expired=crate::backend::SessionExpired(self.saved.token.clone().unwrap());
                                self.expire_session(&expired).await?;
                                return Err(expired.into());
                            }
                            ensure!(response.status().is_success(),"Cannot reconnect to the assistant (HTTP {})",response.status());
                            ensure!(response.headers().get(reqwest::header::CONTENT_TYPE).is_some_and(|value|value.as_bytes().starts_with(b"text/event-stream")),"Backend did not return a conversation stream");
                            Ok::<_,anyhow::Error>(response)
                        }.await;
                        self.live.0.send_modify(|view| { view["connections"].as_object_mut().unwrap().remove("stream"); });
                        match result {
                            Ok(connected)=> { response=connected; decoder=crate::stream::Decoder::default(); break; }
                            Err(error) if error.downcast_ref::<crate::backend::SessionExpired>().is_some() || self.controls.stopped.load(Ordering::SeqCst)=>return Err(error),
                            Err(error) if reconnects==5=>return Err(error.context("Cannot reconnect after 5 attempts")),
                            Err(_)=>{},
                        }
                    }
                    continue;
                }
            };
            for event in decoder.push(&bytes)? {
                match serde_json::from_value::<protocol::ChatEvent>(event)? {
                    protocol::ChatEvent::Progress { mut progress } => {
                        self.cache_generated(&mut progress).await?;
                        self.live.0.send_modify(|view|view["can_steer"]=json!(!self.controls.stopped.load(Ordering::SeqCst)));
                        let activity=progress.activity.clone();
                        let label=if let Some(activity)=&activity { activity.as_str() } else if progress.attempt==0 { "Waiting for assistant…" } else if progress.text.is_empty() { "Thinking…" } else { "Responding…" };
                        if let Some(credits)=progress.credits { if let Some(account)=self.account.write().unwrap().as_mut() { account.credits=credits; } self.live.state(self.state()); }
                        let request=self.saved.request.as_ref().unwrap();
                        self.controls.receive(&progress.requests,&self.processes,&self.full_access,self.saved.settings.commands_enabled,&request.cwd);
                        self.live.activity(label,Some(progress));
                    }
                    protocol::ChatEvent::Complete { mut response } => {
                        if let Some(mut progress)=response.progress.take() { self.cache_generated(&mut progress).await?; self.live.activity("Responding…",Some(progress)); }
                        self.record_credits(response.task_credits);
                        return Ok(response);
                    },
                    protocol::ChatEvent::Error { message,settled,credits,task_credits,steering,progress } => {
                        self.record_credits(task_credits);
                        if let Some(mut progress)=progress { self.cache_generated(&mut progress).await?; self.live.activity("Thinking…",Some(progress)); }
                        if let (Some(account),Some(credits))=(self.account.write().unwrap().as_mut(),credits) { account.credits=credits; }
                        if settled {
                            let steering_ids:Vec<_>=steering.iter().map(|input|input.id).collect();
                            let steering_start=self.saved.messages.len();
                            for id in &steering_ids { self.controls.remove(*id).await?; }
                            self.saved.messages.extend(steering.into_iter().map(|input|Message { attachments:input.attachments, role:Role::User,text:input.text }));
                            let cwd=self.saved.request.as_ref().unwrap().cwd.clone();
                            self.saved.request=None;
                            self.saved.task_id=None;
                            if !steering_ids.is_empty() { self.live.committed_steering(self.state(),&steering_ids,steering_start); }
                            if !self.controls.stopped.load(Ordering::SeqCst) { self.prepare(cwd,Uuid::new_v4())?; }
                            self.save()?;
                        }
                        bail!("{message}");
                    }
                }
            }
        }

    }


}

/// Normalize a required launch argument to a validated backend origin.
pub fn normalize_backend(value:&str)->Result<String> {
    let value=value.trim();
    ensure!(!value.is_empty(),"Provide a backend server address");
    let value=if value.contains("://") { value.to_owned() } else { format!("https://{value}") };
    validate_backend(&value)?;
    Ok(reqwest::Url::parse(&value)?.origin().ascii_serialization())
}

/// Allow unencrypted transport only to loopback development servers.
fn validate_backend(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    ensure!(url.host_str().is_some(),"Backend must have a hostname");
    let local = matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"));
    if url.scheme()!="https" && !(local && url.scheme()=="http") { bail!("Backend must use HTTPS (HTTP is allowed only on localhost)"); }
    ensure!(url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && url.path()=="/","Backend must be an origin without credentials or a path");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queued attachments survive an interrupted request and reload without leaking into live snapshots.
    #[tokio::test]
    async fn attachments_survive_retry()->Result<()> {
        use axum::{routing::post,Json,Router};
        let requests=Arc::new(std::sync::Mutex::new(Vec::<ChatRequest>::new()));
        let seen=requests.clone();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(move |Json(request):Json<ChatRequest>| {
            let mut requests=seen.lock().unwrap(); requests.push(request.clone());
            let event=if requests.len()==1 { protocol::ChatEvent::Progress { progress:Default::default() } }
                else { protocol::ChatEvent::Complete { response:ChatResponse { task_credits:Some(1), progress:Some(protocol::Progress { compacted:Some(vec!["compact".into()]),..Default::default() }),reply:protocol::AssistantReply { text:"Read".into() },credits:99,revision:request.base+2,steering:vec![] } } };
            async move { ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&event).unwrap())) }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let directory=tempfile::tempdir()?;
        let mut engine=Engine::open(directory.path(),&backend).await?; engine.saved.token=Some("fixture".into());
        let attachments=Some(vec![protocol::Attachment::Text { name:"日志.txt".into(),content:"PRIVATE_ATTACHMENT_TEXT".into() }]);
        engine.controls.enqueue(crate::controls::Queued { id:Uuid::new_v4(),text:String::new(),cwd:"/tmp".into(),editing:false,attachments:attachments.clone() }).await?;
        assert!(!engine.live.0.borrow().to_string().contains("PRIVATE_ATTACHMENT_TEXT"));
        assert!(engine.next().await.is_err()); drop(engine);
        let mut engine=Engine::open(directory.path(),&backend).await?; engine.retry().await?;
        assert!(!engine.live.0.borrow().to_string().contains("PRIVATE_ATTACHMENT_TEXT"));
        assert_eq!(engine.saved.messages[0].attachments,attachments);
        assert_eq!(engine.live.0.borrow()["steps"][0]["compacted"][0],"compact");
        let requests=requests.lock().unwrap(); assert_eq!(requests[0].request_id,requests[1].request_id); assert_eq!(requests[1].messages[0].attachments,attachments);
        server.abort(); Ok(())
    }

    /// A settled failure updates an overdraft immediately and retries as a newly admitted task.
    #[tokio::test]
    async fn failed_task_keeps_balance_and_starts_a_new_identity()->Result<()> {
        use axum::{routing::post,Json,Router};
        let ids=Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen=ids.clone();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(move |Json(request):Json<ChatRequest>| {
            seen.lock().unwrap().push((request.task_id,request.request_id));
            async move {
                let event=protocol::ChatEvent::Error { task_credits:Some(2), progress:None,message:"Stopped".into(),settled:true,credits:Some(-2),steering:vec![] };
                ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&event).unwrap()))
            }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let directory=tempfile::tempdir()?;
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.saved.token=Some("fixture".into());
        *engine.account.write().unwrap()=Some(Account { email:"fixture@example.test".into(),credits:1,development:true,input_credits_per_million:200.0 });
        assert!(engine.chat("Hello".into(),"/tmp".into()).await.is_err());
        assert_eq!(engine.state()["account"]["credits"],-2);
        let next=engine.saved.request.as_ref().unwrap();
        assert_eq!(next.task_id,next.request_id);
        assert_ne!(next.task_id,ids.lock().unwrap()[0].0);
        drop(engine);
        let recovered=Engine::open(directory.path(),&backend).await?;
        assert_eq!(recovered.saved.task_id,Some(recovered.saved.request.as_ref().unwrap().task_id));
        server.abort();
        Ok(())
    }

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
                let event=if requests.len()==1 { protocol::ChatEvent::Progress { progress:protocol::Progress { credits:None,requests:vec![],compacted:None,attempt:1,activity:None,text:"Partial 世界".into(),summaries:vec![] } } }
                    else { protocol::ChatEvent::Complete { response:ChatResponse { task_credits:Some(1), progress:None, reply:protocol::AssistantReply { text:"Complete 世界".into() },credits:8,revision:request.base+request.messages.len()+1,steering:vec![] } } };
                ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&event).unwrap()))
            }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let directory=tempfile::tempdir()?;
        let mut engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        engine.set_backend(backend.clone()).await?;
        engine.saved.token=Some("fixture".into());
        engine.settings(true)?;
        assert!(engine.chat("Hello".into(),directory.path().to_string_lossy().into_owned()).await.is_err());
        assert_eq!(engine.saved.messages.len(),1);
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


    /// An expired approval cannot run after permission was revoked or a chat was cleared.

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
            let response=ChatResponse { task_credits:Some(1), progress:None, reply:protocol::AssistantReply { text:"Recorded".into() },credits:100,revision:request.base+2,steering:vec![] };
            ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete { response }).unwrap()))
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        engine.set_backend(backend.clone()).await?;
        engine.saved.token=Some("fixture".into());
        engine.settings(false)?;
        let cwd=directory.path().to_string_lossy().into_owned();
        for _ in 0..30 { engine.chat("世界".repeat(2000),cwd.clone()).await?; }
        let runs=engine.live.0.borrow()["runs"].clone();
        assert_eq!(runs.as_array().unwrap().len(),30);
        assert!(runs.as_array().unwrap().iter().all(|run|run["finished_at"].as_u64()>=run["started_at"].as_u64() && run["finished_at"].is_u64()));
        assert_eq!(runs[29]["end"],engine.saved.messages.len());
        assert_eq!(Engine::open(directory.path(),&backend).await?.live.0.borrow()["runs"],runs);
        assert!(serde_json::to_vec(&engine.saved.messages)?.len()>96_000);
        assert_eq!(engine.saved.committed,60);
        let first=crate::controls::Queued { attachments:None, id:Uuid::new_v4(),text:"First queued message".into(),cwd:cwd.clone(),editing:false };
        let second=crate::controls::Queued { attachments:None, id:Uuid::new_v4(),text:"Second queued message".into(),cwd:cwd.clone(),editing:false };
        engine.controls.enqueue(first.clone()).await?;
        engine.controls.enqueue(second.clone()).await?;
        engine.prepare_message(first.text,cwd,first.id,None)?;
        drop(engine);
        let mut engine=Engine::open(directory.path(),&backend).await?;
        assert_eq!(engine.live.0.borrow()["queue"][0]["id"],second.id.to_string());
        engine.retry().await?;
        assert!(engine.next().await?);
        assert!(engine.live.0.borrow()["queue"].as_array().unwrap().is_empty());
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

    /// A terminal reply reconciles a queued steer whose acknowledgement was lost in transit.
    #[tokio::test]
    async fn confirmed_steering_is_not_dispatched_again()->Result<()> {
        use axum::{routing::post,Json,Router};
        let directory=tempfile::tempdir()?;
        let id=Uuid::new_v4();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(move |Json(request):Json<ChatRequest>|async move {
            let response=ChatResponse { task_credits:Some(1), progress:None, reply:protocol::AssistantReply { text:"Done".into() },credits:10,revision:request.base+request.messages.len()+2,
                steering:vec![protocol::Steering { attachments:None, id,text:"Queued steering".into() }] };
            ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete { response }).unwrap()))
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.saved.token=Some("fixture".into());
        engine.controls.enqueue(crate::controls::Queued { attachments:None, id,text:"Queued steering".into(),cwd:"/tmp".into(),editing:false }).await?;
        engine.chat("Original task".into(),"/tmp".into()).await?;
        assert!(!engine.next().await?);
        assert_eq!(engine.saved.messages.len(),3);
        assert_eq!(engine.saved.messages[1].text,"Queued steering");
        server.abort();
        Ok(())
    }
}
