use super::*;
use std::sync::Arc;
use protocol::Role;
use rusqlite::params;
use serde_json::json;

impl Pool {
    /// Read quotas in parallel on the shared transports without interrupting active conversations.
    pub async fn monitor(self: Arc<Self>, db: Arc<Db>) {
        let mut interval=tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            self.clone().refresh(db.clone()).await;
        }
    }

    /// Startup, scheduled refreshes and restored credentials share the same bounded parallel probes.
    pub async fn refresh(self:Arc<Self>,db:Arc<Db>) {
        let mut tasks=tokio::task::JoinSet::new();
        for account in self.accounts() {
            let (pool,db)=(self.clone(),db.clone());
            tasks.spawn(async move { pool.probe(&db,&account).await });
        }
        while let Some(result)=tasks.join_next().await {
            let error=match result { Ok(Ok(()))=>continue,Ok(Err(error))=>format!("{error:#}"),Err(error)=>error.to_string() };
            crate::diagnostics::record("error","account_monitor",None,None,&error);
        }
    }

    /// A failed refresh preserves the last successful reading and clearly marks it as stale.
    pub(super) async fn probe(&self, db: &Db, account: &Arc<ProviderAccount>) -> Result<()> {
        let _access=account.access.read().await;
        if !self.registered(account) { return Ok(()); }
        let mut cached=account.limits.lock().await;
        if cached.is_some_and(|limits|now()-limits.checked_at<300) { return Ok(()); }
        let result=tokio::time::timeout(Duration::from_secs(20),async {
            let mut rpc=self.rpc(account).await.context("Starting authenticated account worker")?;
            let identity=rpc.call("account/read",json!({"refreshToken":false})).await.context("Reading account identity")?;
            let mut limits=if identity["account"]["type"]=="apiKey" { json!({}) } else { rpc.call("account/rateLimits/read",json!({})).await.context("Fetching account quotas")? };
            limits["authType"]=identity["account"]["type"].clone();
            *cached=Some(Limits::read(&limits).context("Parsing account quotas")?);
            Ok::<_,anyhow::Error>((identity,limits))
        }).await.context("Quota refresh timed out after 20 seconds").and_then(|result|result);
        let conn=db.0.lock().unwrap();
        match result {
            Ok((identity,limits)) => {
                let recovered:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE home=?1 AND error IS NOT NULL)",[&account.home],|row|row.get(0))?;
                conn.execute("INSERT INTO provider_accounts(home,checked_at,email,plan,limits) VALUES(?1,?2,?3,?4,?5)
                    ON CONFLICT(home) DO UPDATE SET checked_at=excluded.checked_at,email=excluded.email,plan=excluded.plan,limits=excluded.limits,error=NULL",
                    params![account.home,now(),identity["account"]["email"].as_str(),identity["account"]["planType"].as_str(),limits.to_string()])?;
                if limits["authType"]!="apiKey" { conn.execute("INSERT INTO quota_history(account_home,checked_at,limits) VALUES(?1,?2,?3)",params![account.home,now(),limits.to_string()])?; }
                if recovered { crate::diagnostics::record("info","quota_refresh",None,Some(&account.home),"Quota refresh recovered"); }
            }
            Err(error) => {
                *cached=None;
                let message=crate::diagnostics::clean(&format!("Quota refresh failed: {error:#}"));
                crate::diagnostics::record("error","quota_refresh",None,Some(&account.home),&message);
                conn.execute("INSERT INTO provider_accounts(home,error) VALUES(?1,?2) ON CONFLICT(home) DO UPDATE SET error=excluded.error",
                    params![account.home,message])?;
            }
        }
        Ok(())
    }

    /// Combine stored readings with current worker occupancy; never expose authentication material.
    pub fn account_status(&self, db: &Db) -> Result<Vec<Value>> {
        use rusqlite::OptionalExtension;
        let conn=db.0.lock().unwrap();
        self.accounts().iter().map(|account| {
            let mut row=conn.query_row("SELECT checked_at,email,plan,limits,error FROM provider_accounts WHERE home=?1",[&account.home],|r| {
                Ok(json!({"checked_at":r.get::<_,Option<i64>>(0)?,"email":r.get::<_,Option<String>>(1)?,"plan":r.get::<_,Option<String>>(2)?,
                    "limits":r.get::<_,Option<String>>(3)?,"error":r.get::<_,Option<String>>(4)?}))
            }).optional()?.unwrap_or(json!({"checked_at":null,"limits":null}));
            row["id"]=json!(crate::db::hash(&account.home));
            row["paused"]=json!(conn.query_row("SELECT EXISTS(SELECT 1 FROM provider_pauses WHERE home=?1)",[&account.home],|row|row.get::<_,bool>(0))?);
            row["home"]=json!(account.home);
            row["usage"]=conn.query_row("SELECT COUNT(*),COUNT(total_tokens),SUM(input_tokens),SUM(cached_tokens),SUM(output_tokens) FROM usage WHERE account_home=?1",[&account.home],|row| {
                Ok(json!({"attempts":row.get::<_,i64>(0)?,"measured":row.get::<_,i64>(1)?,"input":row.get::<_,Option<i64>>(2)?,"cached":row.get::<_,Option<i64>>(3)?,"output":row.get::<_,Option<i64>>(4)?}))
            })?;
            let active=account.worker.try_lock().ok().and_then(|worker|worker.as_ref().map(Rpc::active)).unwrap_or(0);
            row["active_requests"]=json!(active);
            row["busy"]=json!(active>0);
            row["limits"]=row["limits"].as_str().map(serde_json::from_str).transpose()?.unwrap_or(Value::Null);
            Ok(row)
        }).collect()
    }

    /// Test only the selected account, with an ephemeral thread, no tools, and no user debit.
    pub async fn test_account(&self, config: &Config, db: &Db, id: &str, prompt: String) -> Result<Value> {
        let account=self.account(id)?;
        let _access=account.access.read().await;
        ensure!(self.registered(&account),"Unknown account");
        ensure!(!self.paused(db,&account.home)?,"Resume this account before running a test");

        let request=protocol::ChatRequest { task_id:uuid::Uuid::new_v4(),base:0,request_id:uuid::Uuid::new_v4(),conversation_id:uuid::Uuid::new_v4(),
            platform:Platform::Linux,messages:vec![protocol::Message { attachments:None, role:Role::User,text:prompt }],commands_enabled:false,cwd:"/".into() };
        let mut attempt=crate::usage::Attempt::start(db,None,&request,&account.home,"admin_test")?;
        let result=tokio::time::timeout(Duration::from_secs(180),async {
            let mut rpc=self.rpc(&account).await?;
            let (reply,thread,_)=rpc.answer(config,&request,None,&Value::Null,false,None,None,&attempt,&mut super::progress::Reporter::new(None,1)).await?;
            rpc.call("thread/unsubscribe",json!({"threadId":thread})).await?;
            Ok::<_,anyhow::Error>(reply)
        }).await;
        let result=result.context("Test timed out").and_then(|result|result);
        let failure=result.as_ref().err().map(|error|crate::diagnostics::clean(&format!("{error:#}")));
        attempt.finish(failure.as_deref())?;
        match result {
            Ok(reply)=>Ok(json!({"text":reply.text,"usage_id":attempt.id})),
            Err(_)=> {
                let message=failure.unwrap();
                crate::diagnostics::record("error","admin_test",Some(&request.request_id.to_string()),Some(&account.home),&message);
                anyhow::bail!("{message}");
            }
        }
    }
}
