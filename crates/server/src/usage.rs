use anyhow::Result;
use rusqlite::params;
use serde_json::Value;
use crate::db::{now, Db};

pub struct Attempt<'a> { pub db: &'a Db, pub id: i64, pub purpose: &'static str, finished: bool }

impl<'a> Attempt<'a> {
    /// Record each provider attempt, including retries and administrator tests, before starting it.
    pub fn start(db: &'a Db, user: Option<i64>, request: &protocol::ChatRequest, home: &str, purpose: &'static str) -> Result<Self> {
        let conn=db.0.lock().unwrap();
        conn.execute("INSERT INTO usage(user_id,request_id,conversation_id,account_home,purpose,started_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![user,request.request_id.to_string(),request.conversation_id.to_string(),home,purpose,now()])?;
        Ok(Self { db,id:conn.last_insert_rowid(),purpose,finished:false })
    }

    /// Save identifiers without retaining an extra copy of prompts or responses.
    pub fn identify(&self, thread: &str, turn: &str, model: Option<&str>) -> Result<()> {
        self.db.0.lock().unwrap().execute("UPDATE usage SET thread_id=?1,turn_id=?2,model=?3 WHERE id=?4",params![thread,turn,model,self.id])?;
        Ok(())
    }

    /// Use differences of cumulative counters: repeated notifications must not double-count usage.
    pub fn tokens(&self, total: &Value, baseline: &Value) -> Result<()> {
        let fields=["inputTokens","cachedInputTokens","outputTokens","reasoningOutputTokens","totalTokens"];
        let counts: Option<Vec<i64>>=fields.iter().map(|key| {
            let end=total[*key].as_i64()?;
            let start=baseline[*key].as_i64()?;
            end.checked_sub(start).filter(|value|*value>=0)
        }).collect();
        if let Some(c)=counts {
            self.db.0.lock().unwrap().execute("UPDATE usage SET input_tokens=?1,cached_tokens=?2,output_tokens=?3,reasoning_tokens=?4,total_tokens=?5 WHERE id=?6",
                params![c[0],c[1],c[2],c[3],c[4],self.id])?;
        }
        Ok(())
    }

    /// Preserve failures even when the application refunds its own credit.
    pub fn finish(&mut self, error: Option<&str>) -> Result<()> {
        self.db.0.lock().unwrap().execute("UPDATE usage SET finished_at=?1,error=?2 WHERE id=?3",params![now(),error,self.id])?;
        self.finished=true;
        Ok(())
    }
}

impl Drop for Attempt<'_> {
    /// Cancellation and timeouts leave an auditable partial attempt, not an indefinitely running row.
    fn drop(&mut self) {
        if !self.finished { let _=self.finish(Some("Interrupted or timed out; token counts may be partial")); }
    }
}
