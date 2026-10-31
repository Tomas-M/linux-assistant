use super::*;

// Keep the existing inline budget until the backend's message/history limits are raised.
const INLINE_LIMIT: usize = 8_000;

/// Preserve both ends of captured output, with an explicit omission notice and valid UTF-8.
fn preview(text: &str) -> String {
    if text.len()<=INLINE_LIMIT { return text.to_owned(); }
    let notice="\n[Output truncated: middle omitted. Captured stdout/stderr are in Processes (up to 1 MiB each). Use a filtered command to inspect missing details.]\n";
    let budget=INLINE_LIMIT-notice.len();
    let mut start=budget/2;
    let mut end=text.len()-(budget-start);
    while !text.is_char_boundary(start) { start-=1; }
    while !text.is_char_boundary(end) { end+=1; }
    format!("{}{notice}{}",&text[..start],&text[end..])
}

impl Engine {
    /// Send one bounded result in the original conversation; retry never executes the command again.
    pub(super) fn finish_output(&mut self) -> Result<()> {
        let record=self.processes.get(self.saved.pending_output.unwrap())?;
        ensure!(record.finished_at.is_some() || record.background_pids.is_some(),"Command is still running");
        self.saved.messages.push(Message { attachments:None, role:Role::Tool,text:preview(&record.result()) });
        self.saved.pending_output=None;
        if let Err(error)=self.prepare(record.proposal.cwd.clone(),Uuid::new_v4()) {
            self.saved.messages.pop();
            self.saved.pending_output=Some(record.id);
            self.save()?;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use axum::{routing::post,Json,Router,http::StatusCode,response::IntoResponse};

    /// Keep short output exact and retain boundary evidence from long international output.
    #[test]
    fn preview_preserves_ends_and_unicode() {
        assert_eq!(preview("short 世界\n"),"short 世界\n");
        let output=preview(&format!("stdout:\nHEAD\n{}\nstderr:\nTAIL", "世界🙂\0\n".repeat(10_000)));
        assert!(output.len()<=INLINE_LIMIT);
        assert!(output.starts_with("stdout:\nHEAD\n"));
        assert!(output.ends_with("stderr:\nTAIL"));
        assert!(output.contains("Output truncated: middle omitted"));
    }

    /// A long real shell result uses only the main conversation and survives a lost reply without rerunning.
    #[tokio::test]
    async fn long_output_stays_in_main_chat_and_retries_once() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let requests=Arc::new(Mutex::new(Vec::<ChatRequest>::new()));
        let seen=requests.clone();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let backend=format!("http://{}",listener.local_addr()?);
        let app=Router::new().route("/v1/chat",post(move |Json(request):Json<ChatRequest>| {
            let seen=seen.clone();
            async move {
                let mut requests=seen.lock().unwrap();
                assert!(serde_json::to_vec(&request).unwrap().len()<=MAX_REQUEST_BYTES);
                requests.push(request.clone());
                if requests.len()==2 {
                    return (StatusCode::INTERNAL_SERVER_ERROR,Json(json!({"error":"Ambiguous fixture failure"}))).into_response();
                }
                let command=(request.messages.last().unwrap().role==Role::User).then(||CommandProposal { files:None,
                    cwd:request.cwd,reason:"Inspect a diagnostic log".into(),
                    command:if cfg!(windows) { "Add-Content -LiteralPath 'runs.txt' -Value 'run'; [Console]::Out.WriteLine('HEAD_MARKER'); [Console]::Out.Write(('世界🙂 '*10000)); [Console]::Error.WriteLine('TAIL_ERROR')" }
                        else { "printf 'run\\n' >> runs.txt; printf 'HEAD_MARKER\\n'; i=0; while [ $i -lt 10000 ]; do printf '世界🙂 '; i=$((i+1)); done; printf 'TAIL_ERROR\\n' >&2" }.into(),
                });
                ([("content-type","text/event-stream")],format!("data: {}\n\n",serde_json::to_string(&protocol::ChatEvent::Complete {
                    response:ChatResponse { task_credits:Some(1), progress:None, reply:protocol::AssistantReply { text:"Fixture response".into(),command },credits:50,revision:request.base+request.messages.len()+1,steering:vec![] }
                }).unwrap())).into_response()
            }
        }));
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.set_backend(backend.clone()).await?;
        engine.saved.token=Some("test-session".into());
        engine.settings(true)?;
        engine.chat("Inspect diagnostics".into(),directory.path().to_string_lossy().into_owned()).await?;
        let id=engine.saved.pending.as_ref().unwrap().id;
        assert!(engine.decide(id,true).await.is_err());
        assert!(engine.saved.pending_output.is_none() && engine.saved.pending.is_none());
        let request_id=engine.saved.request.as_ref().unwrap().request_id;
        drop(engine);
        let mut engine=Engine::open(directory.path(),&backend).await?;
        engine.retry().await?;
        assert_eq!(std::fs::read_to_string(directory.path().join("runs.txt"))?.lines().count(),1);
        assert!(engine.processes.get(id)?.stdout.len()>100_000);
        let requests=requests.lock().unwrap();
        assert_eq!(requests.len(),3);
        assert_eq!(requests[2].request_id,request_id);
        assert_eq!(requests[0].task_id,requests[0].request_id);
        assert!(requests.iter().all(|r|r.task_id==requests[0].task_id));
        assert!(engine.saved.task_id.is_none());
        assert!(requests.iter().all(|r|r.conversation_id==engine.saved.conversation_id));
        let output=&requests[2].messages.last().unwrap().text;
        assert!(output.len()<=INLINE_LIMIT && output.contains("HEAD_MARKER") && output.contains("TAIL_ERROR"));
        assert!(output.contains("Output truncated: middle omitted"));
        assert_eq!(engine.saved.messages.iter().filter(|m|m.text==*output).count(),1);
        server.abort();
        Ok(())
    }
}
