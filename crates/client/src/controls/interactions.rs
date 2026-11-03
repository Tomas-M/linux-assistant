use super::*;
use protocol::{Interaction,InteractionKind,InteractionAnswer,InteractionResponse,LocalAction};
use crate::processes::Processes;

#[derive(Clone)]
pub(super) struct Receipt { request:Interaction,answer:Option<InteractionAnswer> }

impl Controls {
    /// Start each native request once; watch snapshots may repeat it until the backend acknowledges its result.
    pub fn receive(self:&Arc<Self>,requests:&[Interaction],processes:&Processes,full_access:&Arc<AtomicBool>,enabled:bool) {
        if self.stopped.load(Ordering::SeqCst) { return; }
        let present=requests.iter().map(|request|request.id).collect::<std::collections::HashSet<_>>();
        self.tasks.lock().unwrap().retain(|id,task| { if present.contains(id) { true } else { task.cancelled.store(true,Ordering::SeqCst); task.handle.abort(); false } });
        self.pending.lock().unwrap().retain(|id,_|present.contains(id));
        self.live.0.send_modify(|view|view["interactions"].as_array_mut().unwrap().retain(|request|request["id"].as_str().and_then(|id|id.parse::<Uuid>().ok()).is_some_and(|id|present.contains(&id))));
        for request in requests {
            let mut tasks=self.tasks.lock().unwrap();
            if self.stopped.load(Ordering::SeqCst) { break; }
            if tasks.contains_key(&request.id) { continue; }
            let (controls,request,processes,full_access)=(self.clone(),request.clone(),processes.clone(),full_access.clone());
            let id=request.id;
            let cancelled=Arc::new(AtomicBool::new(false));
            let lease=cancelled.clone();
            let task=tokio::spawn(async move {
                let result=controls.execute(&request,&processes,&full_access,enabled,&lease).await;
                if let Err(error)=result {
                    controls.failures.send_replace(Some(Arc::new(error.context("Could not deliver the local result; retry the unfinished request. The operation will not be repeated"))));
                }
            });
            tasks.insert(id,LocalTask { handle:task.abort_handle(),cancelled });
        }
    }

    /// UI replies can answer only a currently displayed question or exact operation approval.
    pub fn respond(&self,input:InteractionResponse)->Result<()> {
        let mut pending=self.pending.lock().unwrap();
        let (request,_)=pending.get(&input.id).context("This request is no longer waiting for your answer")?;
        match (&request.kind,&input.answer) {
            (InteractionKind::Questions { questions },InteractionAnswer::Questions { answers })=>protocol::validate_answers(questions,answers)?,
            (InteractionKind::Tool { .. },InteractionAnswer::Approval { .. })=>{},
            _=>anyhow::bail!("The answer does not match the pending request"),
        }
        pending.remove(&input.id).unwrap().1.send(input.answer).map_err(|_|anyhow::anyhow!("The task is no longer waiting"))?;
        self.live.0.send_modify(|view|view["interactions"].as_array_mut().unwrap().retain(|request|request["id"]!=input.id.to_string()));
        Ok(())
    }

    /// Enabling full access consumes displayed tool approvals, while leaving questions for the user.
    pub fn approve_waiting(&self) {
        let ids=self.pending.lock().unwrap().iter().filter_map(|(id,(request,_))|matches!(request.kind,InteractionKind::Tool { .. }).then_some(*id)).collect::<Vec<_>>();
        for id in ids { let _=self.respond(InteractionResponse { id,answer:InteractionAnswer::Approval { approve:true } }); }
    }

    /// Publish the request before waiting; browser reloads see the same form and one-use ID.
    async fn wait(&self,request:&Interaction)->Result<InteractionAnswer> {
        let (reply,result)=tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(request.id,(request.clone(),reply));
        self.live.0.send_modify(|view|view["interactions"].as_array_mut().unwrap().push(json!(request)));
        result.await.context("The task stopped before an answer was received")
    }

    /// Keep an acknowledgement in RAM before sending it; transport retries cannot replay side effects.
    async fn execute(&self,request:&Interaction,processes:&Processes,full_access:&Arc<AtomicBool>,enabled:bool,cancelled:&Arc<AtomicBool>)->Result<()> {
        // Bound large image buffers through acknowledgement, including retries from retained receipts.
        let _image_access=if matches!(&request.kind,InteractionKind::Tool { action:LocalAction::ViewImage { .. } }) { Some(self.file_access.clone().lock_owned().await) } else { None };
        let receipt=self.receipts.lock().unwrap().get(&request.id).cloned();
        if let Some(receipt)=&receipt { ensure!(serde_json::to_value(&receipt.request)?==serde_json::to_value(request)?,"Tool request ID reused with different arguments"); }
        let recovering=receipt.is_some();
        let answer:InteractionAnswer=if let Some(answer)=receipt.and_then(|receipt|receipt.answer) { answer } else {
            let answer=match &request.kind {
                InteractionKind::Questions { .. }=>self.wait(request).await?,
                InteractionKind::Tool { action }=> {
                    let mut image=None;
                    let result:Result<String>=async {
                        ensure!(!cancelled.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst),"Task stopped");
                        let mut operation=request.id;
                        let mut action=action.clone();
                        if recovering {
                            operation=Uuid::new_v4();
                            action=match &action {
                                LocalAction::ExecCommand { .. } if processes.get(request.id).is_ok()=>LocalAction::WriteStdin { session_id:request.id,chars:String::new(),close_stdin:false,yield_time_ms:5_000,reason:"Recover the existing command result without rerunning it".into() },
                                action if action.replay_safe()=>action.clone(),
                                _=>anyhow::bail!("The previous operation's outcome is unknown. Do not resend stdin or repeat edits automatically; inspect the session or current file first"),
                            };
                        }
                        ensure!(enabled || action.is_poll(),"Local access is disabled");
                        if !action.is_poll() && !full_access.load(Ordering::SeqCst) {
                            ensure!(matches!(self.wait(request).await?,InteractionAnswer::Approval { approve:true }),"The user declined this operation. Do not work around the refusal");
                        }
                        ensure!(!cancelled.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst),"Task stopped");
                        self.receipts.lock().unwrap().insert(request.id,Receipt { request:request.clone(),answer:None });
                        let session=match &action { LocalAction::ExecCommand { .. }|LocalAction::WriteStdin { .. }|LocalAction::StopProcess { .. }=>Some(action.session_id(operation)),_=>None };
                        self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"session_id":session,"running":true,"output":"","changes":[]}));
                        match &action {
                            LocalAction::ViewImage { path,.. }=> {
                                let (media,path)=(self.media.clone(),std::path::PathBuf::from(path)); let id=request.id;
                                let (output,attachment,preview)=tokio::task::spawn_blocking(move||media.view(id,&path)).await??;
                                ensure!(!cancelled.load(Ordering::SeqCst),"Image viewing cancelled");
                                image=Some(attachment);
                                self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"running":false,"changes":[],"image":preview}));
                                Ok(output)
                            }
                            LocalAction::ExecCommand { .. }|LocalAction::WriteStdin { .. }|LocalAction::StopProcess { .. }=> {
                                let session=action.session_id(operation);
                                let command=match &action {
                                    LocalAction::ExecCommand { cmd,workdir,.. }=>Some((cmd.clone(),workdir.clone())),
                                    _=>processes.get(session).ok().map(|record|(record.proposal.command,record.proposal.cwd)),
                                };
                                if let Some((command,cwd))=command {
                                    self.live.0.send_modify(|view| {
                                        let after=view["state"]["messages"].as_array().unwrap().len();
                                        view["commands"].as_array_mut().unwrap().push(json!({"after":after,"id":session,"action_id":request.id,"command":command,"cwd":cwd,"reason":action.reason()}));
                                    });
                                }
                                processes.perform(operation,&action,&self.command_yield,cancelled).await?;
                                let output=processes.output(operation)?;
                                if let Ok(record)=processes.get(session) {
                                    self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"session_id":session,"running":record.finished_at.is_none() && record.background_pids.is_none(),"changes":record.changes}));
                                }
                                ensure!(output.success,"{}",output.text);
                                Ok(output.text)
                            }
                            _=> {
                                let access=self.file_access.clone().lock_owned().await;
                                let (action,stopped)=(action.clone(),cancelled.clone());
                                let (output,changes)=tokio::task::spawn_blocking(move|| {
                                    let _access=access;
                                    let paths=action.affected_paths();
                                    let before=crate::changes::capture("",&paths);
                                    let result=crate::files::execute(&action,&stopped);
                                    let mut changes=crate::changes::compare(before);
                                    if result.is_ok() && changes.is_empty() {
                                        if let LocalAction::ApplyPatch { path,old_text,new_text,.. }=&action {
                                            if old_text!=new_text { changes.push(json!({"path":path,"kind":"Edited","added":null,"removed":null,"diff":"File changed; the full diff is too large to display."})); }
                                        }
                                    }
                                    (result,changes)
                                }).await?;
                                self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"running":false,"changes":changes}));
                                output
                            }
                        }
                    }.await;
                    let success=result.is_ok();
                    let output=match result { Ok(output)=>output,Err(error)=>format!("Local operation failed: {error:#}") };
                    self.live.0.send_modify(|view| { view["tool_results"][&request.item_id]["output"]=json!(crate::engine::output::preview(&output)); view["tool_results"][&request.item_id]["success"]=json!(success); if !success { view["tool_results"][&request.item_id]["running"]=json!(false); } });
                    InteractionAnswer::Tool { output:crate::engine::output::preview(&output),success,image }
                }
            };
            self.receipts.lock().unwrap().insert(request.id,Receipt { request:request.clone(),answer:Some(answer.clone()) });
            answer
        };
        if let InteractionAnswer::Tool { image:Some(protocol::Attachment::Image { name,.. }),.. }=&answer {
            self.live.0.send_modify(|view|view["tool_results"][&request.item_id]["image"]=json!({"kind":"image","name":name,"url":format!("/api/media/{}/0",request.id),"run":view["runs"].as_array().unwrap().last().map(|run|run["started_at"].clone())}));
        }
        self.send("respond",json!(InteractionResponse { id:request.id,answer })).await?;
        Ok(())
    }
}
