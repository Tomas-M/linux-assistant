use std::{path::Path,sync::{Arc,atomic::Ordering}};
use anyhow::Result;
use serde_json::{json,Value};
use tokio::sync::Mutex;
use crate::{engine::Engine,stream::Live,controls::Controls,processes::Processes};

/// Both interfaces use the same queue worker, independently of their display connection.
pub async fn run(engine:Arc<Mutex<Engine>>,retry:bool)->Result<Value> {
    let mut engine=match engine.try_lock_owned() {
        Ok(engine)=>engine,
        Err(_) if !retry=>return Ok(Value::Null),
        Err(error)=>return Err(error.into()),
    };
    let live=engine.live.clone();
    engine.controls.stopped.store(false,Ordering::SeqCst);
    live.0.send_modify(|view| { view["busy"]=json!(true); view["error"]=Value::Null; view["activity"]=Value::Null; view["steering"]=json!([]); });
    tokio::spawn(async move {
        let result=async {
            if retry { engine.retry().await?; } else { engine.next().await?; }
            while !engine.controls.stopped.load(Ordering::SeqCst) {
                if !engine.next().await? { break; }
            }
            Ok::<_,anyhow::Error>(())
        }.await;
        let presentation=if result.is_err() || engine.controls.stopped.load(Ordering::SeqCst) {
            engine.finish_activity(result.as_ref().err().map(ToString::to_string))
        } else { Ok(()) };
        let result=result.and(presentation).map(|_|engine.state());
        live.state(engine.state());
        live.0.send_modify(|view| {
            view["busy"]=json!(false); view["can_steer"]=json!(false); view["steering"]=json!([]);
            view["error"]=json!(result.as_ref().err().map(ToString::to_string));
            if result.is_ok() { view["activity"]=Value::Null; }
        });
        result
    }).await?
}

/// Publish accepted steering without waiting for generation to release the engine.
pub fn record_steering(live:&Live,input:&protocol::Steering) {
    live.0.send_modify(|view| {
        if view["busy"]!=true { return; }
        let after=view["activity"]["progress"]["summaries"].as_array().map_or(0,Vec::len);
        let inputs=view["steering"].as_array_mut().unwrap();
        if inputs.iter().any(|old|old["id"]==input.id.to_string()) { return; }
        let mut value=protocol::display_messages(&[input])[0].clone();
        value["attachments"]=crate::media::attachments(&input.id.to_string(),input.attachments.as_deref());
        value["progress_after"]=json!(after);
        inputs.push(value);
    });
}

/// Stop only commands belonging to the current task; keep the access preference unchanged.
pub async fn interrupt(controls:&Controls,live:&Live,processes:&Processes)->Result<()> {
    controls.cancel_local();
    let ids={
        let view=live.0.borrow();
        let start=view["runs"].as_array().and_then(|runs|runs.last()).filter(|run|run["finished_at"].is_null()).and_then(|run|run["start"].as_u64()).unwrap_or(u64::MAX);
        view["commands"].as_array().unwrap().iter().filter(|command|command["id"]==command["action_id"])
            .filter(|command|command["after"].as_u64().is_some_and(|after|after>=start)).filter_map(|command|command["id"].as_str()?.parse().ok()).collect::<Vec<uuid::Uuid>>()
    };
    processes.stop_foreground(Some(&ids));
    controls.interrupt().await
}

/// A rejected control/account request wakes a busy turn before clearing that exact saved session.
pub async fn expire(engine:&Arc<Mutex<Engine>>,controls:&Controls,path:&Path,expired:&crate::backend::SessionExpired)->Result<()> {
    let saved:crate::engine::Saved=crate::storage::read(path)?;
    if saved.token.as_deref()!=Some(expired.0.as_str()) { return Ok(()); }
    controls.failures.send_replace(Some(Arc::new(expired.clone().into())));
    controls.cancel_local();
    engine.lock().await.expire_session(expired).await
}

/// Account reads stay available during generation; credentials never enter display snapshots.
pub async fn account(path:&Path,live:&Live,method:reqwest::Method,route:&str,body:Option<Value>)->Result<Value> {
    use anyhow::Context;
    let saved:crate::engine::Saved=crate::storage::read(path)?;
    let mut request=crate::backend::client(60)?.request(method,format!("{}{route}",saved.settings.backend)).bearer_auth(saved.token.context("Sign in first")?);
    if let Some(body)=body { request=request.json(&body); }
    let response=crate::backend::send(request,live,None).await?;
    Engine::response(response).await
}
