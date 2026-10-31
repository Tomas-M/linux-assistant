use anyhow::{ensure, Result};
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
                self.commentary=params["item"]["phase"]=="commentary";
                self.json.clear();
                self.snapshot.text.clear();
            }
            Some("item/reasoning/summaryPartAdded"|"item/reasoning/summaryTextDelta") => {
                let item=params["itemId"].as_str().unwrap_or_default();
                let id=format!("{item}:{}",params["summaryIndex"]);
                let delta=params["delta"].as_str().unwrap_or_default();
                ensure!(self.snapshot.summaries.iter().map(|part|part.text.len()).sum::<usize>()+delta.len()<=64_000,"Progress summary too long");
                self.part(&id,item,ProgressKind::Reasoning)?.text.push_str(delta);
            }
            Some("item/completed") if params["item"]["type"]=="reasoning"=> {
                let item=params["item"]["id"].as_str().unwrap_or_default();
                for (index,text) in params["item"]["summary"].as_array().into_iter().flatten().enumerate() {
                    let part=self.part(&format!("{item}:{index}"),item,ProgressKind::Reasoning)?;
                    part.text=text.as_str().unwrap_or_default().into();
                    part.completed=true;
                }
                for part in self.snapshot.summaries.iter_mut().filter(|part|part.item_id==item) { part.completed=true; }
            }
            Some("item/agentMessage/delta") => {
                let id=params["itemId"].as_str().unwrap_or_default();
                if self.item!=id { self.item=id.into(); self.commentary=false; self.json.clear(); self.snapshot.text.clear(); }
                let delta=params["delta"].as_str().unwrap_or_default();
                ensure!(self.json.len()+delta.len()<=512_000,"Streamed answer too long");
                self.json.push_str(delta);
                if self.commentary {
                    ensure!(self.json.len()<=16_000,"Progress commentary too long");
                    let text=self.json.clone();
                    self.part(id,id,ProgressKind::Commentary)?.text=text;
                } else { self.snapshot.text=text_prefix(&self.json).unwrap_or_default(); }
            }
            Some("item/completed") if params["item"]["type"]=="agentMessage" => {
                let text=params["item"]["text"].as_str().unwrap_or_default();
                ensure!(text.len()<=512_000,"Streamed answer too long");
                if params["item"]["phase"]=="commentary" {
                    ensure!(text.len()<=16_000,"Progress commentary too long");
                    let id=params["item"]["id"].as_str().unwrap_or_default();
                    let part=self.part(id,id,ProgressKind::Commentary)?;
                    part.text=text.into(); part.completed=true;
                    self.snapshot.text.clear();
                } else { self.snapshot.text=text_prefix(text).unwrap_or_default(); }
            }
            _ => return Ok(()),
        }
        ensure!(self.snapshot.text.len()<=64_000,"Answer preview too long");
        ensure!(self.snapshot.summaries.iter().map(|part|part.text.len()).sum::<usize>()<=64_000,"Progress summary too long");
        self.publish(matches!(event["method"].as_str(),Some("item/started"|"item/completed")));
        Ok(())
    }

    /// Preserve native item identity, kind and section ordering instead of flattening commentary into reasoning.
    fn part(&mut self,id:&str,item:&str,kind:ProgressKind)->Result<&mut ProgressText> {
        if let Some(index)=self.snapshot.summaries.iter().position(|part|part.id==id) { return Ok(&mut self.snapshot.summaries[index]); }
        ensure!(self.snapshot.summaries.len()<128,"Too many progress summaries");
        self.snapshot.summaries.push(ProgressText { id:id.into(),item_id:item.into(),kind,completed:false,text:String::new() });
        Ok(self.snapshot.summaries.last_mut().unwrap())
    }

    /// Coalesce token bursts to ten snapshots per second; stage changes bypass the delay.
    fn publish(&mut self,force:bool) {
        if !force && self.published.elapsed()<Duration::from_millis(100) { return; }
        if let Some(sender)=&self.sender { sender.send_replace(ChatEvent::Progress { progress:self.snapshot.clone() }); }
        self.published=Instant::now();
    }
}

impl Drop for Reporter {
    /// Completion, Stop and disconnected clients retain the latest valid partial answer.
    fn drop(&mut self) {
        let summaries=self.snapshot.summaries.iter().map(|part|part.text.len()).sum::<usize>();
        if self.snapshot.text.len()<=64_000 && summaries<=64_000 { self.publish(true); }
    }
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
