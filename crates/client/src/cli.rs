use std::io::{self, Write};
use anyhow::Result;
use serde_json::{json, Value};
use crate::local::Connection;

/// Run the terminal frontend against exactly the same daemon as browser mode.
pub async fn run(connection: Connection) -> Result<()> {
    println!("{} — /help for commands, /quit to leave the prompt",safe(connection.get("/api/branding").await?["name"].as_str().unwrap()));
    let mut state = connection.get("/api/state").await?;
    println!("Local shell: {}",state["shell"].as_str().unwrap());
    if state["authenticated"]==true {
        match connection.post("/api/account",json!({})).await {
            Ok(current) => state=current,
            Err(error) => { println!("{}",safe(&error.to_string())); state=connection.get("/api/state").await?; }
        }
    }
    if state["authenticated"]!=true {
        println!("Backend: {}",state["settings"]["backend"].as_str().unwrap());
        println!("Type /login to sign in with email.");
    }
    loop {
        let Some(line) = read_line("you> ")? else { break; };
        let line = line.trim();
        if line.is_empty() { continue; }
        if matches!(line,"/quit"|"/exit") { break; }
        let result = handle(&connection,line,&state).await;
        match result {
            Ok(Some(next)) => { state=next; display(&state,line.starts_with('/') && !matches!(line,"/approve"|"/deny"|"/retry")); }
            Ok(None) => {}
            Err(error) => {
                println!("Error: {}",safe(&error.to_string()));
                state=connection.get("/api/state").await?;
                if state["retry_available"]==true { println!("Use /retry to safely retry this request, or /new to start over."); }
            }
        }
        if line=="/stop" { break; }
    }
    Ok(())
}

/// Keep command approval explicit and separate from ordinary conversational text.
async fn handle(connection: &Connection, line: &str, state: &Value) -> Result<Option<Value>> {
    let (name,arg) = line.split_once(' ').unwrap_or((line,""));
    let next = match name {
        "/help" => {
            println!("/login  /logout  /commands on|off\n/full on|off  /approve  /deny  /credits  /buy  /new  /retry  /history\n/stop (stop daemon)  /quit (leave prompt)");
            return Ok(None);
        }
        "/login" => {
            let Some(email)=read_line("Email: ")? else { return Ok(None); };
            let login=connection.post("/api/login",json!({"email":email.trim()})).await?;
            if let Some(code)=login["development_code"].as_str() { println!("Development code: {code}"); }
            println!("Enter the six-digit code sent to your email.");
            loop {
                let Some(code)=read_line("Code (empty to cancel): ")? else { return Ok(None); };
                if code.trim().is_empty() { return Ok(None); }
                match connection.post("/api/login/verify",json!({"code":code.trim()})).await {
                    Ok(state)=>break state,
                    Err(error)=>println!("{}",safe(&error.to_string())),
                }
            }
        }
        "/commands" => {
            anyhow::ensure!(["on","off"].contains(&arg),"Use /commands on or /commands off");
            connection.post("/api/settings",json!({"commands_enabled":arg=="on"})).await?
        }
        "/full" => {
            anyhow::ensure!(["on","off"].contains(&arg),"Use /full on or /full off; enable commands first");
            live_post(connection,"/api/access",json!({"enabled":arg=="on"})).await?
        }
        "/approve"|"/deny" => {
            anyhow::ensure!(!state["pending"].is_null(),"No pending command; use /history to refresh shared state");
            live_post(connection,"/api/command",json!({"id":state["pending"]["id"],"approve":name=="/approve"})).await?
        }
        "/credits" => connection.post("/api/account",json!({})).await?,
        "/buy" => {
            let checkout=connection.post("/api/checkout",json!({})).await?;
            let url=checkout["url"].as_str().unwrap();
            println!("Buy credits: {url}");
            let _=webbrowser::open(url);
            return Ok(None);
        }
        "/logout" => connection.post("/api/logout",json!({})).await?,
        "/retry" => live_post(connection,"/api/retry",json!({})).await?,
        "/new" => connection.post("/api/new",json!({})).await?,
        "/history" => {
            let next=connection.get("/api/state").await?;
            if let Some(messages)=next["messages"].as_array() {
                for message in messages { println!("{}: {}",message["role"].as_str().unwrap(),safe(message["text"].as_str().unwrap())); }
            }
            next
        }
        "/stop" => { connection.post("/api/stop",json!({})).await?; println!("Local daemon stopped."); return Ok(None); }
        _ if name.starts_with('/') => anyhow::bail!("Unknown command; use /help"),
        _ => live_post(connection,"/api/chat",json!({"text":line,"cwd":std::env::current_dir()?.to_string_lossy()})).await?,
    };
    Ok(Some(next))
}

/// Show exact shell text and directory before offering approval.
fn display(state: &Value, show_text: bool) {
    if let Some(last)=state["messages"].as_array().and_then(|messages|messages.last()).filter(|_|show_text) {
        println!("\n{}: {}\n",last["role"].as_str().unwrap_or("assistant"),safe(last["text"].as_str().unwrap_or_default()));
    }
    if !state["pending"].is_null() {
        let proposal=&state["pending"]["proposal"];
        println!("Proposed local command:\n{}\nDirectory: {}\nReason: {}\nType /approve to run or /deny to decline.",
            safe(proposal["command"].as_str().unwrap()),safe(proposal["cwd"].as_str().unwrap()),safe(proposal["reason"].as_str().unwrap()));
    }
    let permission=if state["full_access"]==true { "full access" } else if state["settings"]["commands_enabled"]==true { "approval required" } else { "off" };
    if let Some(credits)=state["account"]["credits"].as_i64() { println!("Credits: {credits} | commands: {permission}"); }
}

/// Observe the same live snapshots as the browser while a durable local operation runs.
async fn live_post(connection: &Connection, path: &str, body: Value) -> Result<Value> {
    // Validate discovery before attaching the capability to a second connection.
    connection.get("/api/health").await?;
    let mut events=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build()?
        .get(format!("{}/api/events",connection.url)).bearer_auth(&connection.secret).send().await?.error_for_status()?;
    let operation=connection.post(path,body);
    tokio::pin!(operation);
    let mut decoder=crate::stream::Decoder::local();
    let (mut label,mut text,mut turn)=(String::new(),String::new(),String::new());
    let mut summaries=std::collections::HashMap::<String,String>::new();
    loop {
        tokio::select! {
            result=&mut operation => {
                let state=result?;
                if let Some(last)=state["messages"].as_array().and_then(|messages|messages.last()) {
                    let final_text=last["text"].as_str().unwrap_or_default();
                    println!("{}",safe(final_text.strip_prefix(&text).unwrap_or(final_text)));
                }
                return Ok(state);
            }
            chunk=events.chunk() => {
                let Some(bytes)=chunk? else { return operation.await; };
                for view in decoder.push(&bytes)? {
                    if view["busy"]!=true { continue; }
                    let activity=&view["activity"];
                    let current=activity["label"].as_str().unwrap_or_default();
                    if current!=label { println!("\n{}",safe(current)); label=current.into(); }
                    let progress=&activity["progress"];
                    let key=format!("{}:{}",view["state"]["messages"].as_array().map_or(0,Vec::len),progress["attempt"]);
                    if key!=turn { turn=key; text.clear(); summaries.clear(); }
                    if let Some(parts)=progress["summaries"].as_array() {
                        for part in parts {
                            let id=part["id"].as_str().unwrap_or_default().to_owned();
                            let value=part["text"].as_str().unwrap_or_default();
                            let previous=summaries.entry(id).or_default();
                            print!("{}",safe(value.strip_prefix(previous.as_str()).unwrap_or(value)));
                            *previous=value.into();
                        }
                    }
                    let value=progress["text"].as_str().unwrap_or_default();
                    print!("{}",safe(value.strip_prefix(&text).unwrap_or(value)));
                    text=value.into();
                    io::stdout().flush()?;
                }
            }
        }
    }
}

/// Use a plain portable prompt without requiring a terminal UI runtime.
fn read_line(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut value=String::new();
    Ok((io::stdin().read_line(&mut value)?>0).then_some(value))
}

/// Strip terminal control sequences from model and process output before displaying them.
fn safe(value: &str) -> String { value.chars().filter(|c|!c.is_control() || matches!(c,'\n'|'\t')).collect() }
