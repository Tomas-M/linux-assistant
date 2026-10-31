mod rpc;
mod transport;
mod progress;
mod monitor;
mod accounts;
mod backup;
pub use backup::AccountBackup;
#[cfg(test)]
mod tests;

use std::{path::PathBuf, time::Duration,sync::{Arc,RwLock}};
use anyhow::{ensure, Context, Result};
use protocol::{AssistantReply, ChatRequest, CommandProposal, Platform};
#[cfg(test)]
use protocol::Role;
use serde_json::Value;
use tokio::sync::{Mutex,RwLock as AsyncRwLock};
use crate::{config::Config, db::{now, Db}};
use rpc::Rpc;

pub struct Pool { accounts:RwLock<Vec<Arc<ProviderAccount>>>, managed:PathBuf, login:Mutex<Option<accounts::Login>>, executable:PathBuf, selection:Mutex<()> }
struct ProviderAccount { home: String, access:AsyncRwLock<()>, worker: Mutex<Option<Rpc>>, limits: Mutex<Option<Limits>> }
#[derive(Clone, Copy, Debug)]
struct Limits { used:f64, retry_at:i64, checked_at:i64, api_key:bool }
pub struct Answer { pub reply: AssistantReply, pub account_home: Option<String>, pub thread_id: Option<String>, pub checkpoint: Option<String> }

impl Pool {
    /// Account directories are stable identities, so reordering configuration cannot reassign chats.
    pub fn new(config: &Config) -> Result<Self> {
        let managed=config.database.parent().unwrap().join("managed-accounts");
        let mut homes=Vec::new();
        #[cfg(test)] homes.extend(config.test_homes.clone());
        if managed.exists() {
            let mut saved=std::fs::read_dir(&managed)?.filter_map(|entry|entry.ok()).filter(|entry|entry.file_type().is_ok_and(|kind|kind.is_dir()) && entry.path().join("auth.json").is_file()).map(|entry|entry.path()).collect::<Vec<_>>();
            saved.sort(); homes.extend(saved);
        }
        let mut accounts=Vec::<Arc<ProviderAccount>>::new();
        for home in &homes {
            let home = home.canonicalize().context("Operator account directory does not exist")?.to_string_lossy().into_owned();
            ensure!(!accounts.iter().any(|account|account.home==home),"Duplicate operator account directory");
            if let Ok(auth)=std::fs::read(std::path::Path::new(&home).join("auth.json")) {
                if let Ok(auth)=serde_json::from_slice::<Value>(&auth) {
                    if let Some(key)=auth["OPENAI_API_KEY"].as_str() { crate::diagnostics::protect(key); }
                }
            }
            accounts.push(Arc::new(ProviderAccount { home,access:AsyncRwLock::new(()),worker:Mutex::new(None),limits:Mutex::new(None) }));
        }
        Ok(Self { accounts:RwLock::new(accounts),managed,login:Mutex::new(None),executable:std::env::current_exe()?,selection:Mutex::new(()) })
    }

    /// Snapshot shared account handles without holding the registry lock across network operations.
    fn accounts(&self)->Vec<Arc<ProviderAccount>> { self.accounts.read().unwrap().clone() }

    /// Retained handles cannot accidentally target another account when the registry changes.
    fn registered(&self,account:&Arc<ProviderAccount>)->bool { self.accounts.read().unwrap().iter().any(|saved|Arc::ptr_eq(saved,account)) }

    /// Lock only worker startup; each conversation then owns an independent session on that transport.
    async fn rpc(&self,account:&ProviderAccount)->Result<Rpc> {
        let mut worker=account.worker.lock().await;
        if worker.as_ref().is_none_or(|rpc|!rpc.alive()) { *worker=Some(Rpc::start(&self.executable,&account.home).await?); }
        Ok(worker.as_ref().unwrap().session())
    }

    /// Conversations run concurrently and move after an operator pause or a confirmed quota failure.
    #[cfg(test)]
    pub async fn answer(&self, config: &Config, db: &Db, user: i64, request: &ChatRequest) -> Result<Answer> {
        self.answer_stream(config,db,user,request,None,None).await
    }

    /// Stream preview snapshots while retaining the same affinity, usage and final validation path.
    pub async fn answer_stream(&self, config: &Config, db: &Db, user: i64, request: &ChatRequest, events: Option<&tokio::sync::watch::Sender<protocol::ChatEvent>>, mut controls: Option<&mut tokio::sync::mpsc::Receiver<crate::turns::Control>>) -> Result<Answer> {
        if config.dev {
            let reply=demo(request);
            if let Some(events)=events {
                let mut progress=protocol::Progress { compacted:None,attempt:1,activity:None,summaries:vec![protocol::ProgressText { id:"demo".into(),text:"Simulated progress: preparing a development response.".into() }],text:String::new() };
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
                                        let result=(|| {
                                            ensure!(db.balance(user)?>0,"Add credits to steer the conversation");
                                            db.accept_steering(user,request.request_id,&input)
                                        })().map_err(|error:anyhow::Error|error.to_string());
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
            let recovery=serde_json::json!({"summary":session.checkpoint,"messages":protocol::recovery_messages(&session.messages[session.checkpoint_at..])});
            let checkpoint=recovery.to_string().len()+serde_json::to_vec(&request.messages)?.len()>=64_000 || request.messages.iter().any(|message|message.attachments.is_some());
            let mut tried = Vec::new();
            let mut assigned = match &session.account_home {
                Some(home) => self.accounts().into_iter().find(|account|&account.home==home),
                None => None,
            };
            loop {
                let account = match assigned.take() {
                    Some(account) => account,
                    None => self.select(db,user,request,&tried).await?,
                };
                tried.push(account.home.clone());
                let _access=account.access.read().await;
                if !self.registered(&account) || self.paused(db,&account.home)? { continue; }
                self.probe(db,&account).await?;
                let limits=account.limits.lock().await.with_context(||format!("{}: account quota unavailable; see quota_refresh diagnostics",account.home))?;
                if limits.retry_at>now() { continue; }
                let mut rpc=self.rpc(&account).await?;
                let same_account = session.account_home.as_deref()==Some(account.home.as_str());
                let thread = if same_account { session.thread_id.as_deref() } else { None };
                db.bind_conversation(user,request.conversation_id,&account.home)?;
                let purpose="conversation";
                let mut attempt=crate::usage::Attempt::start(db,Some(user),request,&account.home,purpose)?;
                let mut reporter=progress::Reporter::new(events,tried.len());
                let mut current=request.clone();
                current.messages.extend(db.steering(user,request.request_id)?.into_iter().map(crate::turns::steering_message));
                let result=rpc.answer(config,&current,thread,&recovery,checkpoint,Some(user),controls.as_deref_mut(),&attempt,&mut reporter).await;
                let failure=result.as_ref().err().map(|error|crate::diagnostics::clean(&format!("{error:#}")));
                if let Some(message)=&failure { crate::diagnostics::record("error","generation",Some(&request.request_id.to_string()),Some(&account.home),message); }
                attempt.finish(failure.as_deref())?;
                match result {
                    Ok((reply,thread_id,checkpoint)) => {
                        return Ok(Answer { reply,account_home:Some(account.home.clone()),thread_id:Some(thread_id),checkpoint });
                    }
                    Err(error) if error.downcast_ref::<QuotaReached>().is_some() => {
                        let mut cache=account.limits.lock().await;
                        // API limits are enforced upstream and have no subscription-quota endpoint.
                        let mut limits = if limits.api_key { limits } else { match rpc.call("account/rateLimits/read",serde_json::json!({})).await.and_then(|value|Limits::read(&value)) {
                            Ok(value) => value,
                            Err(error) => {
                                crate::diagnostics::record("warning","quota_refresh",Some(&request.request_id.to_string()),Some(&account.home),&format!("Could not refresh quota after a usage-limit response: {error:#}"));
                                limits
                            }
                        } };
                        limits.retry_at = limits.retry_at.max(now()+60);
                        *cache=Some(limits);
                    }
                    Err(error) => return Err(error),
                }
            }
        }).await.context("AI request timed out")?
    }

    /// Rank fresh quota snapshots by the most depleted window, then spread ties across conversations.
    async fn select(&self, db: &Db, user: i64, request: &ChatRequest, tried: &[String]) -> Result<Arc<ProviderAccount>> {
        let _selection = self.selection.lock().await;
        let mut candidates = Vec::new();
        let mut excluded=Vec::new();
        for account in self.accounts() {
            if tried.contains(&account.home) { excluded.push(format!("{}: already tried for this request",account.home)); continue; }
            if self.paused(db,&account.home)? { excluded.push(format!("{}: paused by operator",account.home)); continue; }
            self.probe(db,&account).await?;
            if let Some(limits) = *account.limits.lock().await {
                if limits.retry_at<=now() { candidates.push((account.clone(),limits.api_key,limits.used,db.assigned_conversations(&account.home)?)); }
                else { excluded.push(format!("{}: quota/cooldown blocks selection until {} UTC (Unix time); {:.1}% used; checked at {}",account.home,limits.retry_at,limits.used,limits.checked_at)); }
            } else { excluded.push(format!("{}: quota unavailable; see quota_refresh diagnostics for this account",account.home)); }
        }
        // Prefer subscription allowance when both modes are enabled; pausing ChatGPT selects API access.
        let account = candidates.into_iter().min_by(|a,b|a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)).then(a.3.cmp(&b.3))).map(|c|c.0)
            .with_context(||format!("No selectable AI account. {}",if excluded.is_empty() { "No accounts configured".into() } else { excluded.join("; ") }))?;
        let _access=account.access.read().await;
        if self.registered(&account) { db.bind_conversation(user,request.conversation_id,&account.home)?; }
        Ok(account.clone())
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
        if value["authType"]=="apiKey" { return Ok(Self { used:0.0,retry_at:0,checked_at:now(),api_key:true }); }
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
        Ok(Self { used,retry_at,checked_at:now(),api_key:false })
    }
}



/// Deterministic development replies exercise local approval without consuming provider usage.
fn demo(request: &ChatRequest) -> AssistantReply {
    let last = request.messages.last().unwrap();
    let command = (request.commands_enabled && last.text.to_lowercase().contains("demo command")).then(|| CommandProposal { files:None,
        command:match request.platform { Platform::Linux => "printf 'Linux Assistant command test\\n'", Platform::Windows => "Write-Output 'Linux Assistant command test'" }.into(),
        cwd:request.cwd.clone(),reason:"Print a harmless test message on your computer.".into(),
    });
    AssistantReply { text:format!("Development mode (no AI connected). You said: {}\nEnable commands and ask for ‘demo command’ to test local approval.",last.text),command }
}
