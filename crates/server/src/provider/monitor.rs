use super::*;
use std::sync::Arc;
use rusqlite::params;
use serde_json::json;

impl Pool {
    /// Read quotas in parallel on the shared transports without interrupting active conversations.
    pub async fn monitor(self: Arc<Self>, db: Arc<Db>) {
        let mut interval=tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut tasks=tokio::task::JoinSet::new();
            for index in 0..self.accounts.len() {
                let pool=self.clone();
                let db=db.clone();
                tasks.spawn(async move { pool.probe(&db,index).await });
            }
            while let Some(result)=tasks.join_next().await {
                if !matches!(result,Ok(Ok(()))) { eprintln!("Account monitoring failed"); }
            }
        }
    }

    /// A failed refresh preserves the last successful reading and clearly marks it as stale.
    pub(super) async fn probe(&self, db: &Db, index: usize) -> Result<()> {
        let account=&self.accounts[index];
        let mut cached=account.limits.lock().await;
        if cached.is_some_and(|limits|now()-limits.checked_at<300) { return Ok(()); }
        let result=tokio::time::timeout(Duration::from_secs(20),async {
            let mut rpc=self.rpc(index).await?;
            let identity=rpc.call("account/read",json!({"refreshToken":false})).await?;
            let limits=rpc.call("account/rateLimits/read",json!({})).await?;
            *cached=Some(Limits::read(&limits)?);
            Ok::<_,anyhow::Error>((identity,limits))
        }).await;
        let conn=db.0.lock().unwrap();
        match result {
            Ok(Ok((identity,limits))) => {
                conn.execute("INSERT INTO provider_accounts(home,checked_at,email,plan,limits) VALUES(?1,?2,?3,?4,?5)
                    ON CONFLICT(home) DO UPDATE SET checked_at=excluded.checked_at,email=excluded.email,plan=excluded.plan,limits=excluded.limits,error=NULL",
                    params![account.home,now(),identity["account"]["email"].as_str(),identity["account"]["planType"].as_str(),limits.to_string()])?;
                conn.execute("INSERT INTO quota_history(account_home,checked_at,limits) VALUES(?1,?2,?3)",params![account.home,now(),limits.to_string()])?;
            }
            _ => {
                *cached=None;
                conn.execute("INSERT INTO provider_accounts(home,error) VALUES(?1,?2) ON CONFLICT(home) DO UPDATE SET error=excluded.error",
                    params![account.home,"Unable to refresh; check account sign-in or connectivity"])?;
            }
        }
        Ok(())
    }

    /// Combine stored readings with current worker occupancy; never expose authentication material.
    pub fn account_status(&self, db: &Db) -> Result<Vec<Value>> {
        use rusqlite::OptionalExtension;
        let conn=db.0.lock().unwrap();
        self.accounts.iter().enumerate().map(|(index,account)| {
            let mut row=conn.query_row("SELECT checked_at,email,plan,limits,error FROM provider_accounts WHERE home=?1",[&account.home],|r| {
                Ok(json!({"checked_at":r.get::<_,Option<i64>>(0)?,"email":r.get::<_,Option<String>>(1)?,"plan":r.get::<_,Option<String>>(2)?,
                    "limits":r.get::<_,Option<String>>(3)?,"error":r.get::<_,Option<String>>(4)?}))
            }).optional()?.unwrap_or(json!({"checked_at":null,"limits":null}));
            row["id"]=json!(index);
            row["home"]=json!(account.home);
            let active=account.worker.try_lock().ok().and_then(|worker|worker.as_ref().map(Rpc::active)).unwrap_or(0);
            row["active_requests"]=json!(active);
            row["busy"]=json!(active>0);
            row["limits"]=row["limits"].as_str().map(serde_json::from_str).transpose()?.unwrap_or(Value::Null);
            Ok(row)
        }).collect()
    }

    /// Test only the selected account, with an ephemeral thread, no tools, and no user debit.
    pub async fn test_account(&self, config: &Config, db: &Db, index: usize, prompt: String) -> Result<Value> {
        let account=self.accounts.get(index).context("Unknown account")?;

        let request=protocol::ChatRequest { task_id:uuid::Uuid::new_v4(),base:0,request_id:uuid::Uuid::new_v4(),conversation_id:uuid::Uuid::new_v4(),
            platform:Platform::Linux,messages:vec![protocol::Message { attachments:None, role:Role::User,text:prompt }],commands_enabled:false,cwd:"/".into() };
        let mut attempt=crate::usage::Attempt::start(db,None,&request,&account.home,"admin_test")?;
        let result=tokio::time::timeout(Duration::from_secs(180),async {
            let mut rpc=self.rpc(index).await?;
            let (reply,thread,_)=rpc.answer(config,&request,None,&Value::Null,false,None,None,&attempt,&mut super::progress::Reporter::new(None,1)).await?;
            rpc.call("thread/unsubscribe",json!({"threadId":thread})).await?;
            Ok::<_,anyhow::Error>(reply)
        }).await;
        let result=result.context("Test timed out").and_then(|result|result);
        attempt.finish(result.as_ref().err().map(|_|"Account test failed; check sign-in, quota or connectivity"))?;
        match result {
            Ok(reply)=>Ok(json!({"text":reply.text,"usage_id":attempt.id})),
            Err(_)=>anyhow::bail!("Account test failed; check sign-in, quota or connectivity"),
        }
    }
}
