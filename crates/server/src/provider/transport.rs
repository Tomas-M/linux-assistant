use std::{collections::HashMap,sync::{Arc,Mutex}};
use anyhow::{ensure,Context,Result};
use serde_json::{json,Value};
use tokio::{io::{AsyncBufReadExt,AsyncWriteExt,BufReader},process::{Child,ChildStdout},sync::{broadcast,mpsc,oneshot}};
use uuid::Uuid;

enum Outbound { Call(Value,Uuid,Option<oneshot::Sender<Value>>), Subscribe(String,Uuid,mpsc::Sender<Value>,oneshot::Sender<bool>), Release(String,Uuid) }
pub(super) struct Transport {
    sender:mpsc::UnboundedSender<Outbound>,
    pub(super) threads:Arc<Mutex<HashMap<String,(Uuid,mpsc::Sender<Value>)>>>,
    pub(super) global:broadcast::Sender<Value>,
    task:tokio::task::JoinHandle<()>,
}

/// Retain a partial UTF-8 line when another outbound request wins the transport select.
async fn read(output:&mut BufReader<ChildStdout>,line:&mut Vec<u8>)->Result<Value> {
    loop {
        let chunk=output.fill_buf().await?;
        ensure!(!chunk.is_empty(),"Embedded Codex stopped unexpectedly");
        let count=chunk.iter().position(|b|*b==b'\n').map_or(chunk.len(),|i|i+1);
        ensure!(line.len()+count<=2_000_000,"Provider event exceeds size limit");
        let complete=chunk[count-1]==b'\n';
        line.extend_from_slice(&chunk[..count]);
        output.consume(count);
        if complete { return Ok(serde_json::from_slice(&std::mem::take(line))?); }
    }
}

impl Transport {
    /// One account process dispatches replies by request ID and notifications by conversation thread.
    pub(super) fn start(mut child:Child)->Arc<Self> {
        let mut input=child.stdin.take().unwrap();
        let mut output=BufReader::new(child.stdout.take().unwrap());
        let (sender,mut receiver)=mpsc::unbounded_channel();
        let threads=Arc::new(Mutex::new(HashMap::<String,(Uuid,mpsc::Sender<Value>)>::new()));
        let global=broadcast::channel(32).0;
        let (routes,notifications,cleanup)=(threads.clone(),global.clone(),sender.clone());
        let task=tokio::spawn(async move {
            let mut pending=HashMap::<u64,(Value,Uuid,oneshot::Sender<Value>)>::new();
            let mut turns=HashMap::<String,String>::new();
            let mut line=Vec::new();
            let mut sequence=0u64;
            let result:Result<()>=async {
                loop {
                    let write=tokio::select! {
                        outgoing=receiver.recv()=>match outgoing.context("Provider transport closed")? {
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
                                    if let Some(reply)=reply { pending.insert(sequence,(value.clone(),owner,reply)); }
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
                                    if let Some((request,owner,reply))=pending.remove(&id) {
                                        if request["method"]=="turn/start" {
                                            if let (Some(thread),Some(turn))=(request["params"]["threadId"].as_str(),message["result"]["turn"]["id"].as_str()) {
                                                turns.insert(thread.into(),turn.into());
                                            }
                                        }
                                        if let Err(message)=reply.send(message) {
                                            let thread=if request["method"]=="thread/start" { message["result"]["thread"]["id"].as_str() } else { request["params"]["threadId"].as_str() };
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
                    for value in write { input.write_all(format!("{value}\n").as_bytes()).await?; input.flush().await?; }
                }
            }.await;
            if result.is_err() { eprintln!("Embedded Codex transport closed; active requests must retry"); }
            routes.lock().unwrap().clear();
            let _=notifications.send(json!({"method":"assistant/transportClosed"}));
            let _=child.kill().await;
        });
        Arc::new(Self { sender,threads,global,task })
    }

    /// Route one response to its caller; a dropped caller never consumes another request's reply.
    pub(super) async fn call(&self,owner:Uuid,method:&str,params:Value)->Result<Value> {
        let (reply,result)=oneshot::channel();
        self.sender.send(Outbound::Call(json!({"method":method,"params":params}),owner,Some(reply)))?;
        result.await.context("Embedded Codex transport closed")
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
}

impl Drop for Transport {
    /// Only losing the entire account transport terminates its process, never dropping one conversation.
    fn drop(&mut self) { self.task.abort(); }
}
