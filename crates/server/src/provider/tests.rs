mod parallel;
use super::*;
use clap::Parser;
use protocol::Message;
use serde_json::json;
use uuid::Uuid;

/// Fixtures and HTTP tests use the same conversation schema as the real client.
fn request() -> ChatRequest {
    let id=Uuid::new_v4();
    ChatRequest { task_id:id,base:0,request_id:id,conversation_id:Uuid::new_v4(),platform:Platform::Linux,
        messages:vec![Message { attachments:None, role:Role::User,text:"Hello 世界".into() }],commands_enabled:false,cwd:"/tmp".into() }
}

/// Persist a provider success exactly as the production HTTP handler does.
async fn turn(pool: &Pool, config: &Config, db: &Db, user: i64, request: &mut ChatRequest) -> Result<Answer> {
    db.reserve(user,request,&crate::db::hash(&serde_json::to_string(request)?),&config.pricing)?;
    let answer = pool.answer(config,db,user,request).await?;
    let completed=db.complete(user,request,&answer.reply,answer.account_home.as_deref(),answer.thread_id.as_deref(),answer.checkpoint.as_deref())?;
    request.base=completed.revision;
    request.messages.clear();
    request.messages.push(Message { attachments:None, role:Role::User,text:"Continue".into() });
    request.request_id = Uuid::new_v4();
    request.task_id=request.request_id;
    Ok(answer)
}

/// Write mock account state without any real credentials or network requests.
fn account(home: &std::path::Path, settings: Value) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(home.join("fixture.json"),serde_json::to_vec(&settings)?)?;
    Ok(())
}

/// Test both quota windows, expired resets, unrelated buckets and unknown allowance.
#[test]
fn ranks_by_bottleneck_and_rejects_unknown_quota() -> Result<()> {
    let future = now()+3600;
    let limits = Limits::read(&json!({"rateLimits":{"primary":{"usedPercent":10},"secondary":{"usedPercent":80}}}))?;
    assert_eq!(limits.used,80.0);
    assert_eq!(limits.retry_at,0);
    assert_eq!(Limits::read(&json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":now()+60},"secondary":{"usedPercent":100,"resetsAt":future}}}))?.retry_at,future);
    assert!(Limits::read(&json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":1}}}))?.retry_at>now());
    assert!(Limits::read(&json!({"rateLimitsByLimitId":{"other":{"primary":{"usedPercent":0}}}})).is_err());
    assert!(Limits::read(&json!({"rateLimits":{}})).is_err());
    Ok(())
}

/// UI-added credentials survive restart, API accounts skip subscription quotas, and pauses control failover.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_admin_account_controls()->Result<()> {
    crate::diagnostics::init_test();
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server"]); config.database=directory.path().join("test.db");
    let db=Arc::new(Db::open(&config.database)?);
    let mut pool=Pool::new(&config)?; pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into(); let pool=Arc::new(pool);
    let pending=pool.clone().add(db.clone(),None).await?;
    assert_eq!(pending["user_code"],"ABCD-1234");
    tokio::time::timeout(Duration::from_secs(10),async { while pool.login_status().await["finished"]!=true { tokio::time::sleep(Duration::from_millis(10)).await; } }).await?;
    assert!(pool.login_status().await["error"].is_null());
    let chatgpt=pool.account_status(&db)?[0]["id"].as_str().unwrap().to_owned();
    let key="sk-fixture-private-1234567890";
    pool.clone().add(db.clone(),Some(key.into())).await?;
    assert!(!crate::diagnostics::clean(key).contains(key));
    let accounts=pool.account_status(&db)?; assert_eq!(accounts.len(),2);
    let api=accounts.iter().find(|account|account["limits"]["authType"]=="apiKey").unwrap();
    let api_id=api["id"].as_str().unwrap().to_owned();
    let api_home=api["home"].as_str().unwrap().to_owned();
    assert!(!std::fs::read_to_string(std::path::Path::new(&api_home).join("events.jsonl"))?.contains("account/rateLimits/read"));
    pool.pause(&db,&chatgpt,true)?;
    let user=db.user("admin-account-controls@example.test",None,100)?; let mut chat=request();
    assert_eq!(turn(&pool,&config,&db,user,&mut chat).await?.account_home.as_deref(),Some(api_home.as_str()));
    account(std::path::Path::new(&api_home),json!({"auth_type":"apiKey","turn_error":"usageLimitExceeded"}))?;
    assert!(pool.answer(&config,&db,user,&request()).await.is_err());
    assert!(!std::fs::read_to_string(std::path::Path::new(&api_home).join("events.jsonl"))?.contains("account/rateLimits/read"));
    pool.pause(&db,&api_id,true)?;
    let error=pool.answer(&config,&db,user,&request()).await.err().unwrap(); assert!(error.to_string().contains("paused by operator"));
    pool.pause(&db,&chatgpt,false)?;
    assert_ne!(turn(&pool,&config,&db,user,&mut chat).await?.account_home.as_deref(),Some(api_home.as_str()));
    drop(pool);
    let reloaded=Pool::new(&config)?;
    assert_eq!(reloaded.account_status(&db)?.iter().find(|account|account["id"]==api_id).unwrap()["paused"],true);
    assert_eq!(reloaded.account_status(&db)?.iter().find(|account|account["id"]==chatgpt).unwrap()["paused"],false);
    Ok(())
}

/// Preserve quota and generation causes, redact credentials, and record recovery without a model call.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_diagnostics_preserve_causes()->Result<()> {
    crate::diagnostics::init_test();
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server"]);
    config.codex_homes.push(directory.path().join("diagnostic-account"));
    let home=&config.codex_homes[0];
    account(home,json!({"primary":23,"rpc_error_method":"account/rateLimits/read","error_message":"HTTP 401: authentication expired; Bearer PRIVATE_TOKEN"}))?;
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    pool.probe(&db,0).await?;
    let status=pool.account_status(&db)?;
    let error=status[0]["error"].as_str().unwrap();
    assert!(error.contains("account/rateLimits/read") && error.contains("HTTP 401"));
    assert!(!error.contains("PRIVATE_TOKEN"));
    account(home,json!({"primary":23,"turn_error":"unexpectedServerError","error_message":"Upstream failed 世界; refresh_token=PRIVATE_TOKEN"}))?;
    pool.probe(&db,0).await?;
    assert!(pool.account_status(&db)?[0]["error"].is_null());
    let user=db.user("diagnostic@example.test",None,100)?;
    let error=pool.answer(&config,&db,user,&request()).await.err().unwrap();
    assert!(error.to_string().contains("unexpectedServerError"));
    let stored:String=db.0.lock().unwrap().query_row("SELECT error FROM usage ORDER BY id DESC LIMIT 1",[],|row|row.get(0))?;
    assert!(stored.contains("Upstream failed 世界") && !stored.contains("PRIVATE_TOKEN"));
    let filter=serde_json::from_value(json!({"search":home.to_string_lossy()}))?;
    let logs=crate::diagnostics::list(axum::extract::Query(filter)).await.map_err(|_|anyhow::anyhow!("Cannot read diagnostics"))?.0;
    assert!(logs.to_string().contains("Quota refresh recovered"));
    assert!(logs.to_string().contains("HTTP 401") && !logs.to_string().contains("PRIVATE_TOKEN"));
    Ok(())
}

/// Exercise early notifications and real JSONL deltas without forwarding private reasoning or JSON syntax.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_streams_before_completion() -> Result<()> {
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server"]);
    config.codex_homes.push(directory.path().join("stream"));
    account(&config.codex_homes[0],json!({"primary":0,"stream":true}))?;
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("stream@example.test",None,10)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let chat=request();
    let (sender,mut receiver)=tokio::sync::watch::channel(protocol::ChatEvent::Progress { progress:Default::default() });
    let answer=pool.answer_stream(&config,&db,user,&chat,Some(&sender),None);
    tokio::pin!(answer);
    let (mut early_summary,mut partial)=(false,false);
    loop {
        tokio::select! {
            result=&mut answer => {
                assert_eq!(result?.reply.text,"Fixture reply from stream");
                assert!(early_summary && partial);
                break;
            }
            changed=receiver.changed() => {
                changed?;
                let event=receiver.borrow_and_update().clone();
                assert!(!serde_json::to_string(&event)?.contains("PRIVATE_RAW_REASONING"));
                if let protocol::ChatEvent::Progress { progress }=event {
                    early_summary|=progress.summaries.iter().any(|part|part.text.contains("before acknowledgement"));
                    partial|=progress.text=="Fixture";
                    assert!("Fixture reply from stream".starts_with(&progress.text));
                }
            }
        }
    }
    assert_eq!(db.0.lock().unwrap().query_row("SELECT total_tokens FROM usage",[],|r|r.get::<_,i64>(0))?,110);
    Ok(())
}

/// A backend on either platform must propose commands for the client's shell.
#[test]
fn demo_uses_client_platform() {
    for (platform,prefix) in [(Platform::Linux,"printf"),(Platform::Windows,"Write-Output")] {
        let mut request = request();
        request.platform=platform;
        request.commands_enabled=true;
        request.messages[0].text="demo command".into();
        assert!(demo(&request).command.unwrap().command.starts_with(prefix));
        request.commands_enabled=false;
        assert!(demo(&request).command.is_none());
    }
}

/// Verify least-used selection, sticky threads, restart persistence, user isolation and preflight failover.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_persistent_account_affinity() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut config = Config::parse_from(["assistant-server"]);
    for (name,primary,secondary) in [("a",70,10),("b",10,80),("c",40,40)] {
        let home=directory.path().join(name);
        account(&home,json!({"primary":primary,"secondary":secondary}))?;
        config.codex_homes.push(home);
    }
    let db_path=directory.path().join("test.db");
    let db=Db::open(&db_path)?;
    let user=db.user("first@example.test",None,20)?;
    let other=db.user("second@example.test",None,20)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let mut chat=request();
    let first=turn(&pool,&config,&db,user,&mut chat).await?;
    assert_eq!(first.account_home.as_deref(),Some(pool.accounts()[2].home.as_str()));
    account(&config.codex_homes[2],json!({"primary":90,"secondary":90}))?;
    pool.accounts()[2].limits.lock().await.as_mut().unwrap().checked_at=now()-301;
    let second=turn(&pool,&config,&db,user,&mut chat).await?;
    assert_eq!(first.thread_id,second.thread_id);
    let events=std::fs::read_to_string(config.codex_homes[2].join("events.jsonl"))?;
    let turns: Vec<Value>=events.lines().map(serde_json::from_str).collect::<std::result::Result<_,_>>()?;
    let last=turns.iter().rev().find(|e|e["method"]=="turn/start").unwrap();
    assert_eq!(last["params"]["environments"],json!([]));
    assert!(last["params"]["effort"].is_null());
    let input: Value=serde_json::from_str(last["params"]["input"][0]["text"].as_str().unwrap())?;
    assert_eq!(input["messages"].as_array().unwrap().len(),1);
    let mut other_chat=request();
    other_chat.conversation_id=chat.conversation_id;
    let isolated=turn(&pool,&config,&db,other,&mut other_chat).await?;
    assert_ne!(isolated.thread_id,first.thread_id);
    assert_ne!(isolated.account_home,first.account_home);
    drop(pool);
    drop(db);
    config.codex_homes.reverse();
    let db=Db::open(&db_path)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let resumed=turn(&pool,&config,&db,user,&mut chat).await?;
    assert_eq!(resumed.thread_id,first.thread_id);
    assert_eq!(resumed.account_home,first.account_home);
    account(&config.codex_homes[0],json!({"primary":100,"secondary":90}))?;
    // Selection refreshes the provider only after the five-minute quota cache expires.
    pool.accounts()[0].limits.lock().await.as_mut().unwrap().checked_at=now()-301;
    let moved=turn(&pool,&config,&db,user,&mut chat).await?;
    assert_ne!(moved.account_home,first.account_home);
    assert_ne!(moved.thread_id,first.thread_id);
    let events=std::fs::read_to_string(std::path::Path::new(moved.account_home.as_ref().unwrap()).join("events.jsonl"))?;
    let last: Value=serde_json::from_str(events.lines().rev().find(|line|line.contains("\"method\":\"turn/start\"")).unwrap())?;
    let input: Value=serde_json::from_str(last["params"]["input"][0]["text"].as_str().unwrap())?;
    assert_eq!(input["messages"].as_array().unwrap().len(),1);
    assert!(!input["recovery"]["messages"].as_array().unwrap().is_empty());
    Ok(())
}

/// Confirm typed mid-turn limits trigger failover, while unrelated errors keep account affinity.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_mid_turn_limit_and_errors() -> Result<()> {
    for error in ["usageLimitExceeded","unexpectedServerError"] {
        let directory=tempfile::tempdir()?;
        let mut config=Config::parse_from(["assistant-server"]);
        for (name,used) in [("first",0),("second",50)] {
            let home=directory.path().join(name);
            account(&home,json!({"primary":used}))?;
            config.codex_homes.push(home);
        }
        let db=Db::open(std::path::Path::new(":memory:"))?;
        let user=db.user("test@example.test",None,10)?;
        let mut pool=Pool::new(&config)?;
        pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
        let mut chat=request();
        let initial=turn(&pool,&config,&db,user,&mut chat).await?;
        account(&config.codex_homes[0],json!({"primary":0,"turn_error":error}))?;
        let result=pool.answer(&config,&db,user,&chat).await;
        if error=="usageLimitExceeded" {
            let next=result?;
            assert_ne!(next.account_home,initial.account_home);
            assert!(pool.accounts()[0].limits.lock().await.unwrap().retry_at>now());
        } else {
            assert!(result.is_err());
            let session=db.conversation(user,chat.conversation_id)?;
            assert_eq!(session.account_home,initial.account_home);
            assert!(session.thread_id.is_none());
        }
    }
    Ok(())
}

/// Both workers must reach a shared barrier, proving generations are not serialized across accounts.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_accounts_run_in_parallel() -> Result<()> {
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server"]);
    for name in ["a","b"] {
        let home=directory.path().join(name);
        account(&home,json!({"primary":0,"barrier":directory.path().to_string_lossy()}))?;
        config.codex_homes.push(home);
    }
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("parallel@example.test",None,10)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let a=request();
    let b=request();
    let (a,b)=tokio::join!(pool.answer(&config,&db,user,&a),pool.answer(&config,&db,user,&b));
    assert_ne!(a?.account_home,b?.account_home);
    Ok(())
}

/// Exercise real thread persistence and turn JSON against a local model stub, without credentials or usage.
#[tokio::test]
#[ignore = "Build assistant-server and set ASSISTANT_TEST_SERVER to its executable"]
async fn integration_embedded_runtime_contract() -> Result<()> {
    let directory=tempfile::tempdir()?;
    let executable=PathBuf::from(std::env::var("ASSISTANT_TEST_SERVER")?);
    let mut rpc=tokio::time::timeout(Duration::from_secs(40),Rpc::start_unauthenticated(&executable,&directory.path().to_string_lossy())).await??;
    assert!(rpc.call("account/read",json!({"refreshToken":false})).await?["account"].is_null());
    let config=rpc.call("config/read",json!({"includeLayers":false})).await?;
    assert_eq!(config["config"]["features"]["shell_tool"],false);
    assert_eq!(config["config"]["features"]["unified_exec"],false);
    assert_eq!(config["config"]["features"]["plugins"],false);
    assert_eq!(config["config"]["web_search"],"disabled");
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint=format!("http://{}/v1",listener.local_addr()?);
    let requests=std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let app=axum::Router::new().route("/v1/responses",axum::routing::post(|axum::extract::State(requests):axum::extract::State<std::sync::Arc<std::sync::Mutex<Vec<Value>>>>,axum::Json(body):axum::Json<Value>|async move {
        let mut answer=json!({"text":"Embedded reply","command":null});
        if body.to_string().contains("\"checkpoint\"") { answer["checkpoint"]=json!("Embedded recovery checkpoint"); }
        requests.lock().unwrap().push(body);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events=[json!({"type":"response.created","response":{"id":"fixture-response"}}),
            json!({"type":"response.output_item.done","item":{"id":"fixture-message","type":"message","role":"assistant",
                "content":[{"type":"output_text","text":answer.to_string()}]}}),
            json!({"type":"response.completed","response":{"id":"fixture-response",
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}})];
        ([("content-type","text/event-stream")],events.iter().map(|event|format!("event: {}\ndata: {event}\n\n",event["type"].as_str().unwrap())).collect::<String>())
    })).with_state(requests.clone());
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let provider=json!({"model_providers.fixture":{"name":"Local integration fixture","base_url":endpoint,"wire_api":"responses",
        "requires_openai_auth":false,"request_max_retries":0,"stream_max_retries":0}});
    let thread=tokio::time::timeout(Duration::from_secs(40),rpc.call("thread/start",json!({"model":"fixture-model","modelProvider":"fixture",
        "config":provider,"environments":[],"ephemeral":false,"sandbox":"read-only","approvalPolicy":"never"}))).await??;
    let id=thread["thread"]["id"].as_str().unwrap();
    let mut chat=request();
    let settings=Config::parse_from(["assistant-server","--reasoning-effort","high"]);
    // New threads are not resumable until their first turn has created a rollout.
    rpc.call("turn/start",json!({"threadId":id,"environments":[],"input":[{"type":"text","text":"Hello"}],"outputSchema":protocol::reply_schema()})).await?;
    let reply=tokio::time::timeout(Duration::from_secs(40),async {
        let mut text=String::new();
        loop {
            let event=rpc.next().await?;
            if event["method"]=="item/completed" && event["params"]["item"]["type"]=="agentMessage" {
                text=event["params"]["item"]["text"].as_str().unwrap().to_owned();
            }
            if event["method"]=="turn/completed" {
                ensure!(event["params"]["turn"]["status"]=="completed","Local model fixture turn failed: {event}");
                return Ok::<AssistantReply,anyhow::Error>(serde_json::from_str(&text)?);
            }
        }
    }).await??;
    assert_eq!(reply.text,"Embedded reply");
    // Unsubscribe flushes and unloads the thread before restarting the real worker.
    rpc.call("thread/unsubscribe",json!({"threadId":id})).await?;
    drop(rpc);
    let mut rpc=Rpc::start_unauthenticated(&executable,&directory.path().to_string_lossy()).await?;
    rpc.call("thread/resume",json!({"threadId":id,"modelProvider":"fixture","config":provider,"excludeTurns":true})).await?;
    let image="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    chat.messages=vec![Message { attachments:Some(vec![protocol::Attachment::Image { name:"screen.png".into(),content:image.into() },protocol::Attachment::Text { name:"日志.txt".into(),content:"ATTACHED_TEXT 世界".into() }]),role:Role::User,text:"Continue".into() }];
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let mut attempt=crate::usage::Attempt::start(&db,None,&chat,"embedded-fixture","admin_test")?;
    let user=db.user("native-steer@example.test",None,5)?;
    db.reserve(user,&chat,"native",&settings.pricing)?;
    let (sender,mut receiver)=tokio::sync::mpsc::channel(4);
    let (ack,result)=tokio::sync::oneshot::channel();
    sender.send(crate::turns::Control::Steer(protocol::Steering { attachments:None, id:Uuid::new_v4(),text:"NATIVE_STEER 世界".into() },ack)).await?;
    let (reply,returned,checkpoint)=tokio::time::timeout(Duration::from_secs(40),rpc.answer(&settings,&chat,Some(id),&Value::Null,true,Some(user),Some(&mut receiver),&attempt,&mut progress::Reporter::new(None,1))).await??;
    assert!(result.await?.is_ok());
    assert_eq!(db.steering(user,chat.request_id)?.len(),1);
    attempt.finish(None)?;
    assert!(db.0.lock().unwrap().query_row("SELECT total_tokens FROM usage",[],|r|r.get::<_,i64>(0))?>=2);
    assert_eq!(reply.text,"Embedded reply");
    assert_eq!(returned,id);
    assert_eq!(checkpoint.as_deref(),Some("Embedded recovery checkpoint"));
    assert_eq!(requests.lock().unwrap().last().unwrap()["reasoning"]["effort"],"high");
    assert!(requests.lock().unwrap().iter().any(|request|request["input"].as_array().unwrap().iter().any(|item|item["content"].as_array().is_some_and(|content|content.iter().any(|part|part["type"]=="input_image" && part["image_url"]==image)))));
    assert!(requests.lock().unwrap().iter().any(|request|request.to_string().contains("ATTACHED_TEXT")));
    rpc.call("thread/compact/start",json!({"threadId":id})).await?;
    tokio::time::timeout(Duration::from_secs(40),async {
        let mut compacted=false;
        loop {
            let event=rpc.next().await?;
            if event["method"]=="item/completed" && event["params"]["item"]["type"]=="contextCompaction" { compacted=true; }
            if compacted && event["method"]=="turn/completed" { return Ok::<_,anyhow::Error>(()); }
            if event["method"]=="error" { anyhow::bail!("Compaction failed: {event}"); }
        }
    }).await??;
    let (sender,mut receiver)=tokio::sync::mpsc::channel(4);
    let (ack,result)=tokio::sync::oneshot::channel();
    sender.send(crate::turns::Control::Stop(ack)).await?;
    let stopped=tokio::time::timeout(Duration::from_secs(40),rpc.answer(&settings,&chat,Some(id),&Value::Null,false,Some(user),Some(&mut receiver),&attempt,&mut progress::Reporter::new(None,1))).await?;
    assert!(stopped.err().unwrap().downcast_ref::<crate::turns::Interrupted>().is_some());
    result.await?.map_err(anyhow::Error::msg)?;
    server.abort();
    Ok(())
}

/// Record retries and resumed turns once, preserve unknown usage, and keep admin tests on the chosen account.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_usage_and_admin_tests() -> Result<()> {
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server","--reasoning-effort","medium"]);
    for name in ["first","second"] {
        let home=directory.path().join(name);
        account(&home,json!({"primary":10}))?;
        config.codex_homes.push(home);
    }
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("usage@example.test",None,10)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let mut chat=request();
    turn(&pool,&config,&db,user,&mut chat).await?;
    turn(&pool,&config,&db,user,&mut chat).await?;
    let events=std::fs::read_to_string(config.codex_homes[0].join("events.jsonl"))?;
    assert_eq!(events.lines().filter(|line|line.contains("account/rateLimits/read")).count(),1);
    assert_eq!(db.0.lock().unwrap().query_row("SELECT SUM(total_tokens) FROM usage",[],|r|r.get::<_,i64>(0))?,220);
    let result=pool.test_account(&config,&db,1,"Say OK".into()).await?;
    assert!(result["text"].as_str().unwrap().contains("second"));
    let events=std::fs::read_to_string(config.codex_homes[1].join("events.jsonl"))?;
    let test:Value=serde_json::from_str(events.lines().find(|line|line.contains("\"method\":\"turn/start\"")).unwrap())?;
    assert_eq!(test["params"]["effort"],"medium");
    assert_eq!(db.balance(user)?,8);
    assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE user_id IS NULL AND purpose='admin_test' AND total_tokens=110",[],|r|r.get::<_,i64>(0))?,1);
    account(&config.codex_homes[0],json!({"primary":10,"turn_error":"usageLimitExceeded"}))?;
    assert!(pool.test_account(&config,&db,0,"Say OK".into()).await.is_err());
    assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE error IS NOT NULL AND total_tokens=110",[],|r|r.get::<_,i64>(0))?,1);
    account(&config.codex_homes[0],json!({"primary":10,"missing_usage":true}))?;
    pool.test_account(&config,&db,0,"Say OK".into()).await?;
    assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM usage WHERE total_tokens IS NULL AND finished_at IS NOT NULL",[],|r|r.get::<_,i64>(0))?,1);
    // Read-only dashboard refreshes reuse stored readings instead of asking ChatGPT again.
    pool.probe(&db,1).await?;
    let before=std::fs::read_to_string(config.codex_homes[1].join("events.jsonl"))?;
    assert_eq!(pool.account_status(&db)?[1]["email"],"fixture@example.test");
    pool.account_status(&db)?;
    assert_eq!(before,std::fs::read_to_string(config.codex_homes[1].join("events.jsonl"))?);
    let account=pool.accounts()[1].clone();
    let _busy=account.worker.lock().await;
    pool.probe(&db,1).await?;
    assert_eq!(pool.account_status(&db)?[1]["busy"],false);
    assert_eq!(before,std::fs::read_to_string(config.codex_homes[1].join("events.jsonl"))?);
    Ok(())
}

/// Repeated checkpoints bound failover input while the full application transcript remains available.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_long_chat_checkpoint_and_failover() -> Result<()> {
    let directory=tempfile::tempdir()?;
    let mut config=Config::parse_from(["assistant-server"]);
    for (name,used) in [("a",0),("b",50)] {
        let home=directory.path().join(name);
        account(&home,json!({"primary":used}))?;
        config.codex_homes.push(home);
    }
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("checkpoint@example.test",None,30)?;
    let mut pool=Pool::new(&config)?;
    pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let mut chat=request();
    for _ in 0..16 {
        chat.messages=vec![Message { attachments:None, role:Role::User,text:format!("IMPORTANT_GOAL {}", "世界".repeat(2000)) }];
        turn(&pool,&config,&db,user,&mut chat).await?;
    }
    let saved=db.conversation(user,chat.conversation_id)?;
    assert!(serde_json::to_vec(&saved.messages)?.len()>96_000);
    assert!(saved.checkpoint.contains("IMPORTANT_GOAL") && saved.checkpoint_at>0);
    assert!(serde_json::to_vec(&saved.messages[saved.checkpoint_at..])?.len()<64_000);
    account(&config.codex_homes[0],json!({"primary":100}))?;
    pool.accounts()[0].limits.lock().await.as_mut().unwrap().checked_at=now()-301;
    let moved=turn(&pool,&config,&db,user,&mut chat).await?;
    assert_eq!(moved.account_home.as_deref(),Some(pool.accounts()[1].home.as_str()));
    let events=std::fs::read_to_string(config.codex_homes[1].join("events.jsonl"))?;
    let event:Value=serde_json::from_str(events.lines().find(|line|line.contains("\"method\":\"turn/start\"")).unwrap())?;
    let input=event["params"]["input"][0]["text"].as_str().unwrap();
    assert!(input.len()<80_000 && input.contains("Fixture recovery summary"));
    assert_eq!(db.conversation(user,chat.conversation_id)?.messages.len(),34);
    Ok(())
}

/// Exercise turn/steer and turn/interrupt through the JSONL driver, including expectedTurnId.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_provider_controls() -> Result<()> {
    for stop in [false,true] {
        let directory=tempfile::tempdir()?;
        let mut config=Config::parse_from(["assistant-server"]);
        config.codex_homes.push(directory.path().join("held"));
        account(&config.codex_homes[0],json!({"primary":0,"hold":true,"fragment":true}))?;
        let db=Db::open(std::path::Path::new(":memory:"))?;
        let user=db.user("controls@example.test",None,5)?;
        let chat=request();
        db.reserve(user,&chat,"control",&config.pricing)?;
        let mut pool=Pool::new(&config)?;
        pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
        let (sender,mut receiver)=tokio::sync::mpsc::channel(4);
        let controller=async {
            // Interrupt a read halfway through a multibyte JSONL event, as a real stream can arrive.
            while !config.codex_homes[0].join("fragment-ready").exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
            tokio::time::sleep(Duration::from_millis(50)).await;
            // Reject controls whose balance changed after admission, before forwarding anything to Codex.
            if !stop {
                for balance in [0,-1] {
                    db.0.lock().unwrap().execute("UPDATE ledger SET delta=?1 WHERE user_id=?2",rusqlite::params![balance,user]).unwrap();
                    let (reply,ack)=tokio::sync::oneshot::channel();
                    sender.send(crate::turns::Control::Steer(protocol::Steering { attachments:None, id:Uuid::new_v4(),text:"Must not reach Codex".into() },reply)).await.unwrap();
                    assert!(ack.await.unwrap().unwrap_err().contains("Add credits to steer"));
                    assert!(db.steering(user,chat.request_id).unwrap().is_empty());
                }
                db.0.lock().unwrap().execute("UPDATE ledger SET delta=5 WHERE user_id=?1",[user]).unwrap();
            }
            let (reply,ack)=tokio::sync::oneshot::channel();
            let control=if stop { crate::turns::Control::Stop(reply) } else { crate::turns::Control::Steer(protocol::Steering { attachments:None, id:Uuid::new_v4(),text:"Switch to 世界".into() },reply) };
            sender.send(control).await.unwrap();
            assert!(ack.await.unwrap().is_ok());
        };
        let (result,_)=tokio::time::timeout(Duration::from_secs(15),async {
            tokio::join!(pool.answer_stream(&config,&db,user,&chat,None,Some(&mut receiver)),controller)
        }).await?;
        if stop { assert!(result.err().unwrap().downcast_ref::<crate::turns::Interrupted>().is_some()); }
        else { assert_eq!(result?.reply.text,"Steered: Switch to 世界"); assert_eq!(db.steering(user,chat.request_id)?.len(),1); }
    }
    Ok(())
}
