use super::*;
use protocol::{AssistantReply,ChatRequest,Message,Platform,Role};
use serde_json::json;
use uuid::Uuid;

/// Starting a resumed turn clears its binding before a provider subscription and new usage ID exist.
#[test]
fn cleanup_cannot_retire_a_thread_while_its_next_turn_is_starting()->Result<()> {
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("cache-race@example.test",None,100)?;
    let id=Uuid::new_v4();
    let mut request=ChatRequest { generation:Default::default(),bridge:protocol::Bridge::current(), task_id:id,request_id:id,conversation_id:Uuid::new_v4(),base:0,platform:Platform::Linux,messages:vec![Message { role:Role::User,text:"First task".into(),attachments:None }],commands_enabled:false,cwd:"/".into() };
    let pricing=crate::config::Config::default().pricing;
    db.reserve(user,&request,"first",&crate::config::Config { pricing:pricing.clone(),..Default::default() })?;
    db.bind_conversation(user,request.conversation_id,"account")?;
    let mut attempt=crate::usage::Attempt::start(&db,Some(user),&request,"account","conversation")?;
    attempt.identify("native-thread","first-turn",None)?;
    attempt.tokens(&json!({"inputTokens":100,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":100}),&json!({"inputTokens":0,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":0}))?;
    attempt.finish(None)?;
    let completed=db.complete(user,&request,&AssistantReply { text:"Done".into() },Some("account"),Some("native-thread"),None)?;
    request.base=completed.revision; request.request_id=Uuid::new_v4(); request.task_id=request.request_id;
    db.reserve(user,&request,"next",&crate::config::Config { pricing:pricing.clone(),..Default::default() })?;
    db.bind_conversation(user,request.conversation_id,"account")?;
    let mut starting=crate::usage::Attempt::start(&db,Some(user),&request,"account","conversation")?;
    assert!(!db.retire_thread("native-thread")?,"Cleanup must protect the previous native thread before the new turn identifies it");
    assert!(db.provider_counters("native-thread")?.is_some());
    starting.finish(Some("Stopped"))?;
    db.settle(user,&request.request_id.to_string(),Err("Stopped".into()))?;
    assert!(db.retire_thread("native-thread")?);
    Ok(())
}
