use super::*;
use protocol::LocalAction;
use tokio::io::AsyncWriteExt;
use std::time::{Duration,Instant};

#[derive(Serialize,Deserialize)]
pub(crate) struct OperationOutput { pub cwd:String,pub text:String }

impl Processes {
    /// Read the durable acknowledgement, never repeat an operation whose response was lost.
    pub(crate) fn output(&self,id:Uuid)->Result<OperationOutput> { storage::read(&self.directory.join("operations").join(format!("{id}.json"))) }

    /// Execute one approved operation and commit its output before advancing the session cursors.
    pub(crate) async fn perform(&self,id:Uuid,action:&LocalAction,cwd:&str,yield_now:&tokio::sync::Notify)->Result<()> {
        action.validate()?;
        let directory=self.directory.join("operations");
        std::fs::create_dir_all(&directory)?;
        let path=directory.join(format!("{id}.json"));
        if path.exists() { return Ok(()); }
        storage::write(&path,&OperationOutput { cwd:cwd.into(),text:"This operation's outcome is unknown because it was interrupted before acknowledgement. Do not repeat the command or stdin automatically; inspect the session or ask the user.".into() })?;
        let result:Result<()>=async {
            let session=action.session_id(id);
            if let LocalAction::ExecCommand { cmd,workdir,reason,files,yield_time_ms,timeout_ms }=action {
                self.start(session,&CommandProposal { command:cmd.clone(),cwd:workdir.clone(),reason:reason.clone(),files:Some(files.clone()) },*yield_time_ms,*timeout_ms,yield_now).await?;
            }
            let (record,input,cursor,files)={
                let entries=self.entries.lock().unwrap();
                let entry=entries.get(&session).context("Unknown local session; it may belong to a previous client instance. Do not rerun it automatically")?;
                (entry.record.clone(),entry.input.clone(),entry.cursor.clone(),entry.files.clone())
            };
            let mut cursor=cursor.lock().await;
            let wait=match action {
                LocalAction::WriteStdin { chars,close_stdin,yield_time_ms,.. }=> {
                    if !chars.is_empty() || *close_stdin {
                        ensure!(record.lock().unwrap().finished_at.is_none(),"Session has finished");
                        let mut pipe=input.lock().await;
                        let stdin=pipe.as_mut().context("Session stdin is closed")?;
                        tokio::time::timeout(Duration::from_secs(5),stdin.write_all(chars.as_bytes())).await.context("Input delivery timed out; some bytes may have been sent. Do not resend automatically")??;
                        if *close_stdin { pipe.take(); }
                    }
                    let minimum=if chars.is_empty() && !*close_stdin { 5_000 } else { 250 };
                    (*yield_time_ms).clamp(minimum,if action.is_poll() { 300_000 } else { 30_000 })
                }
                LocalAction::StopProcess { .. }=> {
                    if record.lock().unwrap().finished_at.is_none() { self.stop(session)?; }
                    5_000
                }
                LocalAction::ExecCommand { .. }=>0,
            };
            let deadline=Instant::now()+Duration::from_millis(wait);
            loop {
                let done={ let record=record.lock().unwrap(); record.finished_at.is_some() || (!matches!(action,LocalAction::StopProcess { .. }) && record.background_pids.is_some()) };
                if done || Instant::now()>=deadline { break; }
                tokio::select! {
                    _=tokio::time::sleep(Duration::from_millis(50))=> {}
                    _=yield_now.notified()=>break,
                }
            }
            let before=files.lock().unwrap().take();
            if let Some(before)=before {
                let (before,changes)=tokio::task::spawn_blocking(move|| { let changes=crate::changes::compare(before.clone()); (before,changes) }).await?;
                let mut current=record.lock().unwrap();
                current.changes=Some(changes);
                if current.finished_at.is_none() && current.background_pids.is_none() { *files.lock().unwrap()=Some(before); }
            }
            let (text,next,cwd)={
                let record=record.lock().unwrap();
                let running=record.finished_at.is_none() && record.background_pids.is_none();
                let exit=record.exit_code.map_or_else(||"not available".into(),|code|code.to_string());
                let mut output=format!("Session ID: {}\nRunning: {running}\nLauncher exit code: {exit}\n",record.id);
                if let Some(error)=&record.error { output.push_str(&format!("Error: {error}\n")); }
                if let Some(pids)=record.background_pids.as_ref().filter(|pids|!pids.is_empty()) { output.push_str(&format!("Background PIDs: {pids:?}. The launcher has finished; do not rerun it.\n")); }
                let mut next=[0,0];
                for (index,name,text,start,total) in [(0,"stdout",&record.stdout,cursor.0,record.stdout_bytes),(1,"stderr",&record.stderr,cursor.1,record.stderr_bytes)] {
                    let tail=&record.tails[index];
                    let begin=total.saturating_sub(tail.len());
                    output.push_str(&format!("{name} (new output):\n"));
                    if start<begin {
                        if start==0 {
                            let mut head=text.len().min(2048);
                            while !text.is_char_boundary(head) { head-=1; }
                            output.push_str(&text[..head]);
                        }
                        output.push_str(&format!("\n[Intermediate output omitted; rolling buffer starts at byte {begin}, {total} total bytes produced]\n"));
                    }
                    let bytes=tail.iter().skip(start.saturating_sub(begin).min(tail.len())).copied().collect::<Vec<_>>();
                    // Keep a split UTF-8 character for the next poll instead of emitting a replacement twice.
                    let end=match std::str::from_utf8(&bytes) { Err(error) if error.error_len().is_none()=>error.valid_up_to(),_=>bytes.len() };
                    output.push_str(&String::from_utf8_lossy(&bytes[..end]));
                    output.push('\n');
                    next[index]=total-(bytes.len()-end);
                }
                (output,(next[0],next[1]),record.proposal.cwd.clone())
            };
            storage::write(&path,&OperationOutput { cwd,text })?;
            *cursor=next;
            Ok(())
        }.await;
        if let Err(error)=result { storage::write(&path,&OperationOutput { cwd:cwd.into(),text:format!("Local operation failed: {error:#}") })?; }
        Ok(())
    }
}
