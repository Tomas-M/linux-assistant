use anyhow::{ensure, Result};
use clap::Args;
use rusqlite::{params, Connection, OptionalExtension};
use protocol::{ChatRequest, Role};
use crate::db::now;

const SCALE: i64=1_000_000;
const DENOMINATOR: i128=1_000_000_000_000;

#[derive(Clone, Debug, Args)]
pub struct Pricing {
    #[arg(long, env="ASSISTANT_INPUT_CREDITS_PER_MILLION", default_value="200")]
    pub input_credits_per_million: Rate,
    #[arg(long, env="ASSISTANT_CACHED_INPUT_CREDITS_PER_MILLION", default_value="10")]
    pub cached_input_credits_per_million: Rate,
    #[arg(long, env="ASSISTANT_OUTPUT_CREDITS_PER_MILLION", default_value="1000")]
    pub output_credits_per_million: Rate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate(pub i64);

impl std::str::FromStr for Rate {
    type Err=anyhow::Error;
    /// Parse decimal credits into fixed-point units so rounding never depends on floating-point math.
    fn from_str(value:&str)->Result<Self> {
        let (whole,fraction)=value.split_once('.').unwrap_or((value,""));
        ensure!(!whole.is_empty() && whole.bytes().all(|b|b.is_ascii_digit()),"Rate must be a nonnegative decimal");
        ensure!(fraction.len()<=6 && fraction.bytes().all(|b|b.is_ascii_digit()),"Rate supports up to six decimal places");
        let units=whole.parse::<i64>()?.checked_mul(SCALE).and_then(|whole|whole.checked_add(format!("{fraction:0<6}").parse::<i64>().ok()?));
        Ok(Self(units.ok_or_else(||anyhow::anyhow!("Rate is too large"))?))
    }
}

impl Pricing {
    /// Cached tokens are a subset of input; output already includes reasoning tokens.
    fn cost(&self,input:i64,cached:i64,output:i64)->Result<i128> {
        ensure!(input>=0 && cached>=0 && cached<=input && output>=0,"Invalid token counts");
        Ok(i128::from(input-cached)*i128::from(self.input_credits_per_million.0)
            +i128::from(cached)*i128::from(self.cached_input_credits_per_million.0)
            +i128::from(output)*i128::from(self.output_credits_per_million.0))
    }
}

/// Round the combined token cost up once; successful tasks cost at least one credit.
fn credits(cost:i128)->Result<i64> { Ok(i64::try_from(((cost+DENOMINATOR-1)/DENOMINATOR).max(1))?) }

/// Admit new tasks using the balance, while validated command follow-ups retain their original rates.
pub fn admit(conn:&Connection,user:i64,request:&ChatRequest,pricing:&Pricing)->Result<bool> {
    let task=request.task_id.to_string();
    let conversation=request.conversation_id.to_string();
    let existing:Option<(String,Option<i64>)>=conn.query_row("SELECT conversation_id,finished_at FROM billing_tasks WHERE user_id=?1 AND id=?2",params![user,task],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((stored,finished))=existing {
        ensure!(stored==conversation && finished.is_none(),"Task is already finished or belongs to another conversation");
        ensure!(request.messages.iter().all(|message|message.role==Role::Tool),"Only command results may continue an existing task");
        let response:Option<String>=conn.query_row("SELECT response FROM requests WHERE user_id=?1 AND task_id=?2 ORDER BY rowid DESC LIMIT 1",params![user,task],|r|r.get(0))?;
        let completed:crate::db::Completed=serde_json::from_str(response.as_deref().ok_or_else(||anyhow::anyhow!("Previous request has not completed"))?)?;
        ensure!(completed.reply.command.is_some(),"Task has no command awaiting a result");
        return Ok(true);
    }
    ensure!(request.task_id==request.request_id,"A new task must start with a new request ID");
    let balance:i64=conn.query_row("SELECT COALESCE(SUM(delta),0) FROM ledger WHERE user_id=?1",[user],|r|r.get(0))?;
    if balance<=0 { return Ok(false); }
    conn.execute("UPDATE billing_tasks SET finished_at=?1 WHERE user_id=?2 AND conversation_id=?3 AND finished_at IS NULL",params![now(),user,conversation])?;
    conn.execute("INSERT INTO billing_tasks(user_id,id,conversation_id,input_rate,cached_rate,output_rate) VALUES(?1,?2,?3,?4,?5,?6)",
        params![user,task,conversation,pricing.input_credits_per_million.0,pricing.cached_input_credits_per_million.0,pricing.output_credits_per_million.0])?;
    Ok(true)
}

/// Settle recorded usage atomically with delivery; incremental debits sum to one rounded task cost.
pub fn settle(conn:&Connection,user:i64,request:&str,success:bool,finished:bool)->Result<()> {
    let (task,pricing)=conn.query_row("SELECT t.id,t.input_rate,t.cached_rate,t.output_rate FROM billing_tasks t JOIN requests r ON r.user_id=t.user_id AND r.task_id=t.id WHERE r.user_id=?1 AND r.id=?2",params![user,request],|r| {
        Ok((r.get::<_,String>(0)?,Pricing { input_credits_per_million:Rate(r.get(1)?),cached_input_credits_per_million:Rate(r.get(2)?),output_credits_per_million:Rate(r.get(3)?) }))
    })?;
    let mut statement=conn.prepare("SELECT x.id,x.input_tokens,x.cached_tokens,x.output_tokens FROM usage x JOIN requests r ON r.user_id=x.user_id AND r.id=x.request_id WHERE r.user_id=?1 AND r.task_id=?2 ORDER BY x.id")?;
    let attempts=statement.query_map(params![user,task],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,Option<i64>>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let minimum=format!("task:{user}:{task}");
    let mut charged:i64=conn.query_row("SELECT COALESCE(-SUM(delta),0) FROM ledger WHERE user_id=?1 AND (reference=?3 OR reference IN (SELECT 'usage:'||x.id FROM usage x JOIN requests r ON r.user_id=x.user_id AND r.id=x.request_id WHERE r.user_id=?1 AND r.task_id=?2))",params![user,task,minimum],|r|r.get(0))?;
    let mut cost=0i128;
    for (index,(id,input,cached,output)) in attempts.iter().enumerate() {
        if let (Some(input),Some(cached),Some(output))=(input,cached,output) {
            cost=cost.checked_add(pricing.cost(*input,*cached,*output)?).ok_or_else(||anyhow::anyhow!("Token cost overflow"))?;
        }
        let minimum_due=success && index+1==attempts.len();
        let target=if cost>0 || minimum_due { credits(cost)? } else { 0 };
        if target>charged {
            conn.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,?2,?3,?4)",params![user,charged-target,format!("usage:{id}"),now()])?;
            charged=target;
        }
    }
    if success && charged==0 {
        conn.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,-1,?2,?3)",params![user,minimum,now()])?;
    }
    if finished { conn.execute("UPDATE billing_tasks SET finished_at=?1 WHERE user_id=?2 AND id=?3",params![now(),user,task])?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use protocol::{AssistantReply, CommandProposal, Message, Platform};
    use uuid::Uuid;
    use crate::db::{Db,Reservation};

    /// Give each fixture prompt the same task/request identity used by the client.
    fn request()->ChatRequest {
        let id=Uuid::new_v4();
        ChatRequest { task_id:id,request_id:id,conversation_id:Uuid::new_v4(),base:0,platform:Platform::Linux,
            messages:vec![Message { role:Role::User,text:"Inspect my computer".into() }],commands_enabled:true,cwd:"/tmp".into() }
    }

    /// Feed actual usage notifications twice to verify that duplicate snapshots cannot double-charge.
    fn measured(db:&Db,user:i64,request:&ChatRequest,input:i64,cached:i64,output:i64)->Result<()> {
        let mut attempt=crate::usage::Attempt::start(db,Some(user),request,"fixture-account","conversation")?;
        let baseline=serde_json::json!({"inputTokens":100,"cachedInputTokens":10,"outputTokens":20,"reasoningOutputTokens":10,"totalTokens":120});
        let total=serde_json::json!({"inputTokens":100+input,"cachedInputTokens":10+cached,"outputTokens":20+output,"reasoningOutputTokens":10+output/2,"totalTokens":120+input+output});
        for _ in 0..2 { attempt.tokens(&total,&baseline)?; }
        attempt.finish(None)
    }
    /// Check exact boundaries, cached-input subtraction, fractional rates and large counts.
    #[test]
    fn token_prices_use_exact_whole_credit_rounding()->Result<()> {
        let rates=Pricing { input_credits_per_million:"200".parse()?,cached_input_credits_per_million:"10".parse()?,output_credits_per_million:"1000".parse()? };
        assert_eq!(credits(rates.cost(1_000_000,0,0)?)?,200);
        assert_eq!(credits(rates.cost(1_000_000,1_000_000,0)?)?,10);
        assert_eq!(credits(rates.cost(1_000_000,500_000,100_000)?)?,205);
        assert_eq!(credits(rates.cost(5000,0,0)?)?,1);
        assert_eq!(credits(rates.cost(5001,0,0)?)?,2);
        assert_eq!(credits(rates.cost(0,0,0)?)?,1);
        assert_eq!("0.000001".parse::<Rate>()?,Rate(1));
        assert_eq!("0.1".parse::<Rate>()?,Rate(100_000));
        for value in ["-1","NaN","inf","1e2","1.0000001","9223372036854775807"] { assert!(value.parse::<Rate>().is_err()); }
        assert!(rates.cost(1,2,0).is_err());
        assert!(credits(rates.cost(i64::MAX,0,i64::MAX)?).is_ok());
        Ok(())
    }

    /// A negative balance blocks new prompts, while the admitted command chain shares one rounded bill.
    #[test]
    fn command_chain_survives_overdraft_restart_and_rate_changes()->Result<()> {
        let directory=tempfile::tempdir()?;
        let path=directory.path().join("billing.db");
        let db=Db::open(&path)?;
        let pricing=crate::config::Config::parse_from(["server"]).pricing;
        let user=db.user("chain@example.test",None,1)?;
        let mut input=request();
        let command=AssistantReply { text:"Inspect a file".into(),command:Some(CommandProposal { command:"cat /etc/os-release".into(),cwd:"/tmp".into(),reason:"Identify OS".into() }) };
        assert!(matches!(db.reserve(user,&input,"one",&pricing)?,Reservation::New));
        assert_eq!(db.balance(user)?,1);
        measured(&db,user,&input,6000,0,0)?;
        input.base=db.complete(user,&input,&command,None,None,None)?.revision;
        assert_eq!(db.balance(user)?,-1);
        assert!(matches!(db.reserve(user,&input,"one",&pricing)?,Reservation::Cached(_)));
        assert!(matches!(db.reserve(user,&request(),"blocked",&pricing)?,Reservation::Insufficient));
        drop(db);
        let db=Db::open(&path)?;
        db.recover()?;
        let changed=crate::config::Config::parse_from(["server","--input-credits-per-million","1000"]).pricing;
        let mut stale=request();
        stale.conversation_id=input.conversation_id;
        assert!(matches!(db.reserve(user,&stale,"stale",&changed)?,Reservation::Stale));
        input.request_id=Uuid::new_v4();
        assert!(db.reserve(user,&input,"not-a-tool",&changed).is_err());
        input.messages=vec![Message { role:Role::Tool,text:"Linux".into() }];
        assert!(matches!(db.reserve(user,&input,"two",&changed)?,Reservation::New));
        measured(&db,user,&input,3000,0,0)?;
        input.base=db.complete(user,&input,&command,None,None,None)?.revision;
        assert_eq!(db.balance(user)?,-1);
        input.request_id=Uuid::new_v4();
        assert!(matches!(db.reserve(user,&input,"three",&changed)?,Reservation::New));
        measured(&db,user,&input,0,0,1000)?;
        input.base=db.complete(user,&input,&AssistantReply { text:"Done".into(),command:None },None,None,None)?.revision;
        assert_eq!(db.balance(user)?,-2);
        assert!(matches!(db.reserve(user,&input,"three",&changed)?,Reservation::Cached(_)));
        input.request_id=Uuid::new_v4();
        assert!(db.reserve(user,&input,"closed",&changed).is_err());
        db.0.lock().unwrap().execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,100,'purchase:test',?2)",params![user,now()])?;
        let next=request();
        db.reserve(user,&next,"new-price",&changed)?;
        measured(&db,user,&next,3000,0,0)?;
        db.settle(user,&next.request_id.to_string(),Ok("reply".into()))?;
        assert_eq!(db.balance(user)?,95);
        Ok(())
    }

    /// Failover, interruption and restart bill measured work once; missing counters are never estimated.
    #[test]
    fn failures_and_recovery_preserve_auditable_usage()->Result<()> {
        let db=Db::open(std::path::Path::new(":memory:"))?;
        let pricing=crate::config::Config::parse_from(["server"]).pricing;
        let user=db.user("failure@example.test",None,10)?;
        let input=request();
        db.reserve(user,&input,"failed",&pricing)?;
        measured(&db,user,&input,4000,0,0)?;
        measured(&db,user,&input,2000,0,0)?;
        for _ in 0..2 { db.settle(user,&input.request_id.to_string(),Err("Interrupted".into()))?; }
        assert_eq!(db.balance(user)?,8);
        assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM ledger WHERE reference LIKE 'usage:%'",[],|r|r.get::<_,i64>(0))?,2);
        let interrupted=request();
        db.reserve(user,&interrupted,"restart",&pricing)?;
        measured(&db,user,&interrupted,1_000_000,1_000_000,0)?;
        db.recover()?;
        db.recover()?;
        assert_eq!(db.balance(user)?,-2);
        assert!(matches!(db.reserve(user,&interrupted,"restart",&pricing)?,Reservation::Failed(_)));
        let free=db.user("unknown@example.test",None,3)?;
        let missing=request();
        db.reserve(free,&missing,"unknown",&pricing)?;
        let mut attempt=crate::usage::Attempt::start(&db,Some(free),&missing,"fixture","conversation")?;
        attempt.finish(None)?;
        db.settle(free,&missing.request_id.to_string(),Ok("reply".into()))?;
        assert_eq!(db.balance(free)?,2);
        let unavailable=request();
        db.reserve(free,&unavailable,"no-provider",&pricing)?;
        db.settle(free,&unavailable.request_id.to_string(),Err("Unavailable".into()))?;
        assert_eq!(db.balance(free)?,2);
        Ok(())
    }

    /// Already admitted independent tasks finish; zero balance and cross-user continuations cannot bypass admission.
    #[test]
    fn admission_is_user_scoped_and_requires_positive_balance()->Result<()> {
        let db=Db::open(std::path::Path::new(":memory:"))?;
        let config=crate::config::Config::parse_from(["server"]);
        assert_eq!(config.free_credits,100);
        let user=db.user("parallel@example.test",None,1)?;
        let zero=db.user("zero@example.test",None,0)?;
        let first=request();
        let second=request();
        for task in [&first,&second] { assert!(matches!(db.reserve(user,task,"parallel",&config.pricing)?,Reservation::New)); }
        for task in [&first,&second] {
            measured(&db,user,task,10_000,0,0)?;
            db.settle(user,&task.request_id.to_string(),Ok("reply".into()))?;
        }
        assert_eq!(db.balance(user)?,-3);
        assert!(matches!(db.reserve(zero,&request(),"zero",&config.pricing)?,Reservation::Insufficient));
        let mut stolen=first;
        stolen.request_id=Uuid::new_v4();
        stolen.messages=vec![Message { role:Role::Tool,text:"fake output".into() }];
        assert!(db.reserve(zero,&stolen,"stolen",&config.pricing).is_err());
        Ok(())
    }
}
