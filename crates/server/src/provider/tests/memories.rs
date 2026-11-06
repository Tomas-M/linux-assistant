use super::*;

/// A real JSONL worker requests memory updates while local execution is disabled; no AI service is contacted.
#[tokio::test]
#[ignore = "Build provider-fixture and set ASSISTANT_TEST_PROVIDER to its executable"]
async fn integration_memories_use_the_request_user_and_survive_thread_resume()->Result<()> {
    let directory=tempfile::tempdir()?;
    let home=directory.path().join("account");
    account(&home,json!({"primary":0,"memory_roundtrip":true}))?;
    let config=Config { database:directory.path().join("memory.db"),test_homes:vec![home.clone()],..fixture_config(&directory) };
    let db=Db::open(&config.database)?;
    let first=db.user("first-memory@example.test",None,100)?;
    let second=db.user("second-memory@example.test",None,100)?;
    db.0.lock().unwrap().execute("INSERT INTO user_memories VALUES(?1,'PRIVATE_FIRST_PREFERENCE','seed-first'),(?2,'PRIVATE_SECOND_PREFERENCE','seed-second')",rusqlite::params![first,second])?;
    let mut pool=Pool::new(&config)?; pool.executable=std::env::var("ASSISTANT_TEST_PROVIDER")?.into();
    let mut chat=request(); chat.messages[0].text="Verified audio fix on the first computer".into();
    let answer=tokio::time::timeout(Duration::from_secs(10),turn(&pool,&config,&db,first,&mut chat)).await??;
    assert_eq!(answer.reply.text,"Task completed and memories saved");
    let remembered=crate::memories::read(&db,first)?;
    assert!(remembered.text.contains("PRIVATE_FIRST_PREFERENCE") && remembered.text.contains("Verified audio fix"));
    assert!(!remembered.text.contains("PRIVATE_SECOND_PREFERENCE"));
    let mut other=request(); other.messages[0].text="Verified display fix on the second computer".into();
    tokio::time::timeout(Duration::from_secs(10),turn(&pool,&config,&db,second,&mut other)).await??;
    let remembered_other=crate::memories::read(&db,second)?;
    assert!(remembered_other.text.contains("PRIVATE_SECOND_PREFERENCE") && remembered_other.text.contains("Verified display fix"));
    assert!(!remembered_other.text.contains("PRIVATE_FIRST_PREFERENCE") && !remembered_other.text.contains("Verified audio fix"));
    account(&home,json!({"primary":0,"memory_roundtrip":true,"memory_replace":true,"memory_text":"Consolidated verified result 世界"}))?;
    tokio::time::timeout(Duration::from_secs(10),turn(&pool,&config,&db,first,&mut chat)).await??;
    assert_eq!(crate::memories::read(&db,first)?.text,"Consolidated verified result 世界");
    assert_eq!(crate::memories::read(&db,second)?.text,remembered_other.text);
    assert_eq!(db.balance(first)?,98); assert_eq!(db.balance(second)?,99);
    let events=std::fs::read_to_string(home.join("events.jsonl"))?.lines().map(serde_json::from_str::<Value>).collect::<std::result::Result<Vec<_>,_>>()?;
    let starts=events.iter().filter(|event|event["method"]=="thread/start").collect::<Vec<_>>();
    assert_eq!(starts.len(),2);
    for event in &starts {
        let tools=event["params"]["dynamicTools"].as_array().unwrap();
        assert!(tools.iter().any(|tool|tool["name"]=="memory_read"));
        assert!(tools.iter().any(|tool|tool["name"]=="memory_save"));
        assert!(!event["params"]["baseInstructions"].as_str().unwrap().contains("PRIVATE_"));
    }
    let inputs=events.iter().filter(|event|event["method"]=="turn/start").map(|event|serde_json::from_str::<Value>(event["params"]["input"][0]["text"].as_str().unwrap())).collect::<std::result::Result<Vec<_>,_>>()?;
    assert_eq!(inputs[0]["memories"]["text"],"PRIVATE_FIRST_PREFERENCE");
    assert_eq!(inputs[1]["memories"]["text"],"PRIVATE_SECOND_PREFERENCE");
    assert!(inputs[2]["memories"].is_null());
    assert_eq!(inputs[2]["memory_revision"],remembered.revision);
    pool.shutdown().await?;
    Ok(())
}
