use std::{path::{Path,PathBuf},sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}}};
use anyhow::{ensure,Context,Result};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{storage,stream::Live};

#[derive(Clone,Serialize,Deserialize)]
pub struct Queued { pub id:Uuid,pub text:String,pub cwd:String,pub editing:bool }
#[derive(Clone)]
pub struct Remote { pub backend:String,pub token:String,pub request:Uuid }
pub struct Controls { pub stopped:AtomicBool, remote:Mutex<Option<Remote>>, queue:tokio::sync::Mutex<Vec<Queued>>, path:PathBuf, live:Live }
pub struct Active(Arc<Controls>);

impl Controls {
    /// Queue persistence is independent of the engine lock held during commands and generation.
    pub fn open(directory:&Path,live:Live)->Result<Arc<Self>> {
        let path=directory.join("queue.json");
        let queue=if path.exists() { storage::read(&path)? } else { Vec::new() };
        let controls=Arc::new(Self { stopped:AtomicBool::new(false),remote:Mutex::new(None),queue:tokio::sync::Mutex::new(queue),path,live });
        controls.publish(&controls.queue.try_lock().unwrap())?;
        Ok(controls)
    }
    /// Persist the queue before publishing it, so browser reloads and daemon restarts retain input.
    fn publish(&self,queue:&[Queued])->Result<()> {
        storage::write(&self.path,&queue)?;
        self.live.0.send_modify(|view|view["queue"]=json!(queue));
        Ok(())
    }
    /// Enqueue user input once by its client-generated ID; commands are never supplied here.
    pub async fn enqueue(&self,input:Queued)->Result<()> {
        ensure!(!input.text.trim().is_empty() && input.text.len()<=protocol::MAX_MESSAGE_BYTES,"Enter a message up to 16,000 bytes");
        let mut queue=self.queue.lock().await;
        if let Some(old)=queue.iter_mut().find(|old|old.id==input.id) {
            if old.editing { old.text=input.text; old.editing=false; return self.publish(&queue); }
            ensure!(old.text==input.text && old.cwd==input.cwd,"Queue ID reused");
            return Ok(());
        }
        ensure!(queue.len()<50,"Message queue is full");
        queue.push(input);
        self.publish(&queue)
    }
    /// Remove a pending message without affecting the active task.
    pub async fn remove(&self,id:Uuid)->Result<()> {
        let mut queue=self.queue.lock().await;
        queue.retain(|input|input.id!=id);
        self.publish(&queue)
    }
    /// Hold an edited message in its original slot until the user submits or cancels the edit.
    pub async fn edit(&self,id:Uuid,editing:bool)->Result<Queued> {
        let mut queue=self.queue.lock().await;
        ensure!(!editing || !queue.iter().any(|input|input.editing && input.id!=id),"Finish editing the current message first");
        let input=queue.iter_mut().find(|input|input.id==id).context("This message is no longer queued")?;
        input.editing=editing;
        let input=input.clone();
        self.publish(&queue)?;
        Ok(input)
    }
    /// Claim and durably record input under one queue lock, excluding concurrent edit or steer.
    pub async fn start_next(&self,prepare:impl FnOnce(Queued)->Result<()>)->Result<bool> {
        let mut queue=self.queue.lock().await;
        let Some(input)=queue.first().filter(|input|!input.editing).cloned() else { return Ok(false); };
        prepare(input)?;
        queue.remove(0);
        self.publish(&queue)?;
        Ok(true)
    }
    /// Remove only after steering succeeds; a rejection leaves the original queue and order intact.
    pub async fn steer_queued(&self,id:Uuid)->Result<protocol::Steering> {
        let mut queue=self.queue.lock().await;
        let input=queue.iter().find(|input|input.id==id).context("This message is no longer queued")?;
        ensure!(!input.editing,"Submit the edited message before steering it");
        let input=protocol::Steering { id,text:input.text.clone() };
        self.send("steer",json!(input)).await?;
        queue.retain(|queued|queued.id!=id);
        self.publish(&queue)?;
        Ok(input)
    }
    /// Steering an edited draft consumes its held queue slot only after backend acceptance.
    pub async fn steer(&self,input:&protocol::Steering)->Result<Value> {
        let mut queue=self.queue.lock().await;
        ensure!(!queue.iter().any(|queued|queued.id==input.id && !queued.editing),"Use the queued message's Steer action");
        let result=self.send("steer",json!(input)).await?;
        queue.retain(|queued|queued.id!=input.id);
        self.publish(&queue)?;
        Ok(result)
    }
    /// Starting a new conversation explicitly clears its queued messages.
    pub async fn clear(&self)->Result<()> { let mut queue=self.queue.lock().await; queue.clear(); self.publish(&queue) }
    /// Publish only steering availability; backend credentials stay in this process.
    pub fn active(self:&Arc<Self>,remote:Remote)->Active {
        *self.remote.lock().unwrap()=Some(remote);
        self.live.0.send_modify(|view|view["can_steer"]=json!(false));
        Active(self.clone())
    }
    /// Forward control requests without waiting for the locked engine or command completion.
    pub async fn send(&self,action:&str,body:Value)->Result<Value> {
        let remote=self.remote.lock().unwrap().clone().context("No AI turn is running; queue the message instead")?;
        let request=crate::backend::client(60)?.post(format!("{}/v1/chat/{}/{action}",remote.backend,remote.request)).bearer_auth(remote.token).json(&body);
        let response=crate::backend::send(request,&self.live,None).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Editing survives reload, blocks dispatch at its original slot, and resumes in the same order.
    #[tokio::test]
    async fn editing_preserves_queue_order()->Result<()> {
        let directory=tempfile::tempdir()?;
        let controls=Controls::open(directory.path(),Live::new())?;
        let inputs:Vec<_>=["first","second","third"].into_iter().map(|text|Queued { id:Uuid::new_v4(),text:text.into(),cwd:"/tmp".into(),editing:false }).collect();
        for input in &inputs { controls.enqueue(input.clone()).await?; }
        controls.edit(inputs[1].id,true).await?;
        assert!(controls.start_next(|input| { assert_eq!(input.text,"first"); Ok(()) }).await?);
        assert!(!controls.start_next(|_|panic!("Edited input was dispatched")).await?);
        let controls=Controls::open(directory.path(),Live::new())?;
        assert!(!controls.start_next(|_|panic!("Reload lost the held slot")).await?);
        assert!(controls.edit(inputs[2].id,true).await.is_err());
        controls.enqueue(Queued { text:"edited 世界".into(),..inputs[1].clone() }).await?;
        assert!(controls.start_next(|input| { assert_eq!(input.id,inputs[1].id); assert_eq!(input.text,"edited 世界"); Ok(()) }).await?);
        controls.remove(inputs[2].id).await?;
        assert!(!controls.start_next(|_|panic!("Deleted input was dispatched")).await?);
        Ok(())
    }

    /// A queued steer cannot race dequeue; failed steering keeps the queued input untouched.
    #[tokio::test]
    async fn steering_excludes_dequeue_and_preserves_rejected_input()->Result<()> {
        use axum::{routing::post,Json,Router};
        let directory=tempfile::tempdir()?;
        let controls=Controls::open(directory.path(),Live::new())?;
        let input=Queued { id:Uuid::new_v4(),text:"Steer 世界".into(),cwd:"/tmp".into(),editing:false };
        controls.enqueue(input.clone()).await?;
        assert!(controls.steer_queued(input.id).await.is_err());
        assert_eq!(controls.queue.lock().await[0].text,input.text);
        let entered=Arc::new(tokio::sync::Notify::new());
        let release=Arc::new(tokio::sync::Notify::new());
        let (notify,wait)=(entered.clone(),release.clone());
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat/{id}/steer",post(move |Json(input):Json<protocol::Steering>| {
            let (notify,wait)=(notify.clone(),wait.clone());
            async move { notify.notify_one(); wait.notified().await; Json(json!({"accepted":input.id})) }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let _active=controls.active(Remote { backend,token:"fixture".into(),request:Uuid::new_v4() });
        let steering=controls.clone();
        let id=input.id;
        let task=tokio::spawn(async move { steering.steer_queued(id).await });
        tokio::time::timeout(std::time::Duration::from_secs(5),entered.notified()).await?;
        assert!(controls.queue.try_lock().is_err());
        release.notify_one();
        assert_eq!(task.await??.id,id);
        assert!(!controls.start_next(|_|panic!("Steered input was also queued")).await?);
        server.abort();
        Ok(())
    }
}
