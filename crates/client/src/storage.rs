use std::{fs, io::Write, path::{Path, PathBuf}};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};

/// Keep local credentials and history in a user-private, explicitly overridable directory.
pub fn directory() -> Result<PathBuf> {
    let path = match std::env::var_os("LINUX_ASSISTANT_HOME") {
        Some(path) => PathBuf::from(path),
        None => dirs::data_local_dir().context("Cannot locate user data directory")?.join("linux-assistant"),
    };
    fs::create_dir_all(&path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path,fs::Permissions::from_mode(0o700))?;
    }
    Ok(path.canonicalize()?)
}

/// Atomically replace private state so a crash cannot leave half-written JSON.
pub fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut file,value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// Missing state is handled by callers; malformed state must fail visibly.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// An explicit reset replaces conversation state without migrating it or changing credentials.
pub fn reset_chat(directory:&Path)->Result<()> {
    use fs2::FileExt;
    let lock=fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(directory.join("daemon.lockfile"))?;
    lock.try_lock_exclusive().context("Stop the local assistant before resetting its chat")?;
    let path=directory.join("state.json");
    if path.exists() {
        let previous:serde_json::Value=read(&path)?;
        fs::copy(&path,directory.join(format!("state-before-reset-{}.json",uuid::Uuid::new_v4())))?;
        let saved=crate::engine::Saved { conversation_id:uuid::Uuid::new_v4(),settings:serde_json::from_value(previous["settings"].clone())?,
            token:serde_json::from_value(previous["token"].clone())?,messages:vec![],committed:0,pending:None,request:None,pending_output:None };
        write(&path,&saved)?;
    }
    write(&directory.join("queue.json"),&Vec::<crate::controls::Queued>::new())
}
