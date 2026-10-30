use std::{path::Path,sync::{LazyLock,Mutex,OnceLock}};
use anyhow::{Context,Result};
use axum::{extract::Query,Json};
use regex::Regex;
use rusqlite::{params,Connection};
use serde::Deserialize;
use serde_json::{json,Value};
use crate::{config::Config,db::now,ApiError,ApiResult};

static LOG:OnceLock<Log>=OnceLock::new();
const MAX_ENTRIES:i64=10_000;
const RETENTION:i64=30*86400;
struct Log { connection:Mutex<Connection>,secrets:Vec<String> }

/// Keep diagnostics independent of the application database, including errors while its lock is held.
pub fn init(config:&Config)->Result<()> {
    let secrets=[&config.admin_password,&config.smtp_password,&config.google_client_secret,&config.stripe_secret,&config.stripe_webhook_secret]
        .into_iter().flatten().filter(|value|!value.is_empty()).cloned().collect();
    LOG.set(Log::open(&config.database.with_extension("logs.db"),secrets)?).map_err(|_|anyhow::anyhow!("Diagnostics already initialized"))
}

/// Redact known configuration secrets and common credential formats before storing or printing errors.
pub fn clean(message:&str)->String {
    let mut text=message.to_owned();
    if let Some(log)=LOG.get() { for secret in &log.secrets { text=text.replace(secret,"[redacted]"); } }
    static PATTERNS:LazyLock<Vec<Regex>>=LazyLock::new(||[
        r"(?i)\b(?:bearer|basic)\s+[a-z0-9._~+/=-]+",
        r"\b(?:sk-[A-Za-z0-9_-]{10,}|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]+|whsec_[A-Za-z0-9]+|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)",
        r"(?im)\b(?:set-cookie|cookie)\s*:\s*[^\r\n]+",
        r#"(?i)["']?\b(?:[a-z0-9_-]*(?:password|secret|token|api[_-]?key)|authorization|cookie|set-cookie|code_verifier)["']?\s*[:=]\s*(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s&,;]+)"#,
        r"(?i)https?://[^/\s@]+@",
    ].into_iter().map(|pattern|Regex::new(pattern).unwrap()).collect());
    for pattern in PATTERNS.iter() { text=pattern.replace_all(&text,"[redacted]").into_owned(); }
    text.retain(|ch|!ch.is_control() || ch=='\n' || ch=='\t');
    if text.len()>8192 {
        let mut end=8192;
        while !text.is_char_boundary(end) { end-=1; }
        text.truncate(end); text.push_str("\n[Diagnostic truncated]");
    }
    text
}

/// Record bounded, structured diagnostics; never pass request bodies, prompts, or attachment contents.
pub fn record(level:&str,stage:&str,request:Option<&str>,account:Option<&str>,message:&str) {
    let message=clean(message);
    eprintln!("{}",json!({"level":level,"stage":stage,"request_id":request,"account":account,"message":message}));
    if let Some(log)=LOG.get() {
        if let Err(error)=log.insert(level,stage,request,account,&message) { eprintln!("Could not save diagnostic: {}",clean(&format!("{error:#}"))); }
    }
}

impl Log {
    /// Create a private SQLite sidecar with bounded retention and no dependency on conversation schema.
    fn open(path:&Path,secrets:Vec<String>)->Result<Self> {
        #[cfg(unix)] {
            use std::os::unix::fs::{OpenOptionsExt,PermissionsExt};
            if path!=Path::new(":memory:") {
                std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
                std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600))?;
            }
        }
        let connection=Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS diagnostics(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,level TEXT NOT NULL,stage TEXT NOT NULL,request_id TEXT,account TEXT,message TEXT NOT NULL); CREATE INDEX IF NOT EXISTS diagnostics_time ON diagnostics(created_at);")?;
        connection.execute("DELETE FROM diagnostics WHERE created_at<?1",[now()-RETENTION])?;
        Ok(Self { connection:Mutex::new(connection),secrets })
    }

    /// Bound both history length and age; logging errors do not change request settlement.
    fn insert(&self,level:&str,stage:&str,request:Option<&str>,account:Option<&str>,message:&str)->Result<()> {
        let mut connection=self.connection.lock().unwrap();
        let transaction=connection.transaction()?;
        transaction.execute("INSERT INTO diagnostics(created_at,level,stage,request_id,account,message) VALUES(?1,?2,?3,?4,?5,?6)",params![now(),level,stage,request,account,message])?;
        transaction.execute("DELETE FROM diagnostics WHERE id<=?1 OR created_at<?2",params![transaction.last_insert_rowid()-MAX_ENTRIES,now()-RETENTION])?;
        transaction.commit()?;
        Ok(())
    }

    /// Use an ID cursor so incoming events cannot shift or duplicate older pages.
    fn list(&self,filter:&Filter)->Result<Value> {
        let connection=self.connection.lock().unwrap();
        let mut statement=connection.prepare("SELECT id,created_at,level,stage,request_id,account,message FROM diagnostics WHERE created_at>=?1 AND (?2 IS NULL OR id<?2) AND (?3 IS NULL OR level=?3) AND instr(lower(stage||' '||COALESCE(request_id,'')||' '||COALESCE(account,'')||' '||message),lower(?4))>0 ORDER BY id DESC LIMIT 101")?;
        let mut entries=statement.query_map(params![now()-RETENTION,filter.before,filter.level,filter.search],|row|Ok(json!({
            "id":row.get::<_,i64>(0)?,"created_at":row.get::<_,i64>(1)?,"level":row.get::<_,String>(2)?,"stage":row.get::<_,String>(3)?,
            "request_id":row.get::<_,Option<String>>(4)?,"account":row.get::<_,Option<String>>(5)?,"message":row.get::<_,String>(6)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more=entries.len()>100; entries.truncate(100);
        Ok(json!({"next_before":if more { entries.last().map(|entry|entry["id"].clone()) } else { None },"entries":entries}))
    }
}

#[derive(Default,Deserialize)]
pub struct Filter { before:Option<i64>,level:Option<String>,#[serde(default)] search:String }

/// This handler is mounted only behind the existing operator session authentication.
pub async fn list(Query(filter):Query<Filter>)->ApiResult<Json<Value>> {
    let valid_level=filter.level.as_deref().is_none_or(|level|matches!(level,"info"|"warning"|"error"));
    if !valid_level || filter.search.len()>320 || filter.before.is_some_and(|id|id<=0) { return Err(ApiError::bad("Invalid diagnostic filter")); }
    Ok(Json(LOG.get().context("Diagnostics not initialized")?.list(&filter)?))
}

#[cfg(test)]
/// Give parallel HTTP tests one isolated diagnostic sink without touching a real server log.
pub fn init_test() { LOG.get_or_init(||Log::open(Path::new(":memory:"),vec!["local-test-password-only".into()]).unwrap()); }

#[cfg(test)]
mod tests {
    use super::*;
    /// Credential masking and bounded Unicode retain useful error details without leaking authentication.
    #[test]
    fn redacts_credentials_and_bounds_unicode() {
        init_test();
        let message="TLS failed 世界; Authorization: Bearer private-token; refresh_token=secret-value; {\"password\":\"private value\"}; sk-testsecret123456; https://user:pass@example.test/";
        let text=clean(message);
        for secret in ["private-token","secret-value","private value","sk-testsecret123456","user:pass"] { assert!(!text.contains(secret)); }
        assert!(text.contains("TLS failed 世界"));
        assert!(!clean("local-test-password-only").contains("local-test-password-only"));
        assert!(!clean("Cookie: session=abc; another=def").contains("def"));
        assert!(clean(&"世界".repeat(5000)).ends_with("[Diagnostic truncated]"));
    }

    /// Logs survive restart and cursor pagination stays stable while new diagnostics arrive.
    #[test]
    fn persistence_filtering_and_retention()->Result<()> {
        let directory=tempfile::tempdir()?; let path=directory.path().join("logs.db");
        let log=Log::open(&path,vec![])?;
        for _ in 0..105 { log.insert("error","quota",Some("request-世界"),Some("account-1"),"Refresh failed")?; }
        let first=log.list(&Filter::default())?;
        assert_eq!(first["entries"].as_array().unwrap().len(),100);
        let before=first["next_before"].as_i64();
        log.insert("info","startup",None,None,"Started")?;
        assert_eq!(log.list(&Filter { before,..Default::default() })?["entries"].as_array().unwrap().len(),5);
        assert_eq!(log.list(&Filter { level:Some("info".into()),..Default::default() })?["entries"].as_array().unwrap().len(),1);
        assert_eq!(log.list(&Filter { search:"世界".into(),..Default::default() })?["entries"].as_array().unwrap().len(),100);
        log.connection.lock().unwrap().execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10010) INSERT INTO diagnostics(created_at,level,stage,message) SELECT ?1,'error','test','Repeated failure' FROM n",[now()])?;
        log.insert("info","test",None,None,"Retention check")?;
        assert_eq!(log.connection.lock().unwrap().query_row("SELECT COUNT(*) FROM diagnostics",[],|row|row.get::<_,i64>(0))?,MAX_ENTRIES);
        log.connection.lock().unwrap().execute("UPDATE diagnostics SET created_at=0",[])?;
        log.insert("info","startup",None,None,"Fresh")?;
        assert_eq!(log.list(&Filter::default())?["entries"].as_array().unwrap().len(),1);
        drop(log);
        assert_eq!(Log::open(&path,vec![])?.list(&Filter::default())?["entries"][0]["message"],"Fresh");
        Ok(())
    }
}
