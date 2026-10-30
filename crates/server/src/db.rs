use std::{path::Path, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use protocol::{AssistantReply, ChatRequest, Message, Role, Steering};

pub struct Db(pub Mutex<Connection>);
#[derive(Default)]
pub struct Conversation { pub account_home: Option<String>, pub thread_id: Option<String>, pub messages: Vec<Message>, pub checkpoint: String, pub checkpoint_at: usize }

#[derive(serde::Serialize,serde::Deserialize)]
pub struct Completed { pub reply: AssistantReply, pub revision: usize, pub steering: Vec<Steering> }

/// Use Unix timestamps consistently for sessions, quotas, and payment verification.
pub fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 }

/// Generate two UUIDs so public identifiers and authentication secrets remain distinct.
pub fn secret() -> String { format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()) }

/// Persist only hashes of bearer tokens and email verification codes.
pub fn hash(value: &str) -> String { hex::encode(Sha256::digest(value.as_bytes())) }

impl Db {
    /// Create the initial schema; early development databases can be recreated.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(include_str!("schema.sql")).context("Could not open the current schema. For an older development database, stop the backend and run scripts/reset_conversations.sh before updating")?;
        Ok(Self(Mutex::new(conn)))
    }

    /// A verified identity receives a welcome grant exactly once.
    pub fn user(&self, email: &str, google_sub: Option<&str>, free: i64) -> Result<i64> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        if let Some(sub) = google_sub {
            if let Some(id) = tx.query_row("SELECT id FROM users WHERE google_sub=?1", [sub], |r| r.get(0)).optional()? {
                return Ok(id);
            }
        }
        tx.execute("INSERT OR IGNORE INTO users(email) VALUES(?1)", [email])?;
        let id: i64 = tx.query_row("SELECT id FROM users WHERE email=?1", [email], |r| r.get(0))?;
        if let Some(sub) = google_sub {
            let changed = tx.execute("UPDATE users SET google_sub=?1 WHERE id=?2 AND (google_sub IS NULL OR google_sub=?1)", params![sub,id])?;
            ensure!(changed == 1, "Google identity is already linked to another account");
        }
        tx.execute("INSERT OR IGNORE INTO ledger(user_id,delta,reference,created_at) VALUES(?1,?2,?3,?4)", params![id,free,format!("welcome:{id}"),now()])?;
        tx.commit()?;
        Ok(id)
    }

    /// Derive the balance from the ledger instead of maintaining duplicate state.
    pub fn balance(&self, user: i64) -> Result<i64> {
        Ok(self.0.lock().unwrap().query_row("SELECT COALESCE(SUM(delta),0) FROM ledger WHERE user_id=?1", [user], |r| r.get(0))?)
    }

    /// Resolve an unexpired session without exposing the raw token in storage.
    pub fn authenticate(&self, token: &str) -> Result<i64> {
        Ok(self.0.lock().unwrap().query_row("SELECT user_id FROM sessions WHERE token_hash=?1 AND expires_at>?2 AND user_id NOT IN (SELECT user_id FROM user_blocks)", params![hash(token),now()], |r| r.get(0))?)
    }

    /// Admit a task and reserve its request ID without charging before token usage is known.
    pub fn reserve(&self, user: i64, request: &ChatRequest, fingerprint: &str, pricing: &crate::billing::Pricing) -> Result<Reservation> {
        let id=request.request_id.to_string();
        let conversation=request.conversation_id.to_string();
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: Option<(String, Option<String>, Option<String>)> = tx.query_row(
            "SELECT fingerprint,response,error FROM requests WHERE user_id=?1 AND id=?2", params![user,id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((stored, response, error)) = existing {
            ensure!(stored == fingerprint, "Request ID was reused with different content");
            return Ok(match (response,error) {
                (Some(value),_) => Reservation::Cached(value),
                (_,Some(error)) => Reservation::Failed(error),
                _ => Reservation::Pending,
            });
        }
        let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM requests WHERE user_id=?1 AND conversation_id=?2 AND response IS NULL AND error IS NULL)",params![user,conversation],|r|r.get(0))?;
        if pending { return Ok(Reservation::Pending); }
        let history:Option<String>=tx.query_row("SELECT messages FROM conversations WHERE user_id=?1 AND id=?2",params![user,conversation],|r|r.get(0)).optional()?;
        let revision=history.map(|value|serde_json::from_str::<Vec<Message>>(&value)).transpose()?.map_or(0,|messages|messages.len());
        if request.base!=revision { return Ok(Reservation::Stale); }
        if !crate::billing::admit(&tx,user,request,pricing)? { return Ok(Reservation::Insufficient); }
        tx.execute("INSERT INTO requests(user_id,id,task_id,conversation_id,fingerprint) VALUES(?1,?2,?3,?4,?5)", params![user,id,request.task_id.to_string(),conversation,fingerprint])?;
        tx.commit()?;
        Ok(Reservation::New)
    }

    /// Namespace conversations by application user; provider thread IDs never come from clients.
    pub fn conversation(&self, user: i64, id: Uuid) -> Result<Conversation> {
        let row: Option<(Option<String>,Option<String>,String,String,usize)> = self.0.lock().unwrap().query_row(
            "SELECT account_home,thread_id,messages,checkpoint,checkpoint_at FROM conversations WHERE user_id=?1 AND id=?2",
            params![user,id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get::<_,i64>(4)? as usize))).optional()?;
        match row {
            Some((account_home,thread_id,messages,checkpoint,checkpoint_at)) => Ok(Conversation { account_home,thread_id,messages:serde_json::from_str(&messages)?,checkpoint,checkpoint_at }),
            None => Ok(Conversation::default()),
        }
    }

    /// Persist affinity before generation and invalidate any thread with an unconfirmed next turn.
    pub fn bind_conversation(&self, user: i64, id: Uuid, home: &str) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO conversations(user_id,id,account_home) VALUES(?1,?2,?3)
            ON CONFLICT(user_id,id) DO UPDATE SET account_home=excluded.account_home,thread_id=NULL",params![user,id.to_string(),home])?;
        Ok(())
    }

    /// Break equally available account ties using existing assignments instead of another counter.
    pub fn assigned_conversations(&self, home: &str) -> Result<i64> {
        Ok(self.0.lock().unwrap().query_row("SELECT COUNT(*) FROM conversations WHERE account_home=?1",[home],|r|r.get(0))?)
    }

    /// Commit delivery, thread position and application history together, including after restarts.
    pub fn complete(&self, user: i64, request: &ChatRequest, reply: &AssistantReply, home: Option<&str>, thread: Option<&str>, checkpoint: Option<&str>) -> Result<Completed> {
        let session=self.conversation(user,request.conversation_id)?;
        ensure!(request.base==session.messages.len(),"Conversation revision changed");
        let steering=self.steering(user,request.request_id)?;
        let mut messages=session.messages;
        messages.extend(request.messages.clone());
        messages.extend(steering.iter().map(|input|Message { attachments:input.attachments.clone(), role:Role::User,text:input.text.clone() }));
        messages.push(Message { attachments:None, role:Role::Assistant,text:reply.text.clone() });
        let result=Completed { reply:reply.clone(),revision:messages.len(),steering };
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let changed = tx.execute("UPDATE requests SET response=?1 WHERE user_id=?2 AND id=?3 AND response IS NULL AND error IS NULL",
            params![serde_json::to_string(&result)?,user,request.request_id.to_string()])?;
        ensure!(changed==1,"Request was already settled");
        crate::billing::settle(&tx,user,&request.request_id.to_string(),true,reply.command.is_none())?;
        tx.execute("INSERT INTO conversations(user_id,id,account_home,thread_id,messages,checkpoint,checkpoint_at) VALUES(?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(user_id,id) DO UPDATE SET account_home=excluded.account_home,thread_id=excluded.thread_id,messages=excluded.messages,checkpoint=excluded.checkpoint,checkpoint_at=excluded.checkpoint_at",
            params![user,request.conversation_id.to_string(),home,thread,serde_json::to_string(&messages)?,checkpoint.unwrap_or(&session.checkpoint),(if checkpoint.is_some() { messages.len() } else { session.checkpoint_at }) as i64])?;
        tx.commit()?;
        Ok(result)
    }

    /// Accepted steering is durable and idempotent even when its HTTP acknowledgement is lost.
    pub fn steering(&self, user: i64, request: Uuid) -> Result<Vec<Steering>> {
        let value:Option<String>=self.0.lock().unwrap().query_row("SELECT steering FROM requests WHERE user_id=?1 AND id=?2",params![user,request.to_string()],|r|r.get(0)).optional()?;
        Ok(value.map(|value|serde_json::from_str(&value)).transpose()?.unwrap_or_default())
    }

    /// Append only after Codex accepted the message; never let a client address a provider thread directly.
    pub fn accept_steering(&self, user: i64, request: Uuid, input: &Steering) -> Result<()> {
        let mut inputs=self.steering(user,request)?;
        if let Some(previous)=inputs.iter().find(|previous|previous.id==input.id) {
            ensure!(previous==input,"Steering ID reused with different text");
            return Ok(());
        }
        inputs.push(input.clone());
        self.0.lock().unwrap().execute("UPDATE requests SET steering=?1 WHERE user_id=?2 AND id=?3",params![serde_json::to_string(&inputs)?,user,request.to_string()])?;
        Ok(())
    }

    /// Settle failures once, charging only measured usage; requests that never ran cost nothing.
    pub fn settle(&self, user: i64, id: &str, result: Result<String, String>) -> Result<()> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let success=result.is_ok();
        let changed=match result {
            Ok(response)=>tx.execute("UPDATE requests SET response=?1 WHERE user_id=?2 AND id=?3 AND response IS NULL AND error IS NULL",params![response,user,id])?,
            Err(error)=>tx.execute("UPDATE requests SET error=?1 WHERE user_id=?2 AND id=?3 AND response IS NULL AND error IS NULL",params![error,user,id])?,
        };
        if changed==1 { crate::billing::settle(&tx,user,id,success,true)?; }
        tx.commit()?;
        Ok(())
    }

    /// Recover interrupted reservations after acquiring the exclusive backend lock.
    pub fn recover(&self) -> Result<()> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("UPDATE usage SET finished_at=?1,error='Backend restarted before completion; token counts may be partial' WHERE finished_at IS NULL",[now()])?;
        let pending=tx.prepare("SELECT user_id,id FROM requests WHERE response IS NULL AND error IS NULL")?.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (user,id) in pending { crate::billing::settle(&tx,user,&id,false,true)?; }
        tx.execute("UPDATE requests SET error='Server restarted; recorded usage was settled. Retry.' WHERE response IS NULL AND error IS NULL", [])?;
        tx.commit()?;
        Ok(())
    }
}

pub enum Reservation { New, Cached(String), Failed(String), Pending, Insufficient, Stale }

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Start an independent task using the production request contract.
    fn request()->ChatRequest {
        let id=Uuid::new_v4();
        ChatRequest { task_id:id,base:0,request_id:id,conversation_id:Uuid::new_v4(),platform:protocol::Platform::Linux,
            messages:vec![Message { attachments:None, role:Role::User,text:"Hello".into() }],commands_enabled:false,cwd:"/tmp".into() }
    }

    /// An interrupted turn keeps account affinity and committed history but cannot resume an advanced thread.
    #[test]
    fn interrupted_conversation_recovers_without_reusing_unconfirmed_thread() -> Result<()> {
        let directory=tempfile::tempdir()?;
        let path=directory.path().join("assistant.db");
        let db=Db::open(&path)?;
        let user=db.user("restart@example.test",None,3)?;
        let mut request=request();
        let pricing=crate::config::Config::parse_from(["server"]).pricing;
        db.reserve(user,&request,"first",&pricing)?;
        db.bind_conversation(user,request.conversation_id,"account-a")?;
        db.complete(user,&request,&AssistantReply { text:"Hi".into(),command:None },Some("account-a"),Some("thread-a"),None)?;
        let committed=db.conversation(user,request.conversation_id)?.messages;
        request.request_id=Uuid::new_v4();
        request.task_id=request.request_id;
        request.base=committed.len();
        db.reserve(user,&request,"next",&pricing)?;
        db.bind_conversation(user,request.conversation_id,"account-a")?;
        drop(db);
        let db=Db::open(&path)?;
        db.recover()?;
        let session=db.conversation(user,request.conversation_id)?;
        assert_eq!(session.account_home.as_deref(),Some("account-a"));
        assert!(session.thread_id.is_none());
        assert_eq!(session.messages,committed);
        assert_eq!(db.balance(user)?,2);
        Ok(())
    }

    /// Grants and settlement are idempotent; admission never reserves or invents a credit.
    #[test]
    fn ledger_is_idempotent() -> Result<()> {
        let db=Db::open(Path::new(":memory:"))?;
        let pricing=crate::config::Config::parse_from(["server"]).pricing;
        let user=db.user("person@example.com",None,2)?;
        assert_eq!(db.user("person@example.com",None,100)?,user);
        let a=request();
        assert!(matches!(db.reserve(user,&a,"one",&pricing)?,Reservation::New));
        assert!(matches!(db.reserve(user,&a,"one",&pricing)?,Reservation::Pending));
        assert!(db.reserve(user,&a,"changed",&pricing).is_err());
        let mut parallel=request();
        parallel.conversation_id=a.conversation_id;
        assert!(matches!(db.reserve(user,&parallel,"other",&pricing)?,Reservation::Pending));
        assert_eq!(db.balance(user)?,2);
        for _ in 0..2 { db.settle(user,&a.request_id.to_string(),Err("failed".into()))?; }
        assert_eq!(db.balance(user)?,2);
        let b=request();
        assert!(matches!(db.reserve(user,&b,"two",&pricing)?,Reservation::New));
        for _ in 0..2 { db.settle(user,&b.request_id.to_string(),Ok("reply".into()))?; }
        assert!(matches!(db.reserve(user,&b,"two",&pricing)?,Reservation::Cached(_)));
        assert_eq!(db.balance(user)?,1);
        let c=request();
        assert!(matches!(db.reserve(user,&c,"three",&pricing)?,Reservation::New));
        db.recover()?;
        db.recover()?;
        assert_eq!(db.balance(user)?,1);
        Ok(())
    }
}
