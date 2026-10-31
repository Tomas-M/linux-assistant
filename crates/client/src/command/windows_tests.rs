use super::*;
use std::{os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, time::Duration};
use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE}};

/// Exercise UTF-16 argument passing, UTF-8 output, quoting, working directories and explicit failure.
#[tokio::test]
async fn captures_unicode_quotes_directory_and_exit() -> Result<()> {
    let dir = tempfile::Builder::new().prefix("assistant Žluťoučký 世界 ").tempdir()?;
    std::fs::write(dir.path().join("日本語 ' file.txt"),"Příliš žluťoučký 世界")?;
    let result = run(&CommandProposal { files:None,
        command:"Get-Content -Encoding UTF8 -LiteralPath \"日本語 ' file.txt\"; Write-Output 'quote \" and $literal'; [Console]::Error.WriteLine('problem'); exit 7".into(),
        cwd:dir.path().to_string_lossy().into_owned(),reason:"test".into(),
    }).await?;
    assert!(result.contains("Exit status: exit code: 7"),"{result}");
    assert!(result.contains("Příliš žluťoučký 世界") && result.contains("quote \" and $literal") && result.contains("problem"),"{result}");
    Ok(())
}

/// PowerShell errors and native exit codes must survive the wrapper.
#[tokio::test]
async fn reports_cmdlet_and_native_failures() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for (script,code) in [("Write-Error 'failed'; Write-Output 'must not run'","1"),("& $env:ComSpec /d /c 'exit 9'","9")] {
        let result = run(&CommandProposal { files:None,  command:script.into(),cwd:dir.path().to_string_lossy().into_owned(),reason:"test".into() }).await?;
        assert!(result.contains(&format!("Exit status: exit code: {code}")) && !result.contains("\nstdout:\nmust not run"),"{result}");
    }
    Ok(())
}

/// Both pipes must keep draining after their retained output reaches the cap.
#[tokio::test]
async fn bounds_both_output_streams() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let result = run(&CommandProposal { files:None,  command:"[Console]::Out.Write(('界'*200000)); [Console]::Error.Write(('é'*200000))".into(),
        cwd:dir.path().to_string_lossy().into_owned(),reason:"test".into() }).await?;
    assert_eq!(result.matches("[output truncated]").count(),2);
    assert!(result.len()<49_000);
    Ok(())
}

/// Resolve a real spawned descendant before triggering cleanup, avoiding PID-reuse assertions.
async fn descendant(dir: &std::path::Path) -> Result<OwnedHandle> {
    tokio::time::timeout(Duration::from_secs(15),async {
        loop {
            if let Ok(text) = std::fs::read_to_string(dir.join("child.pid")) {
                if let Ok(pid) = text.trim().parse() {
                    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE,0,pid) };
                    anyhow::ensure!(!handle.is_null(),"Cannot open test descendant");
                    return Ok(unsafe { OwnedHandle::from_raw_handle(handle) });
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await?
}

/// A Stop request or dropped execution future must kill descendants, not only the PowerShell parent.
#[tokio::test]
async fn kills_process_tree_on_stop_and_cancellation() -> Result<()> {
    for abort in [false,true] {
        let dir = tempfile::tempdir()?;
        let proposal = CommandProposal { files:None,
            command:"$child = Start-Process -FilePath \"$PSHOME\\powershell.exe\" -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep 60' -WindowStyle Hidden -PassThru; [IO.File]::WriteAllText((Join-Path $PWD 'child.pid'),[string]$child.Id); Start-Sleep 60".into(),
            cwd:dir.path().to_string_lossy().into_owned(),reason:"test".into(),
        };
        let record=Arc::new(Mutex::new(Record::new(uuid::Uuid::new_v4(),proposal.clone())));
        let running=record.clone();
        let (stop,receiver)=tokio::sync::watch::channel(false);
        let task=tokio::spawn(async move { execute(&proposal,running,receiver,24_000,None,Arc::new(Default::default()),None).await });
        let child = match descendant(dir.path()).await {
            Ok(child) => child,
            Err(error) => { task.abort(); let _ = task.await; return Err(error); }
        };
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            stop.send(true)?;
            tokio::time::timeout(Duration::from_secs(5),task).await???;
            assert!(record.lock().unwrap().error.as_deref().unwrap().contains("Stopped by user"));
        }
        assert_eq!(unsafe { WaitForSingleObject(child.as_raw_handle(),5000) },WAIT_OBJECT_0,"Descendant survived cleanup");
    }
    Ok(())
}

/// Relative paths and missing directories must fail before arbitrary commands can run.
#[tokio::test]
async fn rejects_invalid_working_directories() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for cwd in ["relative".into(),dir.path().join("missing").to_string_lossy().into_owned()] {
        assert!(run(&CommandProposal { files:None,  command:"Write-Output 'unexpected'".into(),cwd,reason:"test".into() }).await.is_err());
    }
    Ok(())
}
