use super::*;

/// Local display metadata never changes the protocol transcript or gets sent to the model.
impl Engine {
    /// Keep command follow-ups in one task, with an independent record for a resumed failed attempt.
    pub(super) fn begin_activity(&self,start:usize) {
        self.live.0.send_modify(|view| {
            let runs=view["runs"].as_array_mut().unwrap();
            if runs.last().is_some_and(|run|run["finished_at"].is_null()) { return; }
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            runs.push(json!({"start":start,"end":start,"started_at":now,"finished_at":null,"error":null,"partial":"","summaries":[]}));
        });
    }

    /// Save a real completion time and any interrupted preview before the next queued task replaces it.
    pub fn finish_activity(&self,error:Option<String>)->Result<()> {
        self.live.0.send_modify(|view| {
            let partial=if error.is_some() { view["activity"]["progress"]["text"].as_str().unwrap_or_default().to_owned() } else { String::new() };
            let summaries=if error.is_some() { view["activity"]["progress"]["summaries"].as_array().cloned().unwrap_or_default() } else { vec![] };
            let Some(run)=view["runs"].as_array_mut().unwrap().last_mut().filter(|run|run["finished_at"].is_null()) else { return; };
            run["end"]=json!(self.saved.messages.len());
            run["finished_at"]=json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64);
            run["error"]=json!(if self.controls.stopped.load(Ordering::SeqCst) { Some("Stopped by you".to_owned()) } else { error });
            run["partial"]=json!(partial);
            run["summaries"]=json!(summaries);
        });
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping retains the preview only in local presentation state, without contaminating future model input.
    #[tokio::test]
    async fn stopped_preview_survives_refresh_and_next_task()->Result<()> {
        let directory=tempfile::tempdir()?;
        let mut engine=Engine::open(directory.path(),"http://127.0.0.1:8787").await?;
        engine.saved.messages.push(Message { role:Role::User,text:"Check the logs".into() });
        engine.begin_activity(0);
        engine.live.activity("Checking logs…",Some(protocol::Progress { text:"Partial response 世界".into(),summaries:vec![protocol::ProgressText { id:"summary".into(),text:"Looking for repeated failures.".into() }],..Default::default() }));
        engine.controls.stopped.store(true,Ordering::SeqCst);
        engine.finish_activity(Some("Stopped".into()))?;
        let run=engine.live.0.borrow()["runs"][0].clone();
        assert_eq!(run["partial"],"Partial response 世界");
        assert_eq!(run["error"],"Stopped by you");
        assert_eq!(run["summaries"][0]["text"],"Looking for repeated failures.");
        assert_eq!(run["end"],1);
        assert_eq!(engine.saved.messages.len(),1);
        assert_eq!(Engine::open(directory.path(),"http://127.0.0.1:8787").await?.live.0.borrow()["runs"][0],run);
        engine.begin_activity(1);
        assert_eq!(engine.live.0.borrow()["runs"][0],run);
        assert!(engine.live.0.borrow()["runs"][1]["finished_at"].is_null());
        engine.new_chat().await?;
        assert!(Engine::open(directory.path(),"http://127.0.0.1:8787").await?.live.0.borrow()["runs"].as_array().unwrap().is_empty());
        Ok(())
    }
}
