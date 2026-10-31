use super::*;
use protocol::{Interaction,InteractionKind,InteractionAnswer,InteractionResponse,LocalAction};
use crate::processes::Processes;

impl Controls {
    /// Start each native request once; watch snapshots may repeat it until the backend acknowledges its result.
    pub fn receive(self:&Arc<Self>,requests:&[Interaction],processes:&Processes,full_access:&Arc<AtomicBool>,enabled:bool,cwd:&str) {
        let present=requests.iter().map(|request|request.id).collect::<std::collections::HashSet<_>>();
        self.tasks.lock().unwrap().retain(|id,task| { if present.contains(id) { true } else { task.abort(); false } });
        self.pending.lock().unwrap().retain(|id,_|present.contains(id));
        self.live.0.send_modify(|view|view["interactions"].as_array_mut().unwrap().retain(|request|request["id"].as_str().and_then(|id|id.parse::<Uuid>().ok()).is_some_and(|id|present.contains(&id))));
        for request in requests {
            let mut tasks=self.tasks.lock().unwrap();
            if tasks.contains_key(&request.id) { continue; }
            let (controls,request,processes,full_access,cwd)=(self.clone(),request.clone(),processes.clone(),full_access.clone(),cwd.to_owned());
            let id=request.id;
            let task=tokio::spawn(async move {
                let result=controls.execute(&request,&processes,&full_access,enabled,&cwd).await;
                if let Err(error)=result {
                    controls.failures.send_replace(Some(format!("Could not deliver the local result: {error:#}. Retry the unfinished request; the operation will not be repeated.")));
                }
            });
            tasks.insert(id,task.abort_handle());
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

    /// Keep an acknowledgement on disk before sending it; transport retries cannot replay side effects.
    async fn execute(&self,request:&Interaction,processes:&Processes,full_access:&Arc<AtomicBool>,enabled:bool,cwd:&str)->Result<()> {
        let directory=self.path.parent().unwrap().join("tool-results");
        std::fs::create_dir_all(&directory)?;
        let path=directory.join(format!("{}.json",request.id));
        let answer:InteractionAnswer=if path.exists() { storage::read(&path)? } else {
            let answer=match &request.kind {
                InteractionKind::Questions { .. }=>self.wait(request).await?,
                InteractionKind::Tool { action }=> {
                    let result:Result<String>=async {
                        ensure!(!self.stopped.load(Ordering::SeqCst),"Task stopped");
                        ensure!(enabled || action.is_poll(),"Local access is disabled");
                        if !action.is_poll() && !full_access.load(Ordering::SeqCst) {
                            ensure!(matches!(self.wait(request).await?,InteractionAnswer::Approval { approve:true }),"The user declined this operation. Do not work around the refusal");
                        }
                        ensure!(!self.stopped.load(Ordering::SeqCst),"Task stopped");
                        storage::write(&path,&InteractionAnswer::Tool { success:false,output:format!("Operation {} was interrupted before acknowledgement; outcome is unknown. Do not repeat it automatically. Check the session or current file first.",request.id) })?;
                        self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"running":true,"output":"","changes":[]}));
                        match action {
                            LocalAction::ExecCommand { .. }|LocalAction::WriteStdin { .. }|LocalAction::StopProcess { .. }=> {
                                let session=action.session_id(request.id);
                                let command=match action {
                                    LocalAction::ExecCommand { cmd,workdir,.. }=>Some((cmd.clone(),workdir.clone())),
                                    _=>processes.get(session).ok().map(|record|(record.proposal.command,record.proposal.cwd)),
                                };
                                if let Some((command,cwd))=command {
                                    self.live.0.send_modify(|view| {
                                        let after=view["state"]["messages"].as_array().unwrap().len();
                                        view["commands"].as_array_mut().unwrap().push(json!({"after":after,"id":session,"action_id":request.id,"command":command,"cwd":cwd,"reason":action.reason()}));
                                    });
                                }
                                processes.perform(request.id,action,cwd,&self.command_yield).await?;
                                let output=processes.output(request.id)?;
                                if let Ok(record)=processes.get(session) {
                                    self.live.0.send_modify(|view|view["tool_results"][&request.item_id]=json!({"session_id":session,"running":record.finished_at.is_none() && record.background_pids.is_none(),"changes":record.changes}));
                                }
                                ensure!(output.success,"{}",output.text);
                                Ok(output.text)
                            }
                            _=> {
                                let access=self.file_access.clone().lock_owned().await;
                                let (action,stopped)=(action.clone(),self.stopped.clone());
                                let (output,changes)=tokio::task::spawn_blocking(move|| {
                                    let _access=access;
                                    let paths=match &action { LocalAction::ApplyPatch { path,.. }|LocalAction::WriteFile { path,.. }=>vec![path.clone()],_=>vec![] };
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
                    InteractionAnswer::Tool { output:crate::engine::output::preview(&output),success }
                }
            };
            storage::write(&path,&answer)?;
            answer
        };
        self.send("respond",json!(InteractionResponse { id:request.id,answer })).await?;
        Ok(())
    }
}
