use std::{collections::HashMap, path::Path, sync::{Arc,Mutex}, time::{SystemTime,UNIX_EPOCH}};
use anyhow::{ensure,Context,Result};
use protocol::CommandProposal;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use tokio::sync::watch;
use uuid::Uuid;
use crate::command;
mod sessions;

pub const OUTPUT_LIMIT: usize = 1_048_576;

#[derive(Clone,Serialize,Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub proposal: CommandProposal,
    pub program: String,
    pub arguments: Vec<String>,
    pub pid: Option<u32>,
    pub background_pids: Option<Vec<u32>>,
    pub changes:Option<Vec<Value>>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    #[serde(skip)]
    pub(crate) tails:[std::collections::VecDeque<u8>;2],
}
struct Entry { record: Arc<Mutex<Record>>, cancel: watch::Sender<bool>,input:Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdin>>>,cursor:Arc<tokio::sync::Mutex<(usize,usize)>>,files:Arc<Mutex<Option<Vec<crate::changes::Snapshot>>>> }
#[derive(Clone)]
pub struct Processes { launch:Arc<Mutex<()>>,operations:Arc<Mutex<HashMap<Uuid,sessions::OperationOutput>>>, entries: Arc<Mutex<HashMap<Uuid,Entry>>> }

/// Use milliseconds for local process timing in both platforms and the browser.
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }

impl Record {
    /// Register the approved command before spawning any operating-system process.
    pub fn new(id: Uuid, proposal: CommandProposal) -> Self {
        Self { id,proposal,program:String::new(),arguments:vec![],pid:None,background_pids:None,changes:None,started_at:now(),finished_at:None,exit_code:None,
            error:None,stdout:String::new(),stderr:String::new(),stdout_bytes:0,stderr_bytes:0,tails:Default::default() }
    }

    /// Keep execution failures distinct from text written to the command's stderr pipe.
    #[cfg(test)]
    pub fn result(&self) -> String {
        let mut status=self.error.clone().unwrap_or_else(||format!("Exit status: exit code: {}",self.exit_code.unwrap_or(-1)));
        if self.finished_at.is_none() {
            if let Some(pids)=self.background_pids.as_ref().filter(|pids|!pids.is_empty()) {
                status.push_str(&format!("\nBackground processes are still running (PIDs: {}). The launcher has finished; do not rerun it. They can be stopped in Tasks.",pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")));
            }
        }
        format!("{status}\nstdout:\n{}{}\nstderr:\n{}{}",self.stdout,
            if self.stdout_bytes>OUTPUT_LIMIT { "\n[output truncated at 1 MiB]" } else { "" },self.stderr,
            if self.stderr_bytes>OUTPUT_LIMIT { "\n[output truncated at 1 MiB]" } else { "" })
    }
}

impl Processes {
    /// Track only commands started by this native process; there is no on-disk history.
    pub fn open() -> Result<Self> {
        Ok(Self { launch:Arc::new(Mutex::new(())),operations:Arc::new(Mutex::new(HashMap::new())),entries:Arc::new(Mutex::new(HashMap::new())) })
    }

    /// List metadata only; large output is fetched separately when a record is expanded.
    pub fn list(&self) -> Value {
        let entries=self.entries.lock().unwrap();
        let mut records: Vec<_>=entries.values().map(|entry| {
            let r=entry.record.lock().unwrap();
            json!({"id":r.id,"command":r.proposal.command,"pid":r.pid,"started_at":r.started_at,"finished_at":r.finished_at,
                "exit_code":r.exit_code,"background_pids":r.background_pids,"error":r.error,"stdout_bytes":r.stdout_bytes,"stderr_bytes":r.stderr_bytes})
        }).collect();
        records.sort_by_key(|r|std::cmp::Reverse(r["started_at"].as_u64().unwrap()));
        json!(records)
    }

    /// Resolve only IDs owned by this daemon; callers cannot inject an arbitrary OS PID.
    pub fn get(&self, id: Uuid) -> Result<Record> {
        let entries=self.entries.lock().unwrap();
        let record=entries.get(&id).context("Process record not found")?.record.lock().unwrap().clone();
        Ok(record)
    }

    /// Request cancellation independently of the assistant's conversation lock.
    pub fn stop(&self, id: Uuid) -> Result<()> {
        let entries=self.entries.lock().unwrap();
        let entry=entries.get(&id).context("Process record not found")?;
        ensure!(entry.record.lock().unwrap().finished_at.is_none(),"Process has already finished");
        entry.cancel.send(true).context("Process has already finished")
    }

    /// Conversation Stop and daemon shutdown leave previously launched background applications alone.
    pub fn stop_foreground(&self,ids:Option<&[Uuid]>) {
        let _launch=self.launch.lock().unwrap();
        for entry in self.entries.lock().unwrap().values() {
            let record=entry.record.lock().unwrap();
            if ids.is_some_and(|ids|!ids.contains(&record.id)) { continue; }
            if record.finished_at.is_none() && record.background_pids.is_none() { let _=entry.cancel.send(true); }
        }
    }

    /// Run a tracked process tree and preserve its captured output even on failure or cancellation.
    async fn start(&self, id: Uuid, proposal: &CommandProposal, yield_time_ms:u64,timeout_ms:Option<u64>,yield_now:&tokio::sync::Notify,cancelled:&Arc<std::sync::atomic::AtomicBool>) -> Result<Record> {
        ensure!(Path::new(&proposal.cwd).is_absolute(),"Command directory must be absolute");
        let capture=proposal.clone();
        let files=Arc::new(Mutex::new(Some(tokio::task::spawn_blocking(move||crate::changes::capture(&capture.cwd,capture.files.as_deref().unwrap_or_default())).await?)));
        let record=Arc::new(Mutex::new(Record::new(id,proposal.clone())));
        let (cancel,receiver)=watch::channel(false);
        let input=Arc::new(tokio::sync::Mutex::new(None));
        {
            let mut entries=self.entries.lock().unwrap();
            // Bound resident output; discard older completed records.
            let mut completed: Vec<_>=entries.iter().filter_map(|(id,entry)| {
                let record=entry.record.lock().unwrap();
                record.finished_at.map(|_|(*id,record.started_at))
            }).collect();
            completed.sort_by_key(|(_,started)|std::cmp::Reverse(*started));
            for (id,_) in completed.into_iter().skip(49) { entries.remove(&id); }
            entries.insert(id,Entry { record:record.clone(),cancel,input:input.clone(),cursor:Arc::new(Default::default()),files:files.clone() });
        }
        let (completed,launcher)=tokio::sync::oneshot::channel();
        let (running,proposal)=(record.clone(),proposal.clone());
        let (cancelled,launch)=(cancelled.clone(),self.launch.clone());
        // The monitor outlives the launcher so AI follow-ups do not wait for background applications.
        let task=tokio::spawn(async move {
            let result=command::execute(&proposal,running.clone(),receiver,OUTPUT_LIMIT,Some(completed),input.clone(),timeout_ms,cancelled,launch).await;
            input.lock().await.take();
            let snapshots=files.lock().unwrap().take();
            let changes=if let Some(snapshots)=snapshots { Some(tokio::task::spawn_blocking(move||crate::changes::compare(snapshots)).await?) } else { None };
            let mut record=running.lock().unwrap();
            if let Some(changes)=changes { record.changes=Some(changes); }
            if let Err(error)=result { record.error=Some(error.to_string()); }
            record.finished_at=Some(now());
            if let Some(pids)=&mut record.background_pids { pids.clear(); }
            Ok::<_,anyhow::Error>(())
        });
        let minimum=if cfg!(windows) { 10_000 } else { 250 };
        tokio::select! {
            result=tokio::time::timeout(std::time::Duration::from_millis(yield_time_ms.clamp(minimum,30_000)),launcher)=> {
                if matches!(result,Ok(Err(_))) { task.await??; }
            }
            _=yield_now.notified()=> {}
        }
        let record=record.lock().unwrap();
        Ok(record.clone())
    }

    /// Direct callers use the same initial session wait as model-proposed commands.
    #[cfg(test)]
    pub async fn run(&self,id:Uuid,proposal:&CommandProposal)->Result<Record> { self.start(id,proposal,10_000,None,&tokio::sync::Notify::new(),&Arc::new(std::sync::atomic::AtomicBool::new(false))).await }

}
