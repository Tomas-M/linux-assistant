use anyhow::Result;
use protocol::CommandProposal;
use std::sync::{Arc,Mutex};
use crate::processes::Record;

#[cfg(windows)]
mod windows;
#[cfg(all(test,windows))]
mod windows_tests;

/// Refuse unsupported hosts rather than executing with a guessed shell.
#[cfg(not(any(target_os = "linux",windows)))]
pub async fn execute(_proposal: &CommandProposal, _record: Arc<Mutex<Record>>, _cancel: tokio::sync::watch::Receiver<bool>, _output_limit: usize) -> Result<()> {
    anyhow::bail!("Local command execution requires Linux or Windows")
}

/// Run approved shell text as the current user with bounded captured output.
#[cfg(all(test,any(target_os = "linux",windows)))]
pub async fn run(proposal: &CommandProposal) -> Result<String> {
    let record=Arc::new(Mutex::new(Record::new(uuid::Uuid::new_v4(),proposal.clone())));
    let (_cancel,receiver)=tokio::sync::watch::channel(false);
    execute(proposal,record.clone(),receiver,24_000).await?;
    let mut output=record.lock().unwrap().result();
    if record.lock().unwrap().stdout_bytes>24_000 { output.push_str("\n[output truncated]"); }
    if record.lock().unwrap().stderr_bytes>24_000 { output.push_str("\n[output truncated]"); }
    Ok(output)
}

/// Stream into the independent process record while retaining native process-tree ownership.
#[cfg(any(target_os = "linux",windows))]
pub async fn execute(proposal: &CommandProposal, record: Arc<Mutex<Record>>, mut cancel: tokio::sync::watch::Receiver<bool>, output_limit: usize) -> Result<()> {
    use std::{process::Stdio, time::Duration};
    use tokio::process::Command;
    anyhow::ensure!(std::path::Path::new(&proposal.cwd).is_absolute(),"Command directory must be absolute");
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c",&proposal.command]).process_group(0);
        command
    };
    #[cfg(windows)]
    let mut command = {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        let root = std::env::var_os("SystemRoot").ok_or_else(||anyhow::anyhow!("Windows SystemRoot is missing"))?;
        let mut command = Command::new(std::path::Path::new(&root).join("System32/WindowsPowerShell/v1.0/powershell.exe"));
        // Pass one Unicode argument directly; never interpret it through cmd.exe or a user profile.
        let script = format!("[Console]::InputEncoding = [Console]::OutputEncoding = $OutputEncoding = New-Object System.Text.UTF8Encoding; $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; & {{\n{}\n}}; exit $LASTEXITCODE",proposal.command);
        command.args(["-NoLogo","-NoProfile","-NonInteractive","-OutputFormat","Text","-Command",&script])
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        command
    };
    command.current_dir(&proposal.cwd).stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for (key,_) in std::env::vars_os() {
        if key.to_string_lossy().to_ascii_uppercase().starts_with("LINUX_ASSISTANT_") { command.env_remove(key); }
    }
    {
        let mut record=record.lock().unwrap();
        record.program=command.as_std().get_program().to_string_lossy().into_owned();
        record.arguments=command.as_std().get_args().map(|arg|arg.to_string_lossy().into_owned()).collect();
    }
    let mut child = command.spawn()?;
    record.lock().unwrap().pid=child.id();
    #[cfg(target_os = "linux")]
    let group = ProcessGroup(child.id().unwrap() as i32);
    #[cfg(windows)]
    let group = windows::contain(&child)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out_record=record.clone();
    let err_record=record.clone();
    let out = tokio::spawn(async move { capture(stdout,out_record,false,output_limit).await });
    let err = tokio::spawn(async move { capture(stderr,err_record,true,output_limit).await });
    let status = tokio::select! {
        status=child.wait() => Some(status),
        _=async { if !*cancel.borrow() { let _=cancel.changed().await; } } => {
            record.lock().unwrap().error=Some("Stopped by user; command process tree terminated.".into());
            None
        },
    };
    drop(group);
    if let Some(status)=status { record.lock().unwrap().exit_code=status?.code(); }
    else { let _=child.wait().await; }
    for mut task in [out,err] {
        match tokio::time::timeout(Duration::from_secs(1),&mut task).await {
            Ok(result) => result??,
            Err(_) => { task.abort(); record.lock().unwrap().error=Some("Output pipe left open by a detached process; capture is incomplete.".into()); }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct ProcessGroup(i32);
#[cfg(target_os = "linux")]
impl Drop for ProcessGroup {
    /// Also kill descendants when execution is cancelled or its shell exits early.
    fn drop(&mut self) { unsafe { libc::kill(-self.0,libc::SIGKILL); } }
}

/// Retain a bounded prefix, expose live output, and continue draining both pipes after the cap.
#[cfg(any(target_os = "linux",windows))]
async fn capture(mut stream: impl tokio::io::AsyncRead + Unpin, record: Arc<Mutex<Record>>, stderr: bool, limit: usize) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut saved = Vec::new();
    let mut buffer = [0u8;8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 { break; }
        let keep = count.min(limit.saturating_sub(saved.len()));
        saved.extend_from_slice(&buffer[..keep]);
        let mut record=record.lock().unwrap();
        if stderr {
            if keep>0 { record.stderr=String::from_utf8_lossy(&saved).into_owned(); }
            record.stderr_bytes+=count;
        } else {
            if keep>0 { record.stdout=String::from_utf8_lossy(&saved).into_owned(); }
            record.stdout_bytes+=count;
        }
    }
    Ok(())
}

#[cfg(all(test,target_os = "linux"))]
mod tests {
    use super::*;
    /// Capture Unicode and nonzero exit statuses without treating shell failure as transport failure.
    #[tokio::test]
    async fn captures_actual_process_results() -> Result<()> {
        let result = run(&CommandProposal {  command:"printf 'Žluťoučký 世界'; printf 'problem' >&2; exit 7".into(),cwd:"/tmp".into(),reason:"test".into() }).await?;
        assert!(result.contains("Žluťoučký 世界") && result.contains("problem") && result.contains('7'));
        Ok(())
    }
    /// Output caps must not block a command that writes more than the pipe buffer.
    #[tokio::test]
    async fn bounds_noisy_output() -> Result<()> {
        let result = run(&CommandProposal {  command:"head -c 200000 /dev/zero".into(),cwd:"/tmp".into(),reason:"test".into() }).await?;
        assert!(result.len()<25_000 && result.contains("truncated"));
        Ok(())
    }
}
