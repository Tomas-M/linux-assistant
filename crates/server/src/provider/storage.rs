use super::*;
use std::{path::Path,time::UNIX_EPOCH};
use rusqlite::{Connection,OpenFlags};

const CACHE_BYTES:u64=1024*1024*1024;
struct Rollout { account:Arc<ProviderAccount>,thread:String,bytes:u64,modified:i64 }

/// Scan only real directories below the dedicated session roots; never follow filesystem links.
fn rollouts(path:&Path,account:&Arc<ProviderAccount>,files:&mut Vec<Rollout>)->Result<()> {
    if !path.try_exists()? { return Ok(()); }
    ensure!(std::fs::symlink_metadata(path)?.is_dir(),"Session cache must be a real directory");
    for entry in std::fs::read_dir(path)? {
        let entry=entry?; let kind=entry.file_type()?;
        if kind.is_dir() { rollouts(&entry.path(),account,files)?; }
        if !kind.is_file() { continue; }
        let name=entry.file_name(); let name=name.to_string_lossy();
        let Some(stem)=name.trim_end_matches(".zst").strip_suffix(".jsonl").filter(|name|name.starts_with("rollout-")) else { continue; };
        let Some(thread)=stem.get(stem.len().saturating_sub(36)..).and_then(|value|uuid::Uuid::parse_str(value).ok()) else { continue; };
        let metadata=entry.metadata()?;
        let generated=Path::new(&account.home).join("generated_images").join(thread.to_string());
        let mut bytes=metadata.len();
        if generated.try_exists()? && std::fs::symlink_metadata(&generated)?.is_dir() {
            for image in std::fs::read_dir(generated)? { let image=image?; if image.file_type()?.is_file() { bytes+=image.metadata()?.len(); } }
        }
        files.push(Rollout { account:account.clone(),thread:thread.to_string(),bytes,modified:metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs() as i64 });
    }
    Ok(())
}

/// Native databases are included by live page usage so reusable free pages do not trigger over-eviction.
fn databases(home:&Path)->Result<Vec<PathBuf>> {
    Ok(std::fs::read_dir(home)?.filter_map(|entry|entry.ok()).filter(|entry|entry.file_type().is_ok_and(|kind|kind.is_file()) && entry.path().extension().is_some_and(|extension|extension=="sqlite" || extension=="db")).map(|entry|entry.path()).collect())
}

impl Pool {
    /// A root-owned installed executable may not be hard-linked by the service user; copy it at most once.
    pub(super) async fn runtime_executable(&self)->Result<PathBuf> {
        if crate::embedded::installed_helper(&self.executable) { return Ok(self.executable.clone()); }
        let mut runtime=self.runtime.lock().await;
        if runtime.is_none() {
            let executable=self.executable.clone();
            let parent=self.managed.parent().filter(|path|!path.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_owned();
            *runtime=Some(tokio::task::spawn_blocking(move||crate::embedded::runtime(&executable,&parent)).await??);
        }
        Ok(runtime.as_ref().unwrap().1.clone())
    }

    /// No workers are running at startup; remove abandoned executable aliases and old binary copies.
    pub fn clean_runtimes(&self)->Result<()> {
        for entry in std::fs::read_dir(self.managed.parent().unwrap())? {
            let entry=entry?;
            if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().starts_with("runtime-") { std::fs::remove_dir_all(entry.path())?; }
        }
        if !self.managed.try_exists()? { return Ok(()); }
        ensure!(std::fs::symlink_metadata(&self.managed)?.is_dir(),"Managed account root must be a real directory");
        for home in std::fs::read_dir(&self.managed)? {
            let home=home?;
            if !home.file_type()?.is_dir() || home.file_name().to_str().is_none_or(|name|uuid::Uuid::parse_str(name).is_err()) { continue; }
            for entry in std::fs::read_dir(home.path())? {
                let entry=entry?;
                if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().starts_with("runtime-") { std::fs::remove_dir_all(entry.path())?; }
            }
        }
        Ok(())
    }

    /// Native deletion removes rollout, history and attachment records together; active turns remain protected.
    pub async fn prune(&self,db:&Db)->Result<()> {
        let mut files=Vec::new(); let mut total=0u64;
        for account in self.accounts() {
            let _access=account.access.read().await;
            if !self.registered(&account) { continue; }
            let snapshot=account.clone();
            let snapshot=tokio::task::spawn_blocking(move||->Result<_> {
                let home=Path::new(&snapshot.home); let mut files=Vec::new(); let mut bytes=0;
                rollouts(&home.join("sessions"),&snapshot,&mut files)?;
                rollouts(&home.join("archived_sessions"),&snapshot,&mut files)?;
                let logs=home.join("logs_2.sqlite");
                if logs.try_exists()? {
                    let connection=Connection::open(logs)?;
                    connection.busy_timeout(Duration::from_secs(1))?;
                    // Codex caps each log partition separately; also bound their combined account size and age.
                    connection.execute("DELETE FROM logs WHERE ts<?1",[now()-crate::storage::RETENTION])?;
                    connection.execute("DELETE FROM logs WHERE id IN (SELECT id FROM (SELECT id,SUM(estimated_bytes) OVER (ORDER BY id DESC) AS bytes FROM logs) WHERE bytes>16777216)",[])?;
                }
                for path in databases(home)? {
                    let connection=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                    let pages:i64=connection.query_row("PRAGMA page_count",[],|row|row.get(0))?;
                    let free:i64=connection.query_row("PRAGMA freelist_count",[],|row|row.get(0))?;
                    let size:i64=connection.query_row("PRAGMA page_size",[],|row|row.get(0))?;
                    bytes+=((pages-free)*size) as u64;
                }
                Ok((files,bytes))
            }).await.map_err(anyhow::Error::from).and_then(|result|result);
            let (found,bytes)=match snapshot {
                Ok(snapshot)=>snapshot,
                Err(error)=> { crate::diagnostics::record("error","storage_cleanup",None,Some(&account.home),&format!("Scanning provider cache: {error:#}")); continue; }
            };
            total+=bytes+found.iter().map(|file|file.bytes).sum::<u64>(); files.extend(found);
        }
        files.sort_by_key(|file|file.modified);
        for file in files {
            // Give newly created threads time to register their transport and usage ownership.
            if file.modified>now()-60 { continue; }
            let _access=file.account.access.read().await;
            if !self.registered(&file.account) { continue; }
            let referenced:bool=db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM conversations WHERE thread_id=?1)",[&file.thread],|row|row.get(0))?;
            if referenced && file.modified>=now()-crate::storage::RETENTION && total<=CACHE_BYTES { continue; }
            let mut rpc=match tokio::time::timeout(Duration::from_secs(20),self.rpc(&file.account)).await {
                Ok(Ok(rpc))=>rpc,
                result=> { crate::diagnostics::record("warning","storage_cleanup",None,Some(&file.account.home),&format!("Starting cleanup worker: {}",match result { Ok(Err(error))=>format!("{error:#}"),_=>"Timed out".into() })); continue; }
            };
            if rpc.has_thread(&file.thread) || !db.retire_thread(&file.thread)? { continue; }
            let result=tokio::time::timeout(Duration::from_secs(20),rpc.call("thread/delete",serde_json::json!({"threadId":file.thread}))).await;
            match result {
                Ok(Ok(_))=> {
                    total=total.saturating_sub(file.bytes);
                    let generated=Path::new(&file.account.home).join("generated_images").join(&file.thread);
                    if generated.try_exists()? && std::fs::symlink_metadata(&generated)?.is_dir() {
                        ensure!(generated.canonicalize()?.starts_with(Path::new(&file.account.home).canonicalize()?),"Generated image cache escaped the account home");
                        std::fs::remove_dir_all(generated)?;
                    }
                }
                result=>crate::diagnostics::record("warning","storage_cleanup",None,Some(&file.account.home),&format!("Could not remove expired provider thread {}: {result:?}",file.thread)),
            }
        }
        for account in self.accounts() {
            // Vacuum only while idle and only when it can reclaim at least 32 MiB.
            let Ok(_access)=account.access.try_write() else { continue; };
            if !self.registered(&account) { continue; }
            let paths=databases(Path::new(&account.home))?;
            let mut compact=Vec::new();
            for path in paths {
                let connection=Connection::open_with_flags(&path,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let free:i64=connection.query_row("PRAGMA freelist_count",[],|row|row.get(0))?;
                let size:i64=connection.query_row("PRAGMA page_size",[],|row|row.get(0))?;
                if free*size>=32*1024*1024 { compact.push(path); }
            }
            if compact.is_empty() { continue; }
            if let Some(worker)=account.worker.lock().await.take() { worker.shutdown().await?; }
            tokio::task::spawn_blocking(move||->Result<()> {
                for path in compact { crate::storage::reclaim(&Connection::open(path)?,true)?; }
                Ok(())
            }).await??;
        }
        Ok(())
    }

    /// Cancel pending account sign-in and close all transports concurrently after turns settle.
    pub async fn shutdown(&self)->Result<()> {
        if let Some(mut login)=self.login.lock().await.take() { login.task.abort(); let _=(&mut login.task).await; }
        let mut tasks=tokio::task::JoinSet::new();
        for account in self.accounts() {
            tasks.spawn(async move {
                let _access=account.access.write().await;
                if let Some(worker)=account.worker.lock().await.take() { worker.shutdown().await?; }
                Ok::<_,anyhow::Error>(())
            });
        }
        tokio::time::timeout(Duration::from_secs(10),async {
            while let Some(result)=tasks.join_next().await { result??; }
            Ok::<_,anyhow::Error>(())
        }).await.context("Provider shutdown timed out")??;
        self.runtime.lock().await.take();
        Ok(())
    }
}
