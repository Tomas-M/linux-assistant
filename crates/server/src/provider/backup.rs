use super::*;
use serde::{Deserialize,Serialize};
use uuid::Uuid;

#[derive(Clone,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBackup { pub id:Uuid,pub auth:Value,pub paused:bool }

impl Pool {
    /// Export credentials and pause settings using portable identifiers, never machine-specific paths.
    pub async fn backup(&self,db:&Db)->Result<Vec<AccountBackup>> {
        let _selection=self.selection.lock().await;
        let mut backup=Vec::new();
        for account in self.accounts() {
            let _access=account.access.read().await;
            if !self.registered(&account) { continue; }
            let home=std::path::Path::new(&account.home);
            let id=Uuid::parse_str(home.file_name().and_then(|name|name.to_str()).context("Invalid managed account identifier")?)?;
            let auth=serde_json::from_slice(&std::fs::read(home.join("auth.json"))?).context("Cannot read saved account authentication")?;
            backup.push(AccountBackup { id,auth,paused:self.paused(db,&account.home)? });
        }
        Ok(backup)
    }

    /// Stage credentials, exclude active turns, and roll files back if applying the database settings fails.
    pub async fn restore<F>(&self,backup:&[AccountBackup],apply:F)->Result<()>
    where F:FnOnce(&[(String,bool)])->Result<()>+Send {
        let mut ids=std::collections::HashSet::new();
        for account in backup {
            ensure!(ids.insert(account.id),"Backup contains duplicate AI account identifiers");
            ensure!(account.auth.is_object(),"Each AI account must contain its saved authentication object");
        }
        let mut login=self.login.lock().await;
        ensure!(login.as_ref().is_none_or(|login|login.task.is_finished()),"Finish or cancel the pending ChatGPT sign-in before restoring");
        let _selection=self.selection.lock().await;
        let current=self.accounts();
        let mut access=Vec::new();
        for account in &current { access.push(account.access.try_write().context("Pause AI accounts and wait for active turns or quota refreshes to finish before restoring")?); }
        std::fs::create_dir_all(&self.managed)?;
        let parent=self.managed.parent().unwrap().canonicalize()?;
        let managed=parent.join("managed-accounts");
        ensure!(self.managed.canonicalize()?==managed,"Managed account directory must not be a symbolic link");
        let stage=tempfile::Builder::new().prefix(".settings-restore-").tempdir_in(managed.parent().unwrap())?;
        let incoming=stage.path().join("accounts"); let previous=stage.path().join("previous");
        std::fs::create_dir(&incoming)?;
        let mut restored=Vec::new(); let mut homes=Vec::new();
        for account in backup {
            let home=incoming.join(account.id.to_string()); std::fs::create_dir(&home)?;
            let auth=home.join("auth.json"); std::fs::write(&auth,serde_json::to_vec_pretty(&account.auth)?)?;
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&home,std::fs::Permissions::from_mode(0o700))?;
                std::fs::set_permissions(&auth,std::fs::Permissions::from_mode(0o600))?;
            }
            let home=managed.join(account.id.to_string()).to_string_lossy().into_owned();
            homes.push((home.clone(),account.paused));
            restored.push(Arc::new(ProviderAccount { home,access:AsyncRwLock::new(()),worker:Mutex::new(None),limits:Mutex::new(None) }));
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&incoming,std::fs::Permissions::from_mode(0o700))?;
        }
        for account in &current { if let Some(worker)=account.worker.lock().await.take() { worker.shutdown().await?; } }
        std::fs::rename(&managed,&previous)?;
        let result=std::fs::rename(&incoming,&managed).map_err(anyhow::Error::from).and_then(|_|apply(&homes));
        if let Err(error)=result {
            let rollback=if managed.exists() { std::fs::rename(&managed,&incoming) } else { Ok(()) };
            if let Err(rollback)=rollback.and_then(|_|std::fs::rename(&previous,&managed)) {
                let retained=stage.keep();
                return Err(rollback).with_context(||format!("Restore rollback failed; original account files retained in {}",retained.display()));
            }
            return Err(error);
        }
        *self.accounts.write().unwrap()=restored;
        login.take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Busy accounts are rejected before any replacement file or settings can become active.
    #[tokio::test]
    async fn settings_backup_refuses_active_accounts()->Result<()> {
        let directory=tempfile::tempdir()?;
        let home=directory.path().join("managed-accounts").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&home)?; std::fs::write(home.join("auth.json"),b"{}")?;
        let config=Config { database:directory.path().join("test.db"),..Config::default() };
        let pool=Pool::new(&config)?;
        let account=pool.accounts()[0].clone(); let _busy=account.access.read().await;
        assert!(pool.restore(&[],|_|panic!("Settings must remain unchanged")).await.unwrap_err().to_string().contains("active turns"));
        assert!(home.join("auth.json").exists()); assert_eq!(pool.accounts().len(),1);
        Ok(())
    }
}
