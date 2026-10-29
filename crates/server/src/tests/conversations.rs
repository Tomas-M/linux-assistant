use super::*;

/// The transport limit applies to new input, and replay returns the original revision after later turns.
#[tokio::test]
async fn long_history_uses_small_requests_and_stable_replay()->anyhow::Result<()> {
    let state=application()?;
    let user=state.db.user("long@example.test",None,30)?;
    let token=db::secret();
    state.db.0.lock().unwrap().execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",rusqlite::params![db::hash(&token),user,db::now()+60])?;
    let app=router(state.clone());
    let conversation=uuid::Uuid::new_v4();
    let id=uuid::Uuid::new_v4();
    let mut input=json!({"base":0,"task_id":id,"request_id":id,"conversation_id":conversation,"platform":"linux","messages":[{"role":"user","text":"世界".repeat(2000)}],"commands_enabled":false,"cwd":"/tmp"});
    let original=input.clone();
    for revision in 0..20 {
        input["base"]=json!(revision*2);
        input["request_id"]=if revision==0 { original["request_id"].clone() } else { json!(uuid::Uuid::new_v4()) };
        input["task_id"]=input["request_id"].clone();
        assert!(input.to_string().len()<16_000);
        let (status,reply)=request(&app,"/v1/chat",Some(&token),input.clone()).await?;
        assert_eq!(status,StatusCode::OK,"{reply}");
        assert_eq!(reply["revision"],(revision+1)*2);
    }
    assert!(serde_json::to_vec(&state.db.conversation(user,conversation)?.messages)?.len()>96_000);
    let (_,replay)=request(&app,"/v1/chat",Some(&token),original.clone()).await?;
    assert_eq!(replay["revision"],2);
    assert_eq!(state.db.balance(user)?,10);
    let mut stale=original;
    stale["request_id"]=json!(uuid::Uuid::new_v4());
    stale["task_id"]=stale["request_id"].clone();
    assert_eq!(request(&app,"/v1/chat",Some(&token),stale).await?.0,StatusCode::BAD_REQUEST);
    assert_eq!(state.db.balance(user)?,10);
    Ok(())
}

/// Steering is user-scoped and idempotent; Stop settles once and returns accepted input for recovery.
#[tokio::test]
async fn live_steering_and_stop_are_durable()->anyhow::Result<()> {
    let state=application()?;
    let user=state.db.user("control@example.test",None,5)?;
    let other=state.db.user("other@example.test",None,5)?;
    let token=db::secret();
    let other_token=db::secret();
    for (id,token) in [(user,&token),(other,&other_token)] {
        state.db.0.lock().unwrap().execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",rusqlite::params![db::hash(token),id,db::now()+60])?;
    }
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url=format!("http://{}",listener.local_addr()?);
    let app=router(state.clone());
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let id=uuid::Uuid::new_v4();
    let conversation=uuid::Uuid::new_v4();
    let input=json!({"base":0,"task_id":id,"request_id":id,"conversation_id":conversation,"platform":"linux","messages":[{"role":"user","text":"Long development reply. ".repeat(100)}],"commands_enabled":false,"cwd":"/tmp"});
    let http=reqwest::Client::new();
    let mut response=http.post(format!("{url}/v1/chat")).bearer_auth(&token).header("accept","text/event-stream").json(&input).send().await?;
    response.chunk().await?;
    let steering=json!({"id":uuid::Uuid::new_v4(),"text":"Remember 世界"});
    let endpoint=format!("{url}/v1/chat/{id}/steer");
    assert!(!http.post(&endpoint).bearer_auth(&other_token).json(&steering).send().await?.status().is_success());
    for _ in 0..2 { assert!(http.post(&endpoint).bearer_auth(&token).json(&steering).send().await?.status().is_success()); }
    assert_eq!(state.db.steering(user,id)?.len(),1);
    assert!(http.post(format!("{url}/v1/chat/{id}/stop")).bearer_auth(&token).send().await?.status().is_success());
    let result=tokio::time::timeout(Duration::from_secs(5),response.text()).await??;
    assert!(result.contains("Stopped by user") && result.contains("Remember 世界"));
    assert_eq!(state.db.balance(user)?,5);
    assert!(state.db.conversation(user,conversation)?.messages.is_empty());
    let replay=http.post(format!("{url}/v1/chat")).bearer_auth(&token).header("accept","text/event-stream").json(&input).send().await?.text().await?;
    assert!(replay.contains("Remember 世界") && replay.contains("\"settled\":true"));
    assert_eq!(state.db.balance(user)?,5);
    server.abort();
    Ok(())
}
