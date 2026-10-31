use std::{collections::HashMap,sync::{Arc,Mutex}};
use anyhow::{ensure,Context,Result};
use serde_json::{json,Value};
use tokio::{io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt,BufReader},process::{Child,ChildStderr,ChildStdout},sync::{broadcast,mpsc,oneshot}};
use uuid::Uuid;

enum Outbound { Call(Value,Uuid,Option<oneshot::Sender<Value>>), Subscribe(String,Uuid,mpsc::Sender<Value>,oneshot::Sender<bool>), Release(String,Uuid), Shutdown(oneshot::Sender<()>) }
pub(super) struct Transport {
    sender:mpsc::UnboundedSender<Outbound>,
    pub(super) threads:Arc<Mutex<HashMap<String,(Uuid,mpsc::Sender<Value>)>>>,
    pub(super) global:broadcast::Sender<Value>,
    failure:Arc<Mutex<Option<String>>>,
    task:tokio::task::JoinHandle<()>,
    writer:tokio::task::JoinHandle<()>,
    stderr:tokio::task::JoinHandle<()>,
}

/// Record only the message from structured worker warnings/errors, never its attached request fields.
fn worker_message(line:&[u8],account:&str) {
    let text=String::from_utf8_lossy(line);
    if text.trim().is_empty() { return; }
    if let Ok(value)=serde_json::from_str::<Value>(&text) {
        let level=match value["level"].as_str() { Some("ERROR")=>"error",Some("WARN"|"WARNING")=>"warning",_=>return };
        if let Some(message)=value["fields"]["message"].as_str() { crate::diagnostics::record(level,"provider_worker",None,Some(account),message); }
    } else { crate::diagnostics::record("warning","provider_worker",None,Some(account),&text); }
}

/// Drain stderr continuously with bounded memory, including an unterminated fatal startup error.
async fn worker_stderr(mut stderr:ChildStderr,account:&str)->Result<()> {
    let mut chunk=[0u8;4096]; let mut line=Vec::new(); let mut truncated=false;
    loop {
        let count=stderr.read(&mut chunk).await?;
        if count==0 { break; }
        for byte in &chunk[..count] {
            if *byte==b'\n' {
                if truncated { crate::diagnostics::record("warning","provider_worker",None,Some(account),"Worker diagnostic line exceeded 8 KiB and was omitted"); }
                else { worker_message(&line,account); }
                line.clear(); truncated=false;
            } else if line.len()<8192 { line.push(*byte); } else { truncated=true; }
        }
    }
    if !truncated { worker_message(&line,account); }
    Ok(())
}

/// Retain a partial UTF-8 line when another outbound request wins the transport select.
async fn read(output:&mut BufReader<ChildStdout>,line:&mut Vec<u8>)->Result<Value> {
    loop {
        let chunk=output.fill_buf().await?;
        ensure!(!chunk.is_empty(),"Embedded Codex stopped unexpectedly");
        let count=chunk.iter().position(|b|*b==b'\n').map_or(chunk.len(),|i|i+1);
        ensure!(line.len()+count<=protocol::MAX_UPLOAD_BYTES+2_000_000,"Provider event exceeds size limit");
        let complete=chunk[count-1]==b'\n';
        line.extend_from_slice(&chunk[..count]);
        output.consume(count);
        if complete {
            let value=serde_json::from_slice(line)?;
            line.clear();
            if line.capacity()>256*1024 { line.shrink_to(64*1024); }
            return Ok(value);
        }
    }
}

impl Transport {
    /// One account process dispatches replies by request ID and notifications by conversation thread.
    pub(super) fn start(mut child:Child,account:String)->Arc<Self> {
        let stderr=child.stderr.take().unwrap();
        let stderr_account=account.clone();
        let stderr=tokio::spawn(async move {
            if let Err(error)=worker_stderr(stderr,&stderr_account).await { crate::diagnostics::record("error","provider_worker",None,Some(&stderr_account),&format!("Reading worker stderr: {error:#}")); }
        });
        let mut input=child.stdin.take().unwrap();
        let mut output=BufReader::with_capacity(64*1024,child.stdout.take().unwrap());
        // A large input must not prevent the account dispatcher from draining other conversations.
        let (writes,mut queued)=mpsc::unbounded_channel::<Value>();
        let (writer_failed,mut writer_failure)=oneshot::channel();
        let writer=tokio::spawn(async move {
            let result:Result<()>=async {
                while let Some(value)=queued.recv().await {
                    input.write_all(format!("{value}\n").as_bytes()).await?;
                    input.flush().await?;
                }
                Ok(())
            }.await;
            let _=writer_failed.send(result);
        });
        let stop_writer=writer.abort_handle();
        let (sender,mut receiver)=mpsc::unbounded_channel();
        let threads=Arc::new(Mutex::new(HashMap::<String,(Uuid,mpsc::Sender<Value>)>::new()));
        let global=broadcast::channel(32).0;
        let failure=Arc::new(Mutex::new(None));
        let failed=failure.clone();
        let (routes,notifications,cleanup)=(threads.clone(),global.clone(),sender.clone());
        let task=tokio::spawn(async move {
            let mut pending=HashMap::<u64,(String,Option<String>,Uuid,oneshot::Sender<Value>)>::new();
            let mut turns=HashMap::<String,String>::new();
            let mut line=Vec::new();
            let mut sequence=0u64;
            let mut stopped=None;
            let result:Result<()>=async {
                loop {
                    let write=tokio::select! {
                        result=&mut writer_failure=> {
                            result.context("Provider input writer stopped")??;
                            anyhow::bail!("Provider input writer closed");
                        }
                        outgoing=receiver.recv()=>match outgoing.context("Provider transport closed")? {
                            Outbound::Shutdown(reply)=> { stopped=Some(reply); break Ok(()); }
                            Outbound::Subscribe(thread,owner,sender,reply)=> {
                                let mut routes=routes.lock().unwrap();
                                let available=!routes.contains_key(&thread);
                                if available { routes.insert(thread,(owner,sender)); }
                                let _=reply.send(available);
                                Vec::new()
                            }
                            Outbound::Call(mut value,owner,reply)=> {
                                if value["method"]!="initialized" {
                                    sequence+=1;
                                    value["id"]=json!(sequence);
                                    if let Some(reply)=reply {
                                        pending.insert(sequence,(value["method"].as_str().unwrap().into(),value["params"]["threadId"].as_str().map(str::to_owned),owner,reply));
                                    }
                                }
                                vec![value]
                            }
                            Outbound::Release(thread,owner)=> {
                                // A cancelled or rejected subscriber cannot release a later or existing owner.
                                if routes.lock().unwrap().get(&thread).is_some_and(|route|route.0!=owner) { continue; }
                                routes.lock().unwrap().remove(&thread);
                                let mut writes=Vec::new();
                                if let Some(turn)=turns.remove(&thread) {
                                    sequence+=1;
                                    writes.push(json!({"id":sequence,"method":"turn/interrupt","params":{"threadId":thread,"turnId":turn}}));
                                }
                                sequence+=1;
                                writes.push(json!({"id":sequence,"method":"thread/unsubscribe","params":{"threadId":thread}}));
                                writes
                            }
                        },
                        message=read(&mut output,&mut line)=> {
                            let message=message?;
                            if message.get("method").is_some() && message.get("id").is_some() {
                                // Reject host tools without exposing another conversation to the request.
                                if let Some(thread)=message["params"]["threadId"].as_str() {
                                    if let Some((owner,_))=routes.lock().unwrap().get(thread) { let _=cleanup.send(Outbound::Release(thread.into(),*owner)); }
                                }
                                vec![json!({"id":message["id"],"error":{"code":-32601,"message":"Backend tools are disabled"}})]
                            } else {
                                if let Some(id)=message["id"].as_u64() {
                                    if let Some((method,thread,owner,reply))=pending.remove(&id) {
                                        if method=="turn/start" {
                                            if let (Some(thread),Some(turn))=(thread.as_deref(),message["result"]["turn"]["id"].as_str()) {
                                                turns.insert(thread.into(),turn.into());
                                            }
                                        }
                                        if let Err(message)=reply.send(message) {
                                            let thread=if method=="thread/start" { message["result"]["thread"]["id"].as_str() } else { thread.as_deref() };
                                            if let Some(thread)=thread { let _=cleanup.send(Outbound::Release(thread.into(),owner)); }
                                        }
                                    }
                                } else if let Some(thread)=message["params"]["threadId"].as_str() {
                                    if message["method"]=="turn/completed" { turns.remove(thread); }
                                    let route=routes.lock().unwrap().get(thread).cloned();
                                    if let Some((owner,route))=route {
                                        if route.try_send(message.clone()).is_err() { let _=cleanup.send(Outbound::Release(thread.into(),owner)); }
                                    }
                                } else { let _=notifications.send(message); }
                                Vec::new()
                            }
                        }
                    };
                    for value in write { writes.send(value).context("Provider input writer closed")?; }
                }
            }.await;
            if let Err(error)=result {
                let message=crate::diagnostics::clean(&format!("Embedded Codex transport closed: {error:#}; process status: {:?}",child.try_wait()));
                *failed.lock().unwrap()=Some(message.clone());
                crate::diagnostics::record("error","provider_transport",None,Some(&account),&message);
            }
            routes.lock().unwrap().clear();
            let _=notifications.send(json!({"method":"assistant/transportClosed"}));
            stop_writer.abort();
            let _=child.kill().await;
            if let Some(reply)=stopped { let _=reply.send(()); }
        });
        Arc::new(Self { sender,threads,global,task,failure,writer,stderr })
    }

    /// Wait for process exit before removing credentials or databases it may still have open.
    pub(super) async fn shutdown(&self)->Result<()> {
        if !self.alive() { return Ok(()); }
        let (reply,result)=oneshot::channel();
        self.sender.send(Outbound::Shutdown(reply))?;
        Ok(result.await?)
    }

    /// Route one response to its caller; a dropped caller never consumes another request's reply.
    pub(super) async fn call(&self,owner:Uuid,method:&str,params:Value)->Result<Value> {
        let (reply,result)=oneshot::channel();
        self.sender.send(Outbound::Call(json!({"method":method,"params":params}),owner,Some(reply)))?;
        result.await.with_context(||self.failure())
    }

    /// Serialize subscription changes with release and resume requests to avoid stale cleanup races.
    pub(super) async fn subscribe(&self,thread:String,owner:Uuid)->Result<mpsc::Receiver<Value>> {
        let (sender,receiver)=mpsc::channel(256);
        let (reply,result)=oneshot::channel();
        self.sender.send(Outbound::Subscribe(thread,owner,sender,reply))?;
        ensure!(result.await?,"This provider conversation is already active");
        Ok(receiver)
    }

    /// Send initialization or release a subscription without holding a conversation's generation lock.
    pub(super) fn initialized(&self)->Result<()> { Ok(self.sender.send(Outbound::Call(json!({"method":"initialized","params":{}}),Uuid::nil(),None))?) }
    /// Interrupt only this thread if active, then unsubscribe its notifications.
    pub(super) fn release(&self,thread:String,owner:Uuid) { let _=self.sender.send(Outbound::Release(thread,owner)); }
    /// A failed account transport is replaced on the next request.
    pub(super) fn alive(&self)->bool { !self.task.is_finished() }
    /// Retain the reader/process failure when RPC callers only observe a closed response channel.
    pub(super) fn failure(&self)->String { self.failure.lock().unwrap().clone().unwrap_or_else(||"Embedded Codex transport closed".into()) }
}

impl Drop for Transport {
    /// Only losing the entire account transport terminates its process, never dropping one conversation.
    fn drop(&mut self) { self.task.abort(); self.writer.abort(); self.stderr.abort(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Worker diagnostics preserve error messages but discard attached data and routine info records.
    #[tokio::test]
    async fn worker_messages_drop_payloads()->Result<()> {
        crate::diagnostics::init_test();
        let account=Uuid::new_v4().to_string();
        worker_message(&serde_json::to_vec(&json!({"level":"ERROR","fields":{"message":"HTTP 401 世界; Bearer PRIVATE_CREDENTIAL","prompt":"PRIVATE_INPUT"},"span":{"input":"PRIVATE_INPUT"}}))?,&account);
        worker_message(br#"{"level":"INFO","fields":{"message":"PRIVATE_INFO"}}"#,&account);
        let filter=serde_json::from_value(json!({"search":account}))?;
        let entries=crate::diagnostics::list(axum::extract::Query(filter)).await.map_err(|_|anyhow::anyhow!("Cannot read diagnostics"))?.0;
        assert_eq!(entries["entries"].as_array().unwrap().len(),1);
        assert!(entries.to_string().contains("HTTP 401 世界"));
        assert!(!entries.to_string().contains("PRIVATE_"));
        Ok(())
    }
}
