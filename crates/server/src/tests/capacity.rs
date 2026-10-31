use super::*;

/// Exercise 100 authenticated HTTP streams and separate balances against a durable production schema.
#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn hundred_users_complete_with_isolated_billing()->anyhow::Result<()> {
    let directory=tempfile::tempdir()?;
    let mut state=application()?;
    state.db=Arc::new(Db::open(&directory.path().join("capacity.db"))?);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address=format!("http://{}",listener.local_addr()?);
    let app=router(state.clone());
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let mut clients=Vec::new();
    for index in 0..100 {
        let user=state.db.user(&format!("load-{index}@example.test"),None,2)?;
        let token=db::secret();
        state.db.0.lock().unwrap().execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",rusqlite::params![db::hash(&token),user,db::now()+60])?;
        clients.push((user,token));
    }
    let http=reqwest::Client::new();
    let mut requests=tokio::task::JoinSet::new();
    for (user,token) in clients {
        let (http,address)=(http.clone(),address.clone());
        requests.spawn(async move {
            let id=uuid::Uuid::new_v4();
            let prompt=format!("User {user} 世界");
            let input=json!({"base":0,"task_id":id,"request_id":id,"conversation_id":uuid::Uuid::new_v4(),"platform":"linux",
                "messages":[{"role":"user","text":prompt}],"commands_enabled":false,"cwd":"/tmp"});
            let response=http.post(format!("{address}/v1/chat")).bearer_auth(token).header("accept","text/event-stream").json(&input).send().await?.error_for_status()?;
            let body=response.text().await?;
            let event=body.lines().filter_map(|line|line.strip_prefix("data: ")).filter_map(|line|serde_json::from_str::<Value>(line).ok()).find(|event|event["type"]=="complete").unwrap();
            let complete=&event["response"];
            assert!(complete["reply"]["text"].as_str().unwrap().contains(&prompt));
            assert_eq!(complete["credits"],1);
            assert_eq!(complete["task_credits"],1);
            Ok::<_,anyhow::Error>(())
        });
    }
    let start=std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(30),async {
        while let Some(result)=requests.join_next().await { result??; }
        Ok::<_,anyhow::Error>(())
    }).await??;
    let remaining:i64=state.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM users u WHERE (SELECT SUM(delta) FROM ledger WHERE user_id=u.id)=1",[],|row|row.get(0))?;
    assert_eq!(remaining,100);
    println!("100 independent users completed in {:?}",start.elapsed());
    server.abort();
    Ok(())
}
