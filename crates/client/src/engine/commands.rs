use super::*;

impl Engine {
    /// Consume one approved operation; each session keeps its identity across polling and stdin writes.
    pub async fn decide(&mut self,id:Uuid,approve:bool)->Result<Value> {
        let pending=self.saved.pending.as_ref().context("No operation awaiting approval")?;
        ensure!(pending.id==id,"This approval is stale");
        ensure!(!approve || self.saved.settings.commands_enabled || pending.proposal.is_poll(),"Command execution is disabled");
        let pending=self.saved.pending.take().unwrap();
        let session=pending.proposal.session_id(pending.id);
        if approve {
            let command=match &pending.proposal {
                LocalAction::ExecCommand { cmd,workdir,.. }=>Some((cmd.clone(),workdir.clone(),true)),
                _=>self.processes.get(session).ok().map(|record| { let running=record.finished_at.is_none() && record.background_pids.is_none(); (record.proposal.command,record.proposal.cwd,running) }),
            };
            if let Some((command,cwd,running))=command {
                self.live.0.send_modify(|view|view["commands"].as_array_mut().unwrap().push(json!({"after":self.saved.messages.len(),"id":session,"action_id":pending.id,"command":command,"cwd":cwd,"reason":pending.proposal.reason(),"running":running})));
            }
        }
        self.saved.messages.push(Message { attachments:None,role:Role::Tool,text:if approve {
            format!("User authorized local operation: {}\nOutcome is unknown until its result follows. Never automatically repeat it.",pending.proposal.description())
        } else { "The user declined the local operation. It was not performed.".into() } });
        if approve {
            self.saved.pending_output=Some(pending.id);
            self.save()?;
            self.live.activity(&format!("Local command: {}",pending.proposal.reason()),None);
            self.live.0.send_modify(|view|view["can_steer"]=json!(true));
            self.processes.perform(pending.id,&pending.proposal,&pending.cwd,&self.controls.command_yield).await?;
            if let Ok(record)=self.processes.get(session) {
                let running=record.finished_at.is_none() && record.background_pids.is_none();
                self.live.0.send_modify(|view| {
                    for command in view["commands"].as_array_mut().unwrap().iter_mut().filter(|command|command["id"]==session.to_string()) {
                        command["changes"]=json!(record.changes);
                        if command["action_id"]==pending.id.to_string() { command["running"]=json!(running); }
                    }
                });
            }
            self.save()?;
        } else { self.prepare(pending.cwd,Uuid::new_v4())?; }
        if self.controls.stopped.load(Ordering::SeqCst) {
            if self.saved.pending_output.is_some() { self.finish_output()?; }
            self.saved.request=None;
            self.save()?;
            return Ok(self.state());
        }
        self.retry().await
    }
}
