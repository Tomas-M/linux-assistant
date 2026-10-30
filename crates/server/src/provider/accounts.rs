use super::*;
use serde_json::json;
use uuid::Uuid;

pub(super) struct Login { id:Uuid, instructions:Value, error:Arc<std::sync::Mutex<Option<String>>>, task:tokio::task::JoinHandle<()> }
impl Login {
    /// Completion and failure are derived from the worker task, never persisted as account states.
    fn view(&self)->Value {
        let mut value=self.instructions.clone();
        value["id"]=json!(self.id); value["finished"]=json!(self.task.is_finished()); value["error"]=json!(*self.error.lock().unwrap());
        value
    }
}
impl Drop for Login {
    /// Cancelling a pending login terminates only its isolated worker, not existing accounts.
    fn drop(&mut self) { self.task.abort(); }
}

impl Pool {
    /// Stable path-derived identifiers cannot target a different account after a backend restart.
    pub(super) fn account(&self,id:&str)->Result<Arc<ProviderAccount>> { self.accounts().into_iter().find(|account|crate::db::hash(&account.home)==id).context("Unknown account") }

    /// A pause prevents new turns while already-running generations retain their account transport.
    pub(super) fn paused(&self,db:&Db,home:&str)->Result<bool> {
        Ok(db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM provider_pauses WHERE home=?1)",[home],|row|row.get(0))?)
    }

    /// Persist pause changes without touching credentials or interrupting active threads.
    pub fn pause(&self,db:&Db,id:&str,paused:bool)->Result<()> {
        let account=self.account(id)?;
        let _access=account.access.try_read().context("Account is being deleted; refresh the dashboard")?;
        ensure!(self.registered(&account),"Unknown account");
        let connection=db.0.lock().unwrap();
        if paused { connection.execute("INSERT OR IGNORE INTO provider_pauses(home) VALUES(?1)",[&account.home])?; }
        else { connection.execute("DELETE FROM provider_pauses WHERE home=?1",[&account.home])?; }
        crate::diagnostics::record("info","account_access",None,Some(&account.home),if paused { "Account paused" } else { "Account resumed" });
        Ok(())
    }

    /// Delete only a managed account after all its users release their shared access guards.
    pub async fn remove(&self,db:&Db,id:&str)->Result<()> {
        let account=self.account(id)?;
        let _access=account.access.try_write().context("Account is in use. Pause it, wait for active turns or quota refreshes to finish, and retry")?;
        ensure!(self.registered(&account),"Unknown account");
        let home=std::path::Path::new(&account.home).canonicalize()?;
        let managed=self.managed.canonicalize()?;
        ensure!(home==std::path::Path::new(&account.home) && home.parent()==Some(managed.as_path()),"Only managed account directories can be deleted");
        if let Some(worker)=account.worker.lock().await.take() { worker.shutdown().await?; }
        std::fs::remove_dir_all(&home).context("Deleting saved account credentials")?;
        self.accounts.write().unwrap().retain(|saved|!Arc::ptr_eq(saved,&account));
        let mut connection=db.0.lock().unwrap();
        let tx=connection.transaction()?;
        tx.execute("DELETE FROM provider_accounts WHERE home=?1",[&account.home])?;
        tx.execute("DELETE FROM provider_pauses WHERE home=?1",[&account.home])?;
        tx.execute("UPDATE conversations SET account_home=NULL,thread_id=NULL WHERE account_home=?1",[&account.home])?;
        tx.commit()?;
        crate::diagnostics::record("info","account_access",None,Some(&account.home),"Saved account credentials deleted; recorded usage retained");
        Ok(())
    }

    /// Report the current login to authenticated operators, including after a browser reload.
    pub async fn login_status(&self)->Value { self.login.lock().await.as_ref().map(Login::view).unwrap_or(Value::Null) }

    /// Dismiss or cancel exactly the displayed attempt; no credentials or existing accounts are deleted.
    pub async fn cancel_login(&self,id:Uuid)->Result<()> {
        let mut login=self.login.lock().await;
        ensure!(login.as_ref().is_some_and(|login|login.id==id),"Sign-in attempt changed; refresh the dashboard");
        login.take();
        Ok(())
    }

    /// Start isolated device-code sign-in or store an API key through the upstream authentication API.
    pub async fn add(self:Arc<Self>,db:Arc<Db>,key:Option<String>)->Result<Value> {
        let mut login=self.login.lock().await;
        if let Some(pending)=login.as_ref().filter(|login|!login.task.is_finished()) {
            ensure!(key.is_none(),"Finish or cancel the current ChatGPT sign-in first");
            return Ok(pending.view());
        }
        login.take();
        if let Some(key)=&key {
            ensure!((20..=8192).contains(&key.len()) && key.trim()==key,"Enter a valid OpenAI API key without surrounding spaces");
            crate::diagnostics::protect(key);
        }
        let id=Uuid::new_v4();
        let home=self.managed.join(id.to_string());
        std::fs::create_dir_all(&home)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            for directory in [&self.managed,&home] { std::fs::set_permissions(directory,std::fs::Permissions::from_mode(0o700))?; }
        }
        let home=home.canonicalize()?.to_string_lossy().into_owned();
        let mut rpc=tokio::time::timeout(Duration::from_secs(30),Rpc::start_unauthenticated(&self.executable,&home)).await.context("Account worker startup timed out")??;
        let api=key.is_some();
        let params=match key { Some(key)=>json!({"type":"apiKey","apiKey":key}),None=>json!({"type":"chatgptDeviceCode"}) };
        let result=tokio::time::timeout(Duration::from_secs(30),rpc.call("account/login/start",params)).await.context("Starting account sign-in timed out")??;
        if api {
            self.connect(&db,home,rpc).await?;
            return Ok(json!({"id":id,"finished":true,"error":null}));
        }
        let url=reqwest::Url::parse(result["verificationUrl"].as_str().context("Missing sign-in URL")?)?;
        let official=url.host_str().is_some_and(|host|host=="auth.openai.com" || host=="chatgpt.com");
        ensure!(url.scheme()=="https" && official,"Unexpected sign-in URL from provider");
        let instructions=json!({"verification_url":url.as_str(),"user_code":result["userCode"].as_str().context("Missing sign-in code")?,"expires_at":now()+600});
        let error=Arc::new(std::sync::Mutex::new(None)); let failure=error.clone(); let pool=self.clone();
        let task=tokio::spawn(async move {
            let result=tokio::time::timeout(Duration::from_secs(600),async {
                loop {
                    let event=rpc.next().await?;
                    if event["method"]!="account/login/completed" { continue; }
                    ensure!(event["params"]["success"]==true,"ChatGPT sign-in failed: {}",event["params"]["error"].as_str().unwrap_or("Cancelled or expired"));
                    return pool.connect(&db,home.clone(),rpc).await;
                }
            }).await.context("ChatGPT sign-in expired after ten minutes").and_then(|result|result);
            if let Err(error)=result {
                let message=crate::diagnostics::clean(&format!("{error:#}"));
                crate::diagnostics::record("error","account_login",None,Some(&home),&message);
                *failure.lock().unwrap()=Some(message);
            }
        });
        let pending=Login { id,instructions,error,task };
        let view=pending.view(); *login=Some(pending);
        Ok(view)
    }

    /// Authentication files are the durable registry; attach the authenticated worker and probe once.
    async fn connect(&self,db:&Db,home:String,mut rpc:Rpc)->Result<()> {
        let identity=rpc.call("account/read",json!({"refreshToken":false})).await?;
        ensure!(matches!(identity["account"]["type"].as_str(),Some("chatgpt"|"apiKey")),"Provider did not authenticate the account");
        ensure!(std::path::Path::new(&home).join("auth.json").is_file(),"Provider did not save account credentials");
        let account=Arc::new(ProviderAccount { home:home.clone(),access:AsyncRwLock::new(()),worker:Mutex::new(Some(rpc)),limits:Mutex::new(None) });
        self.accounts.write().unwrap().push(account.clone());
        crate::diagnostics::record("info","account_login",None,Some(&home),"Account connected");
        self.probe(db,&account).await
    }
}
