use std::{path::{Path,PathBuf},sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}}};
use anyhow::{ensure,Context,Result};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{storage,stream::Live};

#[derive(Clone,Serialize,Deserialize)]
pub struct Queued { pub id:Uuid,pub text:String,pub cwd:String }
#[derive(Clone)]
pub struct Remote { pub backend:String,pub token:String,pub request:Uuid }
pub struct Controls { pub stopped:AtomicBool, remote:Mutex<Option<Remote>>, queue:Mutex<Vec<Queued>>, path:PathBuf, live:Live }
pub struct Active(Arc<Controls>);

impl Controls {
    /// Queue persistence is independent of the engine lock held during commands and generation.
    pub fn open(directory:&Path,live:Live)->Result<Arc<Self>> {
        let path=directory.join("queue.json");
        let queue=if path.exists() { storage::read(&path)? } else { Vec::new() };
        let controls=Arc::new(Self { stopped:AtomicBool::new(false),remote:Mutex::new(None),queue:Mutex::new(queue),path,live });
        controls.publish(&controls.queue.lock().unwrap())?;
        Ok(controls)
    }
    /// Persist the queue before publishing it, so browser reloads and daemon restarts retain input.
    fn publish(&self,queue:&[Queued])->Result<()> {
        storage::write(&self.path,&queue)?;
        self.live.0.send_modify(|view|view["queue"]=json!(queue));
        Ok(())
    }
    /// Enqueue user input once by its client-generated ID; commands are never supplied here.
    pub fn enqueue(&self,input:Queued)->Result<()> {
        ensure!(!input.text.trim().is_empty() && input.text.len()<=protocol::MAX_MESSAGE_BYTES,"Enter a message up to 16,000 bytes");
        let mut queue=self.queue.lock().unwrap();
        if let Some(old)=queue.iter().find(|old|old.id==input.id) { ensure!(old.text==input.text && old.cwd==input.cwd,"Queue ID reused"); return Ok(()); }
        ensure!(queue.len()<50,"Message queue is full");
        queue.push(input);
        self.publish(&queue)
    }
    /// Remove a pending message without affecting the active task.
    pub fn remove(&self,id:Uuid)->Result<()> {
        let mut queue=self.queue.lock().unwrap();
        queue.retain(|input|input.id!=id);
        self.publish(&queue)
    }
    /// Peek until the engine durably records this input; a crash must not lose a queued message.
    pub fn next(&self)->Option<Queued> { self.queue.lock().unwrap().first().cloned() }
    /// Starting a new conversation explicitly clears its queued messages.
    pub fn clear(&self)->Result<()> { let mut queue=self.queue.lock().unwrap(); queue.clear(); self.publish(&queue) }
    /// Publish only steering availability; backend credentials stay in this process.
    pub fn active(self:&Arc<Self>,remote:Remote)->Active {
        *self.remote.lock().unwrap()=Some(remote);
        self.live.0.send_modify(|view|view["can_steer"]=json!(false));
        Active(self.clone())
    }
    /// Forward control requests without waiting for the locked engine or command completion.
    pub async fn send(&self,action:&str,body:Value)->Result<Value> {
        let remote=self.remote.lock().unwrap().clone().context("No AI turn is running; queue the message instead")?;
        let response=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(60)).build()?
            .post(format!("{}/v1/chat/{}/{action}",remote.backend,remote.request)).bearer_auth(remote.token).json(&body).send().await?;
        let status=response.status();
        let value:Value=response.json().await?;
        ensure!(status.is_success(),"{}",value["error"].as_str().unwrap_or("Control request failed"));
        Ok(value)
    }
    /// Stop applies to the whole local task, including its automatic-command loop and queued follow-ups.
    pub async fn interrupt(&self)->Result<()> {
        self.stopped.store(true,Ordering::SeqCst);
        if self.remote.lock().unwrap().is_some() { self.send("stop",json!({})).await?; }
        Ok(())
    }
}
impl Drop for Active {
    /// Clear steering availability on success, transport failure, and cancellation alike.
    fn drop(&mut self) { *self.0.remote.lock().unwrap()=None; self.0.live.0.send_modify(|view|view["can_steer"]=json!(false)); }
}
