use std::{fs::OpenOptions, os::windows::{ffi::OsStrExt,io::{AsRawHandle,IntoRawHandle}}, path::Path};
use anyhow::Result;
use windows_sys::Win32::{Foundation::CloseHandle, System::{Console::{SetStdHandle,STD_ERROR_HANDLE,STD_OUTPUT_HANDLE},Threading::{CreateProcessW,PROCESS_INFORMATION,STARTUPINFOW,DETACHED_PROCESS,CREATE_NEW_PROCESS_GROUP}}};

/// Start a detached daemon without inheriting PowerShell's capture pipes or any other launcher handles.
pub fn spawn(backend:&str) -> Result<()> {
    let executable=std::env::current_exe()?;
    let executable:Vec<u16>=executable.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut command=vec![b'"' as u16];
    command.extend_from_slice(&executable[..executable.len()-1]);
    command.extend(format!("\" --daemon --backend \"{backend}\"\0").encode_utf16());
    let startup=STARTUPINFOW { cb:std::mem::size_of::<STARTUPINFOW>() as u32,..unsafe { std::mem::zeroed() } };
    let mut process:PROCESS_INFORMATION=unsafe { std::mem::zeroed() };
    // No handles are inherited. The child opens its own log after startup.
    let created=unsafe { CreateProcessW(executable.as_ptr(),command.as_mut_ptr(),std::ptr::null(),std::ptr::null(),0,
        DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP,std::ptr::null(),std::ptr::null(),&startup,&mut process) };
    if created==0 { return Err(std::io::Error::last_os_error().into()); }
    unsafe { CloseHandle(process.hThread); CloseHandle(process.hProcess); }
    Ok(())
}

/// Attach daemon diagnostics to its own log, keeping both standard handles alive for the process lifetime.
pub fn redirect_log(directory: &Path) -> Result<()> {
    let log=OpenOptions::new().create(true).append(true).open(directory.join("daemon.log"))?;
    for stream in [STD_OUTPUT_HANDLE,STD_ERROR_HANDLE] {
        if unsafe { SetStdHandle(stream,log.as_raw_handle()) }==0 { return Err(std::io::Error::last_os_error().into()); }
    }
    let _=log.into_raw_handle();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Capturing stdout must reach EOF while the detached server remains alive and reusable.
    #[tokio::test]
    #[ignore = "Build linux-assistant and set ASSISTANT_TEST_CLIENT to its executable"]
    async fn integration_launcher_releases_capture_pipes() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let executable=std::env::var("ASSISTANT_TEST_CLIENT")?;
        let result=async {
            let mut previous=String::new();
            for _ in 0..2 {
                let output=tokio::time::timeout(Duration::from_secs(15),tokio::process::Command::new(&executable)
                    .args(["--backend","assistant.example.test"]).env("LINUX_ASSISTANT_HOME",directory.path()).kill_on_drop(true).output()).await??;
                anyhow::ensure!(output.status.success(),"Launcher failed");
                let url=String::from_utf8(output.stdout)?;
                anyhow::ensure!(url.starts_with("http://127.0.0.1:") && url.contains("/#"),"Missing browser URL");
                if !previous.is_empty() { assert_eq!(previous,url); }
                previous=url;
                let connection=crate::storage::read::<crate::local::Connection>(&directory.path().join("daemon.json"))?;
                connection.get("/api/health").await?;
            }
            Ok::<_,anyhow::Error>(())
        }.await;
        if let Ok(connection)=crate::storage::read::<crate::local::Connection>(&directory.path().join("daemon.json")) {
            let _=connection.post("/api/stop",serde_json::json!({})).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        result
    }
}
