use anyhow::{ensure, Result};
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio_stream::{wrappers::WatchStream,StreamExt};

#[derive(Clone)]
pub struct Live(pub watch::Sender<Value>);

impl Live {
    /// Keep one current view independently of the conversation lock for reconnecting browsers.
    pub fn new() -> Self { Self(watch::channel(json!({"state":null,"busy":false,"activity":null,"error":null,"steps":[],"runs":[],"commands":[]})).0) }

    /// Publish durable conversation state without credentials or duplicated output logs.
    pub fn state(&self, state: Value) { self.0.send_modify(|view|view["state"]=state); }

    /// Update operation status while leaving the latest saved conversation available.
    pub fn activity(&self, label: &str, progress: Option<protocol::Progress>) {
        self.0.send_modify(|view| {
            if let Some(progress)=&progress {
                let after=view["state"]["messages"].as_array().map_or(0,Vec::len);
                let steps=view["steps"].as_array_mut().unwrap();
                let step=json!({"after":after,"summaries":progress.summaries,"attempt":progress.attempt});
                // Text-only deltas do not change saved progress or require serializing its entire history.
                if steps.last()!=Some(&step) {
                    if steps.last().is_some_and(|step|step["after"]==after) { steps.pop(); }
                    steps.push(step);
                    while steps.len()>1 && steps.iter().map(|step|step.to_string().len()).sum::<usize>()>128_000 { steps.remove(0); }
                }
            }
            view["activity"]=json!({"label":label,"progress":progress});
        });
    }

    /// Each connection receives the current snapshot immediately; watch bounds memory for slow tabs.
    pub fn response(&self) -> axum::response::Response {
        use axum::response::{IntoResponse,sse::{Event,KeepAlive,Sse}};
        Sse::new(WatchStream::new(self.0.subscribe()).map(|view|Ok::<_,std::convert::Infallible>(Event::default().json_data(view).unwrap())))
            .keep_alive(KeepAlive::default()).into_response()
    }
}

pub struct Decoder { buffer: Vec<u8>,limit:usize }
impl Default for Decoder {
    /// Provider events are bounded independently of the size of a local visible transcript.
    fn default()->Self { Self { buffer:vec![],limit:1_000_000 } }
}

impl Decoder {
    /// Local authenticated snapshots include the entire visible history and can exceed a model request.
    pub fn local()->Self { Self { buffer:vec![],limit:usize::MAX } }
    /// Parse SSE across arbitrary UTF-8/chunk boundaries, accepting CRLF and heartbeat comments.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(bytes);
        let mut events=Vec::new();
        while let Some(end)=[self.buffer.windows(2).position(|v|v==b"\n\n").map(|i|(i,2)),
            self.buffer.windows(4).position(|v|v==b"\r\n\r\n").map(|i|(i,4))].into_iter().flatten().min() {
            ensure!(end.0<=self.limit,"Stream event exceeds size limit");
            let frame=std::str::from_utf8(&self.buffer[..end.0])?;
            let data=frame.lines().filter_map(|line|line.strip_prefix("data:").map(|data|data.strip_prefix(' ').unwrap_or(data))).collect::<Vec<_>>().join("\n");
            if !data.is_empty() { events.push(serde_json::from_str(&data)?); }
            self.buffer.drain(..end.0+end.1);
        }
        ensure!(self.buffer.len()<=self.limit,"Stream event exceeds size limit");
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Split every byte of multibyte text and ignore proxy keepalive comments.
    #[test]
    fn fragmented_sse() -> Result<()> {
        let mut parser=Decoder::default();
        let mut events=Vec::new();
        for byte in ": alive\r\n\r\ndata: {\"text\":\"世界🙂\"}\r\n\r\n".as_bytes() { events.extend(parser.push(&[*byte])?); }
        assert_eq!(events,vec![json!({"text":"世界🙂"})]);
        Ok(())
    }
}
