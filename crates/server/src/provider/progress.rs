use anyhow::{ensure,Result};
use protocol::{ChatEvent, Progress, ProgressText,ProgressKind};
use serde_json::Value;
use tokio::sync::watch;
use std::time::{Duration,Instant};

pub(super) struct Reporter {
    pub snapshot: Progress,
    sender: Option<watch::Sender<ChatEvent>>,
    json: String,
    item: String,
    commentary: bool,
    published: Instant,
}

impl Reporter {
    /// Each quota failover starts a new preview; text from failed attempts cannot mix with the answer.
    pub fn new(sender: Option<&watch::Sender<ChatEvent>>, attempt: usize) -> Self {
        let mut reporter=Self { snapshot:Progress { attempt,..Default::default() },sender:sender.cloned(),json:String::new(),item:String::new(),commentary:false,published:Instant::now() };
        reporter.publish(true);
        reporter
    }

    /// Forward public summaries and readable structured text only, never raw reasoning or command fragments.
    pub fn event(&mut self, event: &Value) -> Result<()> {
        let params=&event["params"];
        match event["method"].as_str() {
            Some("turn/plan/updated")=> {
                let id=format!("plan:{}",params["turnId"].as_str().unwrap_or_default());
                let part=self.part(&id,&id,ProgressKind::Plan)?;
                part.text="Plan".into();
                let plan=params["plan"].as_array().into_iter().flatten().take(64).map(|step|serde_json::json!({"step":clip(step["step"].as_str().unwrap_or_default(),512),"status":step["status"]})).collect::<Vec<_>>();
                part.data=Some(serde_json::json!({"explanation":clip(params["explanation"].as_str().unwrap_or_default(),4000),"plan":plan}));
                part.completed=params["plan"].as_array().is_some_and(|steps|steps.iter().all(|step|step["status"]=="completed"));
            }
            Some("item/started"|"item/completed") if matches!(params["item"]["type"].as_str(),Some("dynamicToolCall"|"webSearch"))=> {
                let item=&params["item"]; let id=item["id"].as_str().unwrap_or_default();
                let kind=if item["type"]=="webSearch" { ProgressKind::WebSearch } else { ProgressKind::Action };
                let part=self.part(id,id,kind)?;
                part.text=if kind==ProgressKind::WebSearch { format!("{}: {}",if event["method"]=="item/completed" { "Searched the web" } else { "Searching the web" },item["query"].as_str().unwrap_or_default()) } else { item["arguments"]["reason"].as_str().unwrap_or("Working on your computer").into() };
                let arguments=&item["arguments"];
                part.data=Some(serde_json::json!({
                    "tool":item["tool"],"status":item["status"],
                    "action":if kind==ProgressKind::WebSearch { serde_json::json!({"type":item["action"]["type"],"query":clip(item["action"]["query"].as_str().unwrap_or_default(),2048),"url":clip(item["action"]["url"].as_str().unwrap_or_default(),2048),"pattern":clip(item["action"]["pattern"].as_str().unwrap_or_default(),512)}) } else { Value::Null },
                    "arguments":{"cmd":clip(arguments["cmd"].as_str().unwrap_or_default(),2048),"path":clip(arguments["path"].as_str().unwrap_or_default(),4096),"workdir":clip(arguments["workdir"].as_str().unwrap_or_default(),4096),"session_id":arguments["session_id"]}
                }));
                part.completed=event["method"]=="item/completed";
            }
            Some("item/started"|"item/completed") if params["item"]["type"]=="contextCompaction" => {
                self.snapshot.activity=(event["method"]=="item/started").then(||"Compacting context…".into());
                if event["method"]=="item/completed" {
                    if let Some(id)=params["item"]["id"].as_str() {
                        let compacted=self.snapshot.compacted.get_or_insert_with(Vec::new);
                        if !compacted.iter().any(|old|old==id) { compacted.push(id.into()); }
                    }
                }
            }
            Some("item/started") if params["item"]["type"]=="agentMessage" => {
                self.item=params["item"]["id"].as_str().unwrap_or_default().into();
                self.commentary=matches!(params["item"]["phase"].as_str(),Some("commentary"|"partial_answer"));
                self.json.clear();
                self.snapshot.text.clear();
            }
            Some("item/reasoning/summaryPartAdded"|"item/reasoning/summaryTextDelta") => {
                let item=params["itemId"].as_str().unwrap_or_default();
                let id=format!("{item}:{}",params["summaryIndex"]);
                let delta=params["delta"].as_str().unwrap_or_default();
                let part=self.part(&id,item,ProgressKind::Reasoning)?;
                if part.text.len()<16_000 { part.text.push_str(&clip(delta,16_000-part.text.len())); }
            }
            Some("item/completed") if params["item"]["type"]=="reasoning"=> {
                let item=params["item"]["id"].as_str().unwrap_or_default();
                for (index,text) in params["item"]["summary"].as_array().into_iter().flatten().enumerate() {
                    let part=self.part(&format!("{item}:{index}"),item,ProgressKind::Reasoning)?;
                    part.text=clip(text.as_str().unwrap_or_default(),16_000);
                    part.completed=true;
                }
                for part in self.snapshot.summaries.iter_mut().filter(|part|part.item_id==item) { part.completed=true; }
            }
            Some("item/agentMessage/delta") => {
                let id=params["itemId"].as_str().unwrap_or_default();
                if self.item!=id { self.item=id.into(); self.commentary=false; self.json.clear(); self.snapshot.text.clear(); }
                let delta=params["delta"].as_str().unwrap_or_default();
                if self.commentary {
                    if self.json.len()<16_000 { self.json.push_str(&clip(delta,16_000-self.json.len())); }
                    let text=self.json.clone();
                    self.part(id,id,ProgressKind::Commentary)?.text=text;
                } else {
                    ensure!(self.json.len()+delta.len()<=512_000,"Streamed answer too long");
                    self.json.push_str(delta);
                    self.snapshot.text=clip(&text_prefix(&self.json).unwrap_or_default(),64_000);
                }
            }
            Some("item/completed") if params["item"]["type"]=="agentMessage" => {
                let text=params["item"]["text"].as_str().unwrap_or_default();
                if matches!(params["item"]["phase"].as_str(),Some("commentary"|"partial_answer")) {
                    let id=params["item"]["id"].as_str().unwrap_or_default();
                    let part=self.part(id,id,ProgressKind::Commentary)?;
                    part.text=clip(text,16_000); part.completed=true;
                    self.snapshot.text.clear();
                } else { self.snapshot.text=clip(&text_prefix(text).unwrap_or_default(),64_000); }
            }
            _ => return Ok(()),
        }
        self.publish(matches!(event["method"].as_str(),Some("item/started"|"item/completed")));
        Ok(())
    }

    /// Preserve native item identity, kind and section ordering instead of flattening commentary into reasoning.
    fn part(&mut self,id:&str,item:&str,kind:ProgressKind)->Result<&mut ProgressText> {
        if let Some(index)=self.snapshot.summaries.iter().position(|part|part.id==id) { return Ok(&mut self.snapshot.summaries[index]); }
        if self.snapshot.summaries.len()>=128 {
            let index=self.snapshot.summaries.iter().position(|part|part.completed && part.kind!=ProgressKind::Plan).unwrap_or(0);
            self.snapshot.summaries.remove(index);
        }
        self.snapshot.summaries.push(ProgressText { id:id.into(),item_id:item.into(),kind,completed:false,data:None,text:String::new() });
        Ok(self.snapshot.summaries.last_mut().unwrap())
    }

    /// Coalesce token bursts to ten snapshots per second; stage changes bypass the delay.
    pub(super) fn publish(&mut self,force:bool) {
        if !force && self.published.elapsed()<Duration::from_millis(100) { return; }
        for part in &mut self.snapshot.summaries { if part.text.len()>16_000 { part.text=clip(&part.text,16_000); } }
        // Evict display history, never fail the paid turn because public progress is long.
        while self.snapshot.summaries.len()>1 && self.snapshot.summaries.iter().map(|part|part.text.len()+part.data.as_ref().map_or(0,|data|data.to_string().len())+256).sum::<usize>()>96_000 {
            let index=self.snapshot.summaries.iter().position(|part|part.completed && part.kind!=ProgressKind::Plan).unwrap_or(0);
            self.snapshot.summaries.remove(index);
        }
        if let Some(sender)=&self.sender { sender.send_replace(ChatEvent::Progress { progress:self.snapshot.clone() }); }
        self.published=Instant::now();
    }
}

impl Drop for Reporter {
    /// Completion, Stop and disconnected clients retain the latest valid partial answer.
    fn drop(&mut self) {
        self.publish(true);
    }
}

/// Bound public display metadata at UTF-8 boundaries without changing actual tool arguments or model history.
fn clip(text:&str,limit:usize)->String {
    if text.len()<=limit { return text.into(); }
    if limit<3 { return String::new(); }
    let mut end=limit.saturating_sub(3);
    while !text.is_char_boundary(end) { end-=1; }
    format!("{}…",&text[..end])
}

/// Decode only the root text string, including split escapes and UTF-16 surrogate pairs.
fn text_prefix(input: &str) -> Option<String> {
    let mut rest=input.trim_start().strip_prefix('{')?.trim_start();
    loop {
        let mut key=serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let name=key.next()?.ok()?;
        rest=rest[key.byte_offset()..].trim_start().strip_prefix(':')?.trim_start();
        if name=="text" {
            let value=rest.strip_prefix('"')?;
            let mut end=0;
            let mut escaped=false;
            for (index,ch) in value.char_indices() {
                if ch=='"' && !escaped { return serde_json::from_str(&rest[..index+2]).ok(); }
                if ch=='\\' { escaped=!escaped; } else { escaped=false; }
                if !escaped { end=index+ch.len_utf8(); }
            }
            // Only the final incomplete escape can fail; discard at most a surrogate pair.
            for _ in 0..13 {
                if let Ok(text)=serde_json::from_str(&format!("\"{}\"",&value[..end])) { return Some(text); }
                if end==0 { break; }
                end=value[..end].char_indices().last()?.0;
            }
            return None;
        }
        let mut value=serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        value.next()?.ok()?;
        rest=rest[value.byte_offset()..].trim_start().strip_prefix(',')?.trim_start();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fast completion retains each compaction exactly once in the latest progress snapshot.
    #[test]
    fn retains_completed_compaction()->Result<()> {
        let mut reporter=Reporter::new(None,1);
        reporter.event(&serde_json::json!({"method":"item/started","params":{"item":{"type":"contextCompaction","id":"compact-1"}}}))?;
        assert_eq!(reporter.snapshot.activity.as_deref(),Some("Compacting context…"));
        for _ in 0..2 { reporter.event(&serde_json::json!({"method":"item/completed","params":{"item":{"type":"contextCompaction","id":"compact-1"}}}))?; }
        assert!(reporter.snapshot.activity.is_none());
        assert_eq!(reporter.snapshot.compacted.as_ref().unwrap(),&vec!["compact-1"]);
        Ok(())
    }

    /// Every transport split must preserve readable Unicode without exposing nested command text.
    #[test]
    fn decodes_partial_json_without_command_leaks() {
        let source=r#"{"command":{"text":"SECRET","command":"never run"},"text":"Hello \"世界\" \ud83d\ude42\nDone"}"#;
        let expected="Hello \"世界\" 🙂\nDone";
        let mut previous=String::new();
        for end in (0..=source.len()).filter(|end|source.is_char_boundary(*end)) {
            let text=text_prefix(&source[..end]).unwrap_or_default();
            assert!(expected.starts_with(&text),"{text:?}");
            assert!(text.starts_with(&previous));
            previous=text;
        }
        assert_eq!(previous,expected);
    }
}
