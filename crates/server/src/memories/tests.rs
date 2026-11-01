use super::*;
use axum::{body::{Body,to_bytes},http::{Request,header},Router};
use tower::ServiceExt;

/// Issue isolated user sessions without sending email or contacting a provider.
fn login(app:&App,email:&str)->anyhow::Result<(i64,String)> {
    let user=app.db.user(email,None,0)?; let token=crate::db::secret();
    app.db.0.lock().unwrap().execute("INSERT INTO sessions VALUES(?1,?2,?3)",params![crate::db::hash(&token),user,crate::db::now()+3600])?;
    Ok((user,token))
}

/// Exercise the real authentication, JSON decoding and memory endpoints together.
async fn request(app:&Router,token:Option<&str>,body:Option<Value>)->anyhow::Result<(StatusCode,Value)> {
    let mut request=Request::builder().uri("/v1/memories").header(header::CONTENT_TYPE,"application/json");
    if let Some(token)=token { request=request.header(header::AUTHORIZATION,format!("Bearer {token}")); }
    if body.is_some() { request=request.method("POST"); }
    let response=app.clone().oneshot(request.body(body.map_or_else(Body::empty,|body|Body::from(body.to_string())))?).await?;
    let status=response.status();
    let bytes=to_bytes(response.into_body(),64*1024).await?;
    Ok((status,serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

/// Simulate the provider's tool request, with user identity supplied only by the backend.
fn invoke(db:&Db,user:Option<i64>,name:&str,arguments:Value)->Value {
    tool(&json!({"id":"memory-test","method":"item/tool/call","params":{"namespace":null,"tool":name,"arguments":arguments}}),db,user).unwrap()
}

/// Manual updates share the exact compare-and-swap implementation used by assistant tools.
fn store(db:&Db,user:i64,revision:&str,text:&str)->anyhow::Result<Memory> {
    save(db,user,Save { text:text.into(),revision:revision.into() }).map_err(|error|anyhow::anyhow!("{}",error.1))
}

/// Sessions on different computers see one user's notes; other users and anonymous callers cannot.
#[tokio::test]
async fn http_memories_are_private_and_available_without_credits()->anyhow::Result<()> {
    let state=crate::tests::application()?;
    let (user,token)=login(&state,"first@example.test")?;
    let (_,other)=login(&state,"second@example.test")?;
    let (_,second_session)=login(&state,"first@example.test")?;
    let app=crate::router(state.clone());
    assert_eq!(request(&app,None,None).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,None,Some(json!({"text":"private","revision":"0"}))).await?.0,StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app,Some(&token),None).await?.1,json!({"text":"","revision":"0"}));
    let text="Prefer češtinu · 日本語 · Türkçe\nLast task: fixed the audio output";
    let (status,saved)=request(&app,Some(&token),Some(json!({"text":text,"revision":"0"}))).await?;
    assert_eq!(status,StatusCode::OK); assert_eq!(saved["text"],text);
    assert_eq!(request(&app,Some(&second_session),None).await?.1,saved);
    assert_eq!(request(&app,Some(&other),None).await?.1["text"],"");
    assert!(!request(&app,Some(&token),Some(json!({"text":"overwrite","revision":saved["revision"],"user_id":999}))).await?.0.is_success());
    assert_eq!(request(&app,Some(&token),None).await?.1,saved);
    assert_eq!(state.db.balance(user)?,0);
    Ok(())
}

/// Clearing notes must invalidate old writes even though the content becomes empty again.
#[test]
fn stale_revisions_and_clear_protect_manual_edits()->anyhow::Result<()> {
    let db=Db::open(std::path::Path::new(":memory:"))?; let user=db.user("revision@example.test",None,10)?;
    let original=store(&db,user,"0","Old preference")?;
    let edited=store(&db,user,&original.revision,"Manual correction")?;
    assert_eq!(save(&db,user,Save { text:"Stale assistant overwrite".into(),revision:original.revision }).err().unwrap().0,StatusCode::CONFLICT);
    assert_eq!(read(&db,user)?.text,"Manual correction");
    let cleared=store(&db,user,&edited.revision,"")?;
    assert_ne!(cleared.revision,"0"); assert_ne!(cleared.revision,edited.revision);
    assert!(save(&db,user,Save { text:"Restore old notes".into(),revision:edited.revision }).is_err());
    assert_eq!(read(&db,user)?.text,"");
    Ok(())
}

/// The storage budget counts actual UTF-8 bytes and never partially truncates a valid note.
#[test]
fn unicode_budget_rejects_oversized_and_null_text()->anyhow::Result<()> {
    let db=Db::open(std::path::Path::new(":memory:"))?; let user=db.user("unicode@example.test",None,10)?;
    let text="界".repeat(MAX_BYTES/3)+"x";
    assert_eq!(text.len(),MAX_BYTES);
    let saved=store(&db,user,"0",&text)?;
    for text in [text+"x","bad\0text".into()] {
        assert_eq!(save(&db,user,Save { text,revision:saved.revision.clone() }).err().unwrap().0,StatusCode::BAD_REQUEST);
        assert_eq!(read(&db,user)?.text,saved.text);
    }
    Ok(())
}

/// Model tools append compact updates, consolidate, and reject caller-supplied user identities.
#[test]
fn tools_append_consolidate_and_enforce_user_scope()->anyhow::Result<()> {
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("tools@example.test",None,10)?; let other=db.user("other@example.test",None,10)?;
    let read_result=invoke(&db,Some(user),"memory_read",json!({}));
    assert_eq!(read_result["success"],true);
    assert_eq!(serde_json::from_str::<Value>(read_result["contentItems"][0]["text"].as_str().unwrap())?["revision"],"0");
    for text in ["Prefers concise answers","Audio fixed by selecting HDMI"] {
        let revision=read(&db,user)?.revision;
        assert_eq!(invoke(&db,Some(user),"memory_save",json!({"text":text,"revision":revision,"append":true}))["success"],true);
    }
    let notes=read(&db,user)?;
    assert_eq!(notes.text,"Prefers concise answers\n\nAudio fixed by selecting HDMI");
    let summary="Concise answers; HDMI is the verified audio output on the previously inspected computer";
    assert_eq!(invoke(&db,Some(user),"memory_save",json!({"text":summary,"revision":notes.revision,"append":false}))["success"],true);
    assert_eq!(read(&db,user)?.text,summary); assert_eq!(read(&db,other)?.text,"");
    assert_eq!(invoke(&db,None,"memory_read",json!({}))["success"],false);
    assert_eq!(invoke(&db,Some(user),"memory_read",json!({"user_id":other}))["success"],false);
    assert_eq!(invoke(&db,Some(user),"memory_save",json!({"text":"injected","revision":read(&db,user)?.revision,"append":false,"user_id":other}))["success"],false);
    let revision=read(&db,user)?.revision;
    store(&db,user,&revision,"Edited by the user")?;
    assert_eq!(invoke(&db,Some(user),"memory_save",json!({"text":"Outdated note","revision":revision,"append":true}))["success"],false);
    assert_eq!(read(&db,user)?.text,"Edited by the user");
    Ok(())
}

/// Actual simultaneous saves must produce one success and one conflict rather than a lost update.
#[test]
fn concurrent_writers_do_not_overwrite_each_other()->anyhow::Result<()> {
    let db=std::sync::Arc::new(Db::open(std::path::Path::new(":memory:"))?);
    let user=db.user("concurrent@example.test",None,10)?;
    let gate=std::sync::Arc::new(std::sync::Barrier::new(3));
    let writers=["First writer","Second writer"].map(|text| {
        let (db,gate)=(db.clone(),gate.clone());
        std::thread::spawn(move|| { gate.wait(); save(&db,user,Save { text:text.into(),revision:"0".into() }).map(|_|StatusCode::OK).unwrap_or_else(|error|error.0) })
    });
    gate.wait();
    let outcomes=writers.map(|writer|writer.join().unwrap());
    assert_eq!(outcomes.iter().filter(|status|**status==StatusCode::OK).count(),1);
    assert_eq!(outcomes.iter().filter(|status|**status==StatusCode::CONFLICT).count(),1);
    assert!(["First writer","Second writer"].contains(&read(&db,user)?.text.as_str()));
    Ok(())
}

/// Conversation expiry and restarts preserve memory, balances and request identities.
#[test]
fn memory_survives_cache_cleanup_and_reopening()->anyhow::Result<()> {
    let directory=tempfile::tempdir()?; let path=directory.path().join("memory.db");
    let db=Db::open(&path)?; let user=db.user("persistent@example.test",None,100)?;
    let saved=store(&db,user,"0","Verified result 世界")?;
    {
        let connection=db.0.lock().unwrap();
        connection.execute("INSERT INTO conversations(user_id,id,messages) VALUES(?1,'old-chat','[]')",[user])?;
        connection.execute("INSERT INTO billing_tasks(user_id,id,conversation_id,input_rate,cached_rate,output_rate,finished_at) VALUES(?1,'old-task','old-chat',0,0,0,?2)",params![user,crate::db::now()-crate::storage::RETENTION-1])?;
        connection.execute("INSERT INTO requests(user_id,id,task_id,conversation_id,fingerprint,response) VALUES(?1,'old-request','old-task','old-chat','identity','{}')",[user])?;
    }
    db.prune()?;
    assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM conversations",[],|row|row.get::<_,i64>(0))?,0);
    drop(db);
    let db=Db::open(&path)?; let restored=read(&db,user)?;
    assert_eq!(restored.text,saved.text); assert_eq!(restored.revision,saved.revision);
    assert_eq!(db.balance(user)?,100);
    assert_eq!(db.0.lock().unwrap().query_row("SELECT fingerprint FROM requests WHERE id='old-request'",[],|row|row.get::<_,String>(0))?,"identity");
    Ok(())
}
