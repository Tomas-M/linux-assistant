use std::collections::HashMap;
use anyhow::{ensure,Context,Result};
use protocol::{Interaction,InteractionKind,InteractionAnswer};
use serde_json::{json,Value};
use uuid::Uuid;
use super::progress::Reporter;

#[derive(Default)]
pub(super) struct Requests { pending:HashMap<Uuid,(Value,Interaction)> }
impl Requests {
    /// Only advertised client tools and user questions may cross the backend-to-client boundary.
    pub fn receive(&mut self,event:&Value,enabled:bool,reporter:&mut Reporter,db:&crate::db::Db,user:Option<i64>,request_id:Uuid)->Result<()> {
        let params=&event["params"];
        let (item_id,kind)=match event["method"].as_str() {
            Some("item/tool/call")=> {
                ensure!(params["namespace"].is_null(),"Unexpected tool namespace");
                let action=protocol::tool_action(params["tool"].as_str().context("Missing tool name")?,params["arguments"].clone())?;
                ensure!(enabled || action.is_poll(),"Local access is disabled");
                (params["callId"].as_str().context("Missing tool call ID")?,InteractionKind::Tool { action })
            }
            Some("item/tool/requestUserInput")=> {
                let questions:Vec<protocol::Question>=serde_json::from_value(params["questions"].clone())?;
                ensure!(!questions.is_empty() && questions.len()<=3 && params["questions"].to_string().len()<=24_000,"Invalid question request");
                let unique=questions.iter().map(|question|&question.id).collect::<std::collections::HashSet<_>>();
                ensure!(unique.len()==questions.len(),"Question IDs must be unique");
                (params["itemId"].as_str().context("Missing question item ID")?,InteractionKind::Questions { questions })
            }
            _=>anyhow::bail!("Unsupported server request"),
        };
        ensure!(self.pending.len()<16,"Too many simultaneous client requests");
        let request=Interaction { id:Uuid::new_v4(),item_id:item_id.into(),kind };
        if let Some(user)=user { db.0.lock().unwrap().execute("INSERT INTO tool_calls(id,user_id,request_id,interaction) VALUES(?1,?2,?3,?4)",rusqlite::params![request.id.to_string(),user,request_id.to_string(),serde_json::to_string(&request)?])?; }
        self.pending.insert(request.id,(event["id"].clone(),request.clone()));
        reporter.snapshot.requests.push(request);
        reporter.publish(true);
        Ok(())
    }

    /// Validate an answer against its actual pending question/tool and acknowledge exact retries once.
    pub fn answer(&mut self,id:Uuid,answer:InteractionAnswer,reporter:&mut Reporter,db:&crate::db::Db,user:Option<i64>,request_id:Uuid)->Result<Option<(Value,Value)>> {
        if !self.pending.contains_key(&id) {
            let user=user.context("Unknown pending request")?;
            let previous:String=db.0.lock().unwrap().query_row("SELECT answer FROM tool_calls WHERE id=?1 AND user_id=?2 AND request_id=?3 AND answer IS NOT NULL",rusqlite::params![id.to_string(),user,request_id.to_string()],|row|row.get(0)).context("This request is no longer waiting for an answer")?;
            ensure!(serde_json::from_str::<InteractionAnswer>(&previous)?==answer,"Answer ID reused with different content");
            return Ok(None);
        }
        let (rpc,request)=self.pending.get(&id).context("This request is no longer waiting for an answer")?;
        let result=match (&request.kind,&answer) {
            (InteractionKind::Tool { .. },InteractionAnswer::Tool { output,success })=> {
                ensure!(output.len()<=16_000,"Tool result exceeds the output budget");
                json!({"contentItems":[{"type":"inputText","text":output}],"success":success})
            }
            (InteractionKind::Questions { questions },InteractionAnswer::Questions { answers })=> {
                protocol::validate_answers(questions,answers)?;
                let answers=answers.iter().map(|(id,answers)|(id.clone(),json!({"answers":answers}))).collect::<serde_json::Map<_,_>>();
                json!({"answers":answers})
            }
            _=>anyhow::bail!("Answer does not match the pending request"),
        };
        let rpc=rpc.clone();
        if let Some(user)=user { db.0.lock().unwrap().execute("UPDATE tool_calls SET answer=?1 WHERE id=?2 AND user_id=?3",rusqlite::params![serde_json::to_string(&answer)?,id.to_string(),user])?; }
        self.pending.remove(&id);
        reporter.snapshot.requests.retain(|request|request.id!=id);
        reporter.publish(true);
        Ok(Some((rpc,result)))
    }
}
