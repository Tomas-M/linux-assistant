use super::*;
use std::sync::Arc;

/// One account/worker supports overlapping users, admin tests and probes; Stop/cancellation remain scoped.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn same_account_conversations_are_isolated()->Result<()> {
    let directory=tempfile::tempdir()?;
    let mut config=Config::default();
    config.test_homes.push(directory.path().join("shared"));
    account(&config.test_homes[0],json!({"primary":10,"hold":true}))?;
    let db=Arc::new(Db::open(std::path::Path::new(":memory:"))?);
    let user=db.user("two-computers@example.test",None,20)?;
    let other=db.user("other@example.test",None,20)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let pool=Arc::new(pool);
    let mut tasks=Vec::new();
    let mut controls=Vec::new();
    let mut chats=Vec::new();
    for (index,user) in [user,user,other].into_iter().enumerate() {
        let mut chat=request();
        chat.messages[0].text=format!("Conversation {index} 世界");
        db.reserve(user,&chat,"parallel",&config.pricing)?;
        let (sender,mut receiver)=tokio::sync::mpsc::channel(4);
        controls.push(sender);
        chats.push(chat.clone());
        let (pool,db,config)=(pool.clone(),db.clone(),config.clone());
        tasks.push(tokio::spawn(async move { pool.answer_stream(&config,&db,user,&chat,None,Some(&mut receiver)).await }));
    }
    tokio::time::timeout(Duration::from_secs(10),async {
        while db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE turn_id IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap()<3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await?;
    assert_eq!(pool.account_status(&db)?[0]["active_requests"],3);
    // Force a due quota refresh during three active generations, then exercise the same account's test UI.
    pool.accounts()[0].limits.lock().await.as_mut().unwrap().checked_at=now()-301;
    pool.probe(&db,0).await?;
    assert!(pool.test_account(&config,&db,0,"ADMIN_QUICK".into()).await?["text"].as_str().unwrap().contains("shared"));
    tasks[2].abort();
    assert!(matches!(tasks.pop().unwrap().await,Err(error) if error.is_cancelled()));
    let (reply,ack)=tokio::sync::oneshot::channel();
    controls[1].send(crate::turns::Control::Stop(reply)).await?;
    ack.await?.map_err(anyhow::Error::msg)?;
    let stopped=tokio::time::timeout(Duration::from_secs(5),tasks.pop().unwrap()).await??;
    assert!(stopped.err().unwrap().downcast_ref::<crate::turns::Interrupted>().is_some());
    assert!(!tasks[0].is_finished());
    let (reply,ack)=tokio::sync::oneshot::channel();
    controls[0].send(crate::turns::Control::Steer(protocol::Steering { attachments:None, id:Uuid::new_v4(),text:"Only the first conversation 世界".into() },reply)).await?;
    ack.await?.map_err(anyhow::Error::msg)?;
    let answer=tokio::time::timeout(Duration::from_secs(5),tasks.pop().unwrap()).await???;
    assert_eq!(answer.reply.text,"Steered: Only the first conversation 世界");
    assert_eq!(db.steering(user,chats[0].request_id)?.len(),1);
    assert!(db.steering(user,chats[1].request_id)?.is_empty());
    assert!(db.steering(other,chats[2].request_id)?.is_empty());
    let events=std::fs::read_to_string(config.test_homes[0].join("events.jsonl"))?;
    assert_eq!(events.lines().filter(|line|line.contains("\"method\":\"initialize\"")).count(),1);
    assert_eq!(events.lines().filter(|line|line.contains("account/rateLimits/read")).count(),2);
    let input:Value=events.lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).find(|event|event["method"]=="turn/start" && event["params"]["input"][0]["text"].as_str().unwrap().contains("Conversation 0")).unwrap();
    let weight=input["params"]["input"][0]["text"].as_str().unwrap().len() as i64;
    let measured=db.0.lock().unwrap().query_row("SELECT input_tokens,cached_tokens,output_tokens FROM usage WHERE user_id=?1 AND request_id=?2",rusqlite::params![user,chats[0].request_id.to_string()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?)))?;
    assert_eq!(measured,(weight,weight/4,10));
    assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE user_id IS NULL AND purpose='admin_test' AND total_tokens=110",[],|r|r.get::<_,i64>(0))?,1);
    Ok(())
}

/// The unmodified embedded App Server must reach two HTTP model requests before either is released.
#[tokio::test]
#[ignore = "Build assistant-server and set ASSISTANT_TEST_SERVER to its executable"]
async fn embedded_account_runs_parallel_threads()->Result<()> {
    let directory=tempfile::tempdir()?;
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint=format!("http://{}/v1",listener.local_addr()?);
    let app=axum::Router::new().route("/v1/responses",axum::routing::post(|axum::extract::State(barrier):axum::extract::State<Arc<tokio::sync::Barrier>>,axum::Json(body):axum::Json<Value>|async move {
        tokio::time::timeout(Duration::from_secs(15),barrier.wait()).await.expect("Native turns were serialized");
        let text=if body.to_string().contains("FIRST_THREAD") { "FIRST_THREAD" } else { "SECOND_THREAD" };
        let events=[json!({"type":"response.created","response":{"id":text}}),
            json!({"type":"response.output_item.done","item":{"id":text,"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"text":text,"command":null}).to_string()}]}}),
            json!({"type":"response.completed","response":{"id":text,"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}})];
        ([("content-type","text/event-stream")],events.iter().map(|event|format!("event: {}\ndata: {event}\n\n",event["type"].as_str().unwrap())).collect::<String>())
    })).with_state(Arc::new(tokio::sync::Barrier::new(2)));
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let rpc=Rpc::start_unauthenticated(&PathBuf::from(std::env::var("ASSISTANT_TEST_SERVER")?),&directory.path().to_string_lossy()).await?;
    let mut tasks=tokio::task::JoinSet::new();
    for name in ["FIRST_THREAD","SECOND_THREAD"] {
        let mut rpc=rpc.session();
        let endpoint=endpoint.clone();
        tasks.spawn(async move {
            let thread=rpc.call("thread/start",json!({"model":"fixture-model","modelProvider":"fixture","ephemeral":true,"environments":[],
                "config":{"model_providers.fixture":{"name":"Local fixture","base_url":endpoint,"wire_api":"responses","requires_openai_auth":false,"request_max_retries":0,"stream_max_retries":0}},
                "sandbox":"read-only","approvalPolicy":"never"})).await?;
            let id=thread["thread"]["id"].as_str().unwrap();
            // Reject duplicate subscriptions without unsubscribing the original conversation on drop.
            let mut duplicate=rpc.session();
            assert!(duplicate.call("thread/resume",json!({"threadId":id})).await.unwrap_err().to_string().contains("already active"));
            drop(duplicate);
            rpc.call("turn/start",json!({"threadId":id,"environments":[],"input":[{"type":"text","text":name}],"outputSchema":protocol::reply_schema()})).await?;
            let mut answer=String::new();
            loop {
                let event=rpc.next().await?;
                assert_eq!(event["params"]["threadId"],id);
                if event["method"]=="item/completed" && event["params"]["item"]["type"]=="agentMessage" { answer=event["params"]["item"]["text"].as_str().unwrap().into(); }
                if event["method"]=="turn/completed" {
                    assert_eq!(event["params"]["turn"]["status"],"completed");
                    assert_eq!(serde_json::from_str::<Value>(&answer)?["text"],name);
                    return Ok::<_,anyhow::Error>(());
                }
            }
        });
    }
    tokio::time::timeout(Duration::from_secs(45),async { while let Some(result)=tasks.join_next().await { result??; } Ok::<_,anyhow::Error>(()) }).await??;
    server.abort();
    Ok(())
}
