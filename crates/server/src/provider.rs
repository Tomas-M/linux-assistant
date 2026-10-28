mod rpc;
mod progress;
mod monitor;
#[cfg(test)]
mod tests;

use std::{path::PathBuf, time::Duration};
use anyhow::{ensure, Context, Result};
use protocol::{AssistantReply, ChatRequest, CommandProposal, Platform, Role};
use serde_json::Value;
use tokio::sync::Mutex;
use crate::{config::Config, db::{now, Db}};
use rpc::Rpc;

pub struct Pool { accounts: Vec<ProviderAccount>, executable: PathBuf, selection: Mutex<()> }
struct ProviderAccount { home: String, worker: Mutex<Option<Rpc>>, limits: Mutex<Option<Limits>> }
#[derive(Clone, Copy, Debug)]
struct Limits { used: f64, retry_at: i64, checked_at: i64 }
pub struct Answer { pub reply: AssistantReply, pub account_home: Option<String>, pub thread_id: Option<String>, pub checkpoint: Option<String> }

impl Pool {
    /// Account directories are stable identities, so reordering configuration cannot reassign chats.
    pub fn new(config: &Config) -> Result<Self> {
        let mut accounts = Vec::new();
        for home in &config.codex_homes {
            let home = home.canonicalize().context("Operator account directory does not exist")?.to_string_lossy().into_owned();
            ensure!(!accounts.iter().any(|a: &ProviderAccount|a.home==home),"Duplicate operator account directory");
            accounts.push(ProviderAccount { home,worker:Mutex::new(None),limits:Mutex::new(None) });
        }
        Ok(Self { accounts,executable:std::env::current_exe()?,selection:Mutex::new(()) })
    }

    /// Different accounts run concurrently; a conversation only moves after a confirmed quota failure.
    #[cfg(test)]
    pub async fn answer(&self, config: &Config, db: &Db, user: i64, request: &ChatRequest) -> Result<Answer> {
        self.answer_stream(config,db,user,request,None,None).await
    }

    /// Stream preview snapshots while retaining the same affinity, usage and final validation path.
    pub async fn answer_stream(&self, config: &Config, db: &Db, user: i64, request: &ChatRequest, events: Option<&tokio::sync::watch::Sender<protocol::ChatEvent>>, mut controls: Option<&mut tokio::sync::mpsc::Receiver<crate::turns::Control>>) -> Result<Answer> {
        if config.dev {
            let reply=demo(request);
            if let Some(events)=events {
                let mut progress=protocol::Progress { attempt:1,activity:None,summaries:vec![protocol::ProgressText { id:"demo".into(),text:"Simulated progress: preparing a development response.".into() }],text:String::new() };
                events.send_replace(protocol::ChatEvent::Progress { progress:progress.clone() });
                for chunk in reply.text.chars().collect::<Vec<_>>().chunks(12) {
                    let delay=tokio::time::sleep(Duration::from_millis(40));
                    tokio::pin!(delay);
                    loop {
                        tokio::select! {
                            _=&mut delay=>break,
                            control=async { match controls.as_deref_mut() { Some(receiver)=>receiver.recv().await,None=>std::future::pending().await } }=> {
                                match control {
                                    Some(crate::turns::Control::Stop(reply))=> { let _=reply.send(Ok(())); return Err(crate::turns::Interrupted.into()); }
                                    Some(crate::turns::Control::Steer(input,reply))=> {
                                        let result=db.accept_steering(user,request.request_id,&input).map_err(|error|error.to_string());
                                        let _=reply.send(result);
                                    }
                                    None=>controls=None,
                                }
                            }
                        }
                    }
                    progress.text.extend(chunk);
                    events.send_replace(protocol::ChatEvent::Progress { progress:progress.clone() });
                }
            }
            return Ok(Answer { reply,account_home:None,thread_id:None,checkpoint:None });
        }
        tokio::time::timeout(Duration::from_secs(600),async {
            let session = db.conversation(user,request.conversation_id)?;
            ensure!(request.base==session.messages.len(),"Conversation revision changed");
            let recovery=serde_json::json!({"summary":session.checkpoint,"messages":session.messages[session.checkpoint_at..]});
            let checkpoint=recovery.to_string().len()+serde_json::to_vec(&request.messages)?.len()>=64_000;
            let mut tried = Vec::new();
            let mut assigned = match &session.account_home {
                Some(home) => Some(self.accounts.iter().position(|a|&a.home==home).context("Conversation account is no longer configured")?),
                None => None,
            };
            loop {
                let index = match assigned.take() {
                    Some(index) => index,
                    None => self.select(db,user,request,&tried).await?,
                };
                tried.push(index);
                let account = &self.accounts[index];
                // Taking ownership ensures cancellation kills an unfinished worker instead of reusing its RPC stream.
                let mut slot = tokio::time::timeout(Duration::from_secs(5),account.worker.lock()).await.context("Account is busy; retry shortly")?;
                let mut rpc = match slot.take() { Some(rpc) => rpc, None => Rpc::start(&self.executable,&account.home).await? };
                let cached=*account.limits.lock().await;
                let limits=match cached.filter(|limits|now()-limits.checked_at<300) {
                    Some(limits)=>limits,
                    None=>Limits::read(&rpc.call("account/rateLimits/read",serde_json::json!({})).await?)?,
                };
                *account.limits.lock().await = Some(limits);
                if limits.retry_at>now() { *slot=Some(rpc); continue; }
                let same_account = session.account_home.as_deref()==Some(account.home.as_str());
                let thread = if same_account { session.thread_id.as_deref() } else { None };
                db.bind_conversation(user,request.conversation_id,&account.home)?;
                let purpose="conversation";
                let mut attempt=crate::usage::Attempt::start(db,Some(user),request,&account.home,purpose)?;
                let mut reporter=progress::Reporter::new(events,tried.len());
                let mut current=request.clone();
                current.messages.extend(db.steering(user,request.request_id)?.into_iter().map(|input|protocol::Message { role:Role::User,text:input.text }));
                let result=rpc.answer(config,&current,thread,&recovery,checkpoint,Some(user),controls.as_deref_mut(),&attempt,&mut reporter).await;
                let failure=result.as_ref().err().map(|error| {
                    if error.downcast_ref::<QuotaReached>().is_some() { "ChatGPT quota reached" }
                    else if error.downcast_ref::<crate::turns::Interrupted>().is_some() { "Stopped by user; token counts may be partial" }
                    else { "Provider request failed" }
                });
                attempt.finish(failure)?;
                match result {
                    Ok((reply,thread_id,checkpoint)) => {
                        *slot=Some(rpc);
                        return Ok(Answer { reply,account_home:Some(account.home.clone()),thread_id:Some(thread_id),checkpoint });
                    }
                    Err(error) if error.downcast_ref::<QuotaReached>().is_some() => {
                        let mut limits = match rpc.call("account/rateLimits/read",serde_json::json!({})).await {
                            Ok(value) => Limits::read(&value).unwrap_or(limits),
                            Err(_) => limits,
                        };
                        limits.retry_at = limits.retry_at.max(now()+60);
                        *account.limits.lock().await = Some(limits);
                    }
                    Err(error) => return Err(error),
                }
            }
        }).await.context("AI request timed out")?
    }

    /// Rank fresh quota snapshots by the most depleted window, then spread ties across conversations.
    async fn select(&self, db: &Db, user: i64, request: &ChatRequest, tried: &[usize]) -> Result<usize> {
        let _selection = self.selection.lock().await;
        let mut candidates = Vec::new();
        for (index,account) in self.accounts.iter().enumerate() {
            if tried.contains(&index) { continue; }
            let cached = *account.limits.lock().await;
            if cached.is_some_and(|limits|limits.retry_at>now()) { continue; }
            if let Ok(mut slot) = account.worker.try_lock() {
                if cached.is_none_or(|limits|now()-limits.checked_at>=300) {
                    let probe = async {
                        let mut rpc = match slot.take() { Some(rpc) => rpc, None => Rpc::start(&self.executable,&account.home).await? };
                        let limits = Limits::read(&rpc.call("account/rateLimits/read",serde_json::json!({})).await?)?;
                        *slot = Some(rpc);
                        Ok::<_,anyhow::Error>(limits)
                    };
                    match tokio::time::timeout(Duration::from_secs(20),probe).await {
                        Ok(Ok(limits)) => *account.limits.lock().await=Some(limits),
                        _ => { *account.limits.lock().await=None; continue; }
                    }
                }
            }
            if let Some(limits) = *account.limits.lock().await {
                if limits.retry_at<=now() { candidates.push((index,limits.used,db.assigned_conversations(&account.home)?)); }
            }
        }
        let index = candidates.into_iter().min_by(|a,b|a.1.total_cmp(&b.1).then(a.2.cmp(&b.2))).map(|c|c.0)
            .context("No authenticated account with available, reported quota; retry after a reset or sign in again")?;
        db.bind_conversation(user,request.conversation_id,&self.accounts[index].home)?;
        Ok(index)
    }
}

#[derive(Debug)]
struct QuotaReached;
impl std::fmt::Display for QuotaReached {
    /// Only a typed provider quota response authorizes automatic account switching.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f,"ChatGPT usage limit reached") }
}
impl std::error::Error for QuotaReached {}

impl Limits {
    /// Unknown limits are not treated as unused quota; the latest exhausted reset controls eligibility.
    fn read(value: &Value) -> Result<Self> {
        let limits = match value.get("rateLimitsByLimitId").filter(|v|v.is_object()) {
            Some(buckets) => &buckets["codex"], None => &value["rateLimits"],
        };
        let mut windows = Vec::new();
        let mut retry_at = 0;
        for name in ["primary","secondary"] {
            if let Some(used) = limits[name]["usedPercent"].as_f64() {
                ensure!(used.is_finite() && (0.0..=100.0).contains(&used),"Invalid quota percentage");
                let reset = limits[name]["resetsAt"].as_i64();
                if used>=100.0 { retry_at=retry_at.max(reset.unwrap_or(now()+60).max(now()+1)); }
                windows.push(used);
            }
        }
        if limits["rateLimitReachedType"].is_string() { retry_at=retry_at.max(now()+60); }
        let used = windows.into_iter().reduce(f64::max).context("Codex did not report its usage windows")?;
        Ok(Self { used,retry_at,checked_at:now() })
    }
}

/// Authenticate one operator account using the embedded runtime without installing the Codex CLI.
pub async fn login(home: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&home)?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&home,std::fs::Permissions::from_mode(0o700))?; }
    let home = home.canonicalize()?;
    let mut rpc = Rpc::start_unauthenticated(&std::env::current_exe()?,&home.to_string_lossy()).await?;
    let login = rpc.call("account/login/start",serde_json::json!({"type":"chatgptDeviceCode"})).await?;
    println!("Open {} and enter code {}.",login["verificationUrl"].as_str().context("Missing verification URL")?,
        login["userCode"].as_str().context("Missing sign-in code")?);
    tokio::time::timeout(Duration::from_secs(600),async {
        loop {
            let event = rpc.next().await?;
            if event["method"]=="account/login/completed" {
                ensure!(event["params"]["success"]==true,"Operator sign-in failed");
                println!("Operator account saved in {}",home.display());
                return Ok::<_,anyhow::Error>(());
            }
        }
    }).await.context("Operator sign-in timed out")?
}

/// Deterministic development replies exercise local approval without consuming provider usage.
fn demo(request: &ChatRequest) -> AssistantReply {
    let last = request.messages.last().unwrap();
    let command = (request.commands_enabled && last.text.to_lowercase().contains("demo command")).then(|| CommandProposal { 
        command:match request.platform { Platform::Linux => "printf 'Linux Assistant command test\\n'", Platform::Windows => "Write-Output 'Linux Assistant command test'" }.into(),
        cwd:request.cwd.clone(),reason:"Print a harmless test message on your computer.".into(),
    });
    AssistantReply { text:format!("Development mode (no AI connected). You said: {}\nEnable commands and ask for ‘demo command’ to test local approval.",last.text),command }
}
