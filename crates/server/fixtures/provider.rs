//! A deterministic App Server fixture; never contacts OpenAI or executes commands.
use std::{fs, io::{self,BufRead,Write}, path::PathBuf, time::{Duration,Instant}};
use anyhow::{ensure, Result};
use serde_json::{json,Value};

/// Emulate persistent threads, quota changes and concurrent workers over the real JSONL transport.
fn main() -> Result<()> {
    let home=PathBuf::from(std::env::var("CODEX_HOME")?);
    let mut held:Option<(String,String)>=None;
    for line in io::stdin().lock().lines() {
        let request: Value=serde_json::from_str(&line?)?;
        let method=request["method"].as_str().unwrap_or_default();
        if method=="initialized" { continue; }
        writeln!(fs::OpenOptions::new().create(true).append(true).open(home.join("events.jsonl"))?,"{request}")?;
        let mut settings: Value=serde_json::from_slice(&fs::read(home.join("fixture.json"))?)?;
        let turn_id=uuid::Uuid::new_v4().to_string();
        let result=match method {
            "initialize" => json!({}),
            "account/read" => json!({"account":{"type":"chatgpt","email":"fixture@example.test","planType":"plus"}}),
            "account/rateLimits/read" => json!({"rateLimitsByLimitId":{"codex":{
                "primary":{"usedPercent":settings["primary"],"resetsAt":4000000000i64},
                "secondary":settings.get("secondary").map(|used|json!({"usedPercent":used,"resetsAt":4000000000i64}))
            }}}),
            "thread/start" => {
                let id=uuid::Uuid::new_v4().to_string();
                fs::write(home.join(format!("{id}.thread")),b"")?;
                json!({"thread":{"id":id},"model":"fixture-model"})
            }
            "thread/resume" => {
                let id=request["params"]["threadId"].as_str().unwrap();
                ensure!(home.join(format!("{id}.thread")).exists(),"Thread resumed under another account");
                json!({"thread":{"id":id},"model":"fixture-model"})
            }
            "thread/unsubscribe" => json!({}),
            "turn/start" => json!({"turn":{"id":turn_id,"status":"inProgress"}}),
            "turn/steer" => {
                let (thread,turn)=held.as_ref().expect("No active fixture turn");
                ensure!(request["params"]["threadId"]==*thread && request["params"]["expectedTurnId"]==*turn,"Wrong steering target");
                json!({"turnId":turn})
            }
            "turn/interrupt" => json!({}),
            _ => anyhow::bail!("Unexpected method {method}"),
        };
        if method=="turn/start" && settings["stream"]==true {
            let thread=&request["params"]["threadId"];
            println!("{}",json!({"method":"item/reasoning/summaryTextDelta","params":{"threadId":thread,"turnId":turn_id,"itemId":"reason","summaryIndex":0,"delta":"Checking 世界 before acknowledgement."}}));
            println!("{}",json!({"method":"item/reasoning/textDelta","params":{"threadId":thread,"turnId":turn_id,"delta":"PRIVATE_RAW_REASONING"}}));
        }
        println!("{}",json!({"id":request["id"],"result":result}));
        io::stdout().flush()?;
        if matches!(method,"turn/steer"|"turn/interrupt") {
            let (thread,turn)=held.take().expect("No active fixture turn");
            if method=="turn/steer" {
                let text=request["params"]["input"][0]["text"].as_str().unwrap();
                println!("{}",json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"type":"agentMessage","text":json!({"text":format!("Steered: {text}"),"command":null}).to_string()}}}));
            }
            println!("{}",json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"status":if method=="turn/interrupt" { "interrupted" } else { "completed" },"error":null}}}));
            io::stdout().flush()?;
        }
        if method=="thread/resume" {
            let thread=request["params"]["threadId"].as_str().unwrap();
            if let Ok(bytes)=fs::read(home.join(format!("{thread}.usage.json"))) {
                let usage:Value=serde_json::from_slice(&bytes)?;
                println!("{}",json!({"method":"thread/tokenUsage/updated","params":{"threadId":thread,"turnId":"previous-turn","tokenUsage":{"total":usage}}}));
                io::stdout().flush()?;
            }
        }
        if method=="turn/start" {
            let thread=request["params"]["threadId"].as_str().unwrap();
            println!("{}",json!({"method":"turn/started","params":{"threadId":thread,"turn":{"id":turn_id,"status":"inProgress"}}}));
            io::stdout().flush()?;
            if settings["hold"]==true {
                if settings["fragment"]==true {
                    let event=json!({"method":"item/reasoning/summaryTextDelta","params":{"threadId":thread,"turnId":turn_id,"itemId":"fragment","summaryIndex":0,"delta":"Fragmented 世界"}}).to_string();
                    let split=event.find('世').unwrap()+1;
                    io::stdout().write_all(&event.as_bytes()[..split])?;
                    io::stdout().flush()?;
                    fs::write(home.join("fragment-ready"),b"")?;
                    std::thread::sleep(Duration::from_millis(300));
                    io::stdout().write_all(&event.as_bytes()[split..])?;
                    println!();
                    io::stdout().flush()?;
                }
                held=Some((thread.to_owned(),turn_id.clone()));
                continue;
            }
            let usage_path=home.join(format!("{thread}.usage.json"));
            let mut usage:Value=match fs::read(&usage_path) {
                Ok(bytes)=>serde_json::from_slice(&bytes)?,
                Err(_)=>json!({"inputTokens":0,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":0}),
            };
            if settings["missing_usage"]!=true {
                for (field,delta) in [("inputTokens",100),("cachedInputTokens",20),("outputTokens",10),("reasoningOutputTokens",5),("totalTokens",110)] {
                    usage[field]=json!(usage[field].as_i64().unwrap()+delta);
                }
                fs::write(&usage_path,serde_json::to_vec(&usage)?)?;
                for _ in 0..2 { println!("{}",json!({"method":"thread/tokenUsage/updated","params":{"threadId":thread,"turnId":turn_id,"tokenUsage":{"total":usage}}})); }
            }
            if let Some(directory)=settings["barrier"].as_str() {
                let directory=PathBuf::from(directory);
                fs::write(home.join("arrived"),b"")?;
                let start=Instant::now();
                while !(directory.join("a/arrived").exists() && directory.join("b/arrived").exists()) {
                    ensure!(start.elapsed()<Duration::from_secs(10),"Accounts did not execute concurrently");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if let Some(error)=settings["turn_error"].as_str().map(str::to_owned) {
                if error=="usageLimitExceeded" {
                    settings["primary"]=json!(100);
                    fs::write(home.join("fixture.json"),serde_json::to_vec(&settings)?)?;
                }
                println!("{}",json!({"method":"turn/completed","params":{"turn":{"status":"failed","error":{"codexErrorInfo":error}}}}));
            } else {
                let text=format!("Fixture reply from {}",home.file_name().unwrap().to_string_lossy());
                let mut answer=json!({"text":text,"command":null});
                if request["params"]["outputSchema"]["properties"]["checkpoint"].is_object() { answer["checkpoint"]=json!("Fixture recovery summary: keep IMPORTANT_GOAL and completed command results."); }
                if settings["stream"]==true {
                    let serialized=answer.to_string();
                    for ch in serialized.chars() {
                        println!("{}",json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn_id,"itemId":"answer","delta":ch.to_string()}}));
                        io::stdout().flush()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                println!("{}",json!({"method":"item/completed","params":{"item":{"type":"agentMessage","text":answer.to_string()}}}));
                println!("{}",json!({"method":"turn/completed","params":{"turn":{"status":"completed","error":null}}}));
            }
            io::stdout().flush()?;
        }
    }
    Ok(())
}
