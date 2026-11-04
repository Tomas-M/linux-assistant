use std::time::Duration;
use anyhow::Result;
use rusqlite::{params,Connection};
use crate::{App,db::{Db,now}};

#[cfg(test)]
mod tests;

pub const RETENTION:i64=7*86400;
const RESPONSE_RETENTION:i64=86400;
const CACHE_BYTES:i64=256*1024*1024;

/// Keep revision positions but discard pixels and messages already represented by the checkpoint.
pub fn trim_history(messages:&mut [protocol::Message],checkpoint:usize) {
    for (index,message) in messages.iter_mut().enumerate() {
        if index<checkpoint { message.text.clear(); message.attachments=None; continue; }
        for file in message.attachments.as_deref_mut().unwrap_or_default() {
            if let protocol::Attachment::Image { content,.. }=file { content.clear(); }
        }
    }
}

/// Reclaim SQLite pages instead of leaving deleted payloads and a large WAL on disk.
pub fn reclaim(connection:&Connection,compact:bool)->Result<()> {
    connection.execute_batch("PRAGMA journal_size_limit=16777216; PRAGMA wal_checkpoint(TRUNCATE);")?;
    let free:i64=connection.query_row("PRAGMA freelist_count",[],|row|row.get(0))?;
    if compact && free>0 { connection.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?; }
    else if free>0 { connection.execute_batch("PRAGMA incremental_vacuum(8192); PRAGMA wal_checkpoint(TRUNCATE);")?; }
    Ok(())
}

/// The database lock has been acquired and no provider worker exists yet.
pub fn startup(app:&App)->Result<()> {
    app.pool.clean_runtimes()?;
    let config=app.config();
    let parent=config.database.parent().unwrap();
    for entry in std::fs::read_dir(parent)? {
        let entry=entry?; let name=entry.file_name(); let name=name.to_string_lossy();
        let prefix=format!("{}.before-conversation-reset-",config.database.file_name().unwrap().to_string_lossy());
        if entry.file_type()?.is_file() && name.strip_prefix(&prefix).is_some_and(|suffix|!suffix.is_empty() && suffix.bytes().all(|byte|byte.is_ascii_digit())) { std::fs::remove_file(entry.path())?; }
    }
    {
        let connection=app.db.0.lock().unwrap();
        let mut query=connection.prepare("SELECT user_id,id,messages,checkpoint_at FROM conversations")?;
        let rows=query.query_map([],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,i64>(3)? as usize)))?;
        for row in rows {
            let (user,id,history,checkpoint)=row?;
            let mut messages:Vec<protocol::Message>=serde_json::from_str(&history)?;
            trim_history(&mut messages,checkpoint);
            let trimmed=serde_json::to_string(&messages)?;
            if trimmed!=history { connection.execute("UPDATE conversations SET messages=?1 WHERE user_id=?2 AND id=?3",params![trimmed,user,id])?; }
        }
    }
    crate::imagegen::prune(&config)?;
    app.db.prune()?;
    let connection=app.db.0.lock().unwrap();
    let mode:i64=connection.query_row("PRAGMA auto_vacuum",[],|row|row.get(0))?;
    if mode!=2 { connection.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?; }
    reclaim(&connection,true)
}

impl Db {
    /// Keep financial/idempotency rows; expire only replay payloads, old chats and temporary auth data.
    pub fn prune(&self)->Result<()> {
        let mut connection=self.0.lock().unwrap();
        let tx=connection.transaction()?;
        tx.execute_batch("CREATE TEMP TABLE expired_requests(user_id INTEGER,id TEXT,PRIMARY KEY(user_id,id));")?;
        tx.execute("INSERT INTO expired_requests SELECT r.user_id,r.id FROM requests r JOIN billing_tasks t ON t.user_id=r.user_id AND t.id=r.task_id WHERE t.finished_at<?1 AND (r.response IS NOT NULL OR r.error IS NOT NULL) AND (r.response IS NOT NULL OR r.steering!='[]' OR EXISTS(SELECT 1 FROM tool_calls c WHERE c.user_id=r.user_id AND c.request_id=r.id))",[now()-RESPONSE_RETENTION])?;
        tx.execute_batch("DELETE FROM tool_calls WHERE (user_id,request_id) IN (SELECT user_id,id FROM expired_requests);
            UPDATE requests SET response=NULL,error=COALESCE(error,'Response cache expired; start a new chat'),steering='[]' WHERE (user_id,id) IN (SELECT user_id,id FROM expired_requests);
            DROP TABLE expired_requests;")?;
        tx.execute("DELETE FROM quota_history WHERE checked_at<?1",[now()-30*86400])?;
        tx.execute("DELETE FROM sessions WHERE expires_at<=?1",[now()])?;
        tx.execute("DELETE FROM admin_sessions WHERE expires_at<=?1",[now()])?;
        tx.execute("DELETE FROM oauth WHERE expires_at<=?1",[now()])?;
        tx.execute("DELETE FROM devices WHERE expires_at<=?1 AND id NOT IN (SELECT device_id FROM oauth)",[now()])?;
        // Sum logical payload bytes, not database high-water pages, when choosing inactive LRU victims.
        let mut total:i64=tx.query_row("SELECT COALESCE((SELECT SUM(length(CAST(messages AS BLOB))+length(CAST(checkpoint AS BLOB))) FROM conversations),0)+COALESCE((SELECT SUM(COALESCE(length(CAST(response AS BLOB)),0)+length(CAST(steering AS BLOB))) FROM requests),0)+COALESCE((SELECT SUM(length(CAST(interaction AS BLOB))+COALESCE(length(CAST(answer AS BLOB)),0)) FROM tool_calls),0)",[],|row|row.get(0))?;
        let candidates=tx.prepare("SELECT c.user_id,c.id,COALESCE((SELECT MAX(t.finished_at) FROM billing_tasks t WHERE t.user_id=c.user_id AND t.conversation_id=c.id),?1),length(CAST(c.messages AS BLOB))+length(CAST(c.checkpoint AS BLOB))+COALESCE((SELECT SUM(COALESCE(length(CAST(r.response AS BLOB)),0)+length(CAST(r.steering AS BLOB))) FROM requests r WHERE r.user_id=c.user_id AND r.conversation_id=c.id),0)+COALESCE((SELECT SUM(length(CAST(t.interaction AS BLOB))+COALESCE(length(CAST(t.answer AS BLOB)),0)) FROM tool_calls t JOIN requests r ON r.user_id=t.user_id AND r.id=t.request_id WHERE r.user_id=c.user_id AND r.conversation_id=c.id),0) FROM conversations c WHERE NOT EXISTS(SELECT 1 FROM requests r WHERE r.user_id=c.user_id AND r.conversation_id=c.id AND r.response IS NULL AND r.error IS NULL) ORDER BY 3")?.query_map([now()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (user,id,used,bytes) in candidates {
            if used>=now()-RETENTION && total<=CACHE_BYTES { break; }
            tx.execute("DELETE FROM tool_calls WHERE user_id=?1 AND request_id IN (SELECT id FROM requests WHERE user_id=?1 AND conversation_id=?2)",params![user,id])?;
            tx.execute("UPDATE requests SET response=NULL,error=COALESCE(error,'Conversation cache expired; start a new chat'),steering='[]' WHERE user_id=?1 AND conversation_id=?2",params![user,id])?;
            tx.execute("DELETE FROM conversations WHERE user_id=?1 AND id=?2",params![user,id])?;
            total=total.saturating_sub(bytes);
        }
        tx.commit()?;
        reclaim(&connection,false)
    }

    /// Historical usage links the previous native thread to a pending turn even before resume identifies it.
    pub fn retire_thread(&self,thread:&str)->Result<bool> {
        let mut connection=self.0.lock().unwrap();
        let tx=connection.transaction()?;
        let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM requests r JOIN conversations c ON c.user_id=r.user_id AND c.id=r.conversation_id WHERE c.thread_id=?1 AND r.response IS NULL AND r.error IS NULL) OR EXISTS(SELECT 1 FROM usage u WHERE u.thread_id=?1 AND (u.finished_at IS NULL OR EXISTS(SELECT 1 FROM requests r WHERE r.user_id=u.user_id AND r.conversation_id=u.conversation_id AND r.response IS NULL AND r.error IS NULL)))",[thread],|row|row.get(0))?;
        if pending { return Ok(false); }
        tx.execute("UPDATE conversations SET thread_id=NULL WHERE thread_id=?1",[thread])?;
        tx.execute("DELETE FROM provider_counters WHERE thread_id=?1",[thread])?;
        tx.commit()?;
        Ok(true)
    }
}

/// Cache maintenance runs independently of requests and is cancelled before provider shutdown.
pub async fn maintain(app:App) {
    let mut interval=tokio::time::interval(Duration::from_secs(300));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let db=app.db.clone();
        let config=app.config();
        let result=tokio::task::spawn_blocking(move|| { crate::imagegen::prune(&config)?; db.prune() }).await;
        if let Err(error)=result.map_err(anyhow::Error::from).and_then(|result|result) { crate::diagnostics::record("error","storage_cleanup",None,None,&format!("{error:#}")); }
        if let Err(error)=app.pool.prune(&app.db).await { crate::diagnostics::record("error","storage_cleanup",None,None,&format!("{error:#}")); }
    }
}

/// Give interrupted clients five minutes to reclaim their request before closing abandoned tasks.
pub async fn expire_interrupted(app:App,pending:Vec<(i64,String)>) {
    if pending.is_empty() { return; }
    tokio::time::sleep(Duration::from_secs(300)).await;
    let result=tokio::task::spawn_blocking(move||->Result<()> {
        for (user,id) in pending {
            let turns=app.turns.lock().unwrap();
            if turns.contains_key(&(user,uuid::Uuid::parse_str(&id)?)) { continue; }
            app.db.settle(user,&id,Err("The client did not reconnect within 5 minutes after the backend restarted".into()))?;
        }
        Ok(())
    }).await;
    if let Err(error)=result.map_err(anyhow::Error::from).and_then(|result|result) { crate::diagnostics::record("error","restart_cleanup",None,None,&format!("{error:#}")); }
}

/// systemd sends SIGTERM; interactive runs also support Ctrl+C.
pub async fn signal() {
    #[cfg(unix)] {
        let mut terminate=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("Install SIGTERM handler");
        tokio::select! { _=terminate.recv()=>{},_=tokio::signal::ctrl_c()=>{} }
    }
    #[cfg(not(unix))] { let _=tokio::signal::ctrl_c().await; }
}

/// Interrupt paid turns first, drain measured usage, close workers, then checkpoint business data.
pub async fn shutdown(app:&App) {
    crate::diagnostics::record("info","shutdown",None,None,"Stopping admission and draining active AI turns");
    for turn in app.turns.lock().unwrap().values() {
        let (reply,_)=tokio::sync::oneshot::channel();
        let _=turn.sender.try_send(crate::turns::Control::Stop(reply));
    }
    let deadline=tokio::time::Instant::now()+Duration::from_secs(10);
    while !app.turns.lock().unwrap().is_empty() && tokio::time::Instant::now()<deadline { tokio::time::sleep(Duration::from_millis(50)).await; }
    for turn in app.turns.lock().unwrap().values() { turn.cancel.send_replace(true); }
    let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
    while !app.turns.lock().unwrap().is_empty() && tokio::time::Instant::now()<deadline { tokio::time::sleep(Duration::from_millis(50)).await; }
    let closed=app.pool.shutdown().await;
    if let Err(error)=&closed { crate::diagnostics::record("error","shutdown",None,None,&format!("{error:#}")); }
    let db=app.db.clone();
    let drained=app.turns.lock().unwrap().is_empty();
    let result=tokio::task::spawn_blocking(move|| {
        if drained { db.recover()?; }
        let connection=db.0.lock().unwrap();
        reclaim(&connection,false)
    }).await;
    let settled=result.map_err(anyhow::Error::from).and_then(|result|result);
    if let Err(error)=&settled { crate::diagnostics::record("error","shutdown",None,None,&format!("{error:#}")); }
    if drained && closed.is_ok() && settled.is_ok() { crate::diagnostics::record("info","shutdown",None,None,"AI workers closed; recorded usage settled"); }
    else { crate::diagnostics::record("warning","shutdown",None,None,"Shutdown grace period ended with incomplete work; startup recovery will settle any remaining recorded usage"); }
}
