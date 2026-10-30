use super::*;
use axum::{routing::{get,post},Json,Router};
use std::sync::{Arc,Mutex};

/// Retry the same chat ID exactly five times, five seconds apart, and clear the final connection indicator.
#[tokio::test]
async fn gateway_retries_keep_request_identity_and_interval()->Result<()> {
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url=format!("http://{}/v1/chat",listener.local_addr()?);
    let attempts=Arc::new(Mutex::new(Vec::new()));
    let recorded=attempts.clone();
    let app=Router::new().route("/v1/chat",post(move |Json(body):Json<serde_json::Value>| {
        recorded.lock().unwrap().push((std::time::Instant::now(),body));
        async { axum::http::StatusCode::BAD_GATEWAY }
    }));
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let live=Live::new();
    let body=json!({"request_id":uuid::Uuid::new_v4(),"text":"Retry 世界"});
    assert!(send(client(60)?.post(url).json(&body),&live,None).await.unwrap_err().to_string().contains("after 5 attempts"));
    let attempts=attempts.lock().unwrap();
    assert_eq!(attempts.len(),5);
    assert!(attempts.iter().all(|(_,input)|input==&body));
    assert!(attempts.windows(2).all(|pair|pair[1].0.duration_since(pair[0].0)>=Duration::from_secs(5)));
    assert!(live.0.borrow()["connections"].as_object().unwrap().is_empty());
    server.abort();
    Ok(())
}

/// An unavailable backend may recover during the retry delay; cancelled requests remove their indicator too.
#[tokio::test]
async fn refused_connection_recovers_and_cancel_cleans_up()->Result<()> {
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address=listener.local_addr()?;
    drop(listener);
    let live=Live::new();
    let mut changes=live.0.subscribe();
    let progress=live.clone();
    let request=client(60)?.get(format!("http://{address}/v1/me"));
    let task=tokio::spawn(async move { send(request,&progress,None).await });
    tokio::time::timeout(Duration::from_secs(10),async { while changes.borrow()["connections"].as_object().unwrap().is_empty() { changes.changed().await.unwrap(); } }).await?;
    assert_eq!(changes.borrow()["connections"].as_object().unwrap().values().next().unwrap()["attempt"],2);
    let listener=tokio::net::TcpListener::bind(address).await?;
    let app=Router::new().route("/v1/me",get(||async { Json(json!({"credits":10})) }));
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    assert!(tokio::time::timeout(Duration::from_secs(10),task).await???.status().is_success());
    assert!(live.0.borrow()["connections"].as_object().unwrap().is_empty());
    server.abort();
    let _=server.await;
    let progress=live.clone();
    let request=client(60)?.get(format!("http://{address}/v1/me"));
    let task=tokio::spawn(async move { send(request,&progress,None).await });
    tokio::time::timeout(Duration::from_secs(10),async { while changes.borrow()["connections"].as_object().unwrap().is_empty() { changes.changed().await.unwrap(); } }).await?;
    task.abort();
    let _=task.await;
    assert!(live.0.borrow()["connections"].as_object().unwrap().is_empty());
    Ok(())
}

/// Gateway failures cannot replay payment/login mutations, and valid application errors fail immediately.
#[tokio::test]
async fn non_idempotent_posts_and_application_errors_are_not_replayed()->Result<()> {
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url=format!("http://{}",listener.local_addr()?);
    let attempts=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded=attempts.clone();
    let app=Router::new().fallback(move |request:axum::extract::Request| {
        recorded.fetch_add(1,Ordering::SeqCst);
        async move {
            let status=if request.uri().path()=="/v1/chat" { axum::http::StatusCode::SERVICE_UNAVAILABLE } else { axum::http::StatusCode::BAD_GATEWAY };
            (status,Json(json!({"error":"Unavailable"})))
        }
    });
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let live=Live::new();
    for path in ["/v1/checkout","/v1/auth/verify","/v1/chat"] {
        let response=tokio::time::timeout(Duration::from_secs(1),send(client(60)?.post(format!("{url}{path}")),&live,None)).await??;
        assert!(response.status().is_server_error());
    }
    assert_eq!(attempts.load(Ordering::SeqCst),3);
    server.abort();
    Ok(())
}
