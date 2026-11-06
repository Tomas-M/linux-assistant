use super::*;
use anyhow::Result;
use protocol::{ChatRequest,Message,Platform,Role};
use crate::db::{Db,Reservation};

/// Use different reasoning defaults so accidental inheritance across models becomes visible.
fn config()->Config {
    Config { dev:true,public_url:"http://assistant.example.test".into(),model:Some("model-a".into()),reasoning_effort:Some("medium".into()),
        available_models:serde_json::from_value(serde_json::json!([
            {"id":"model-a","reasoning_levels":["low","medium","high"]},
            {"id":"model-b","reasoning_levels":["low","high"]},
            {"id":"model-c","reasoning_levels":[]}
        ])).unwrap(),..Config::default() }
}

/// Each prompt starts a distinct task unless a test explicitly reuses its identity.
fn request(generation:GenerationOptions)->ChatRequest {
    let id=uuid::Uuid::new_v4();
    ChatRequest { generation,bridge:protocol::Bridge::current(),task_id:id,request_id:id,conversation_id:uuid::Uuid::new_v4(),base:0,
        platform:Platform::Linux,messages:vec![Message { role:Role::User,text:"Hello 世界".into(),attachments:None }],commands_enabled:false,cwd:"/".into() }
}

/// Dormant fields cannot override operator settings; enabled choices cannot leak another model's effort.
#[test]
fn model_selection_gate_and_per_model_defaults()->Result<()> {
    let mut config=config();
    let requested=GenerationOptions { model:Some("model-b".into()),reasoning_effort:Some("high".into()) };
    assert_eq!(resolve(&config,&requested)?,GenerationOptions { model:config.model.clone(),reasoning_effort:config.reasoning_effort.clone() });
    config.allow_model_selection=true; config.validate()?;
    assert_eq!(resolve(&config,&requested)?,requested);
    assert_eq!(resolve(&config,&GenerationOptions { model:Some("model-b".into()),reasoning_effort:None })?.reasoning_effort.as_deref(),Some("low"));
    assert_eq!(resolve(&config,&GenerationOptions { model:Some("model-c".into()),reasoning_effort:None })?.reasoning_effort,None);
    assert_eq!(resolve(&config,&GenerationOptions::default())?.reasoning_effort.as_deref(),Some("medium"));
    assert_eq!(resolve(&config,&GenerationOptions { model:None,reasoning_effort:Some("high".into()) })?.model,config.model);
    assert!(resolve(&config,&GenerationOptions { model:Some("model-b".into()),reasoning_effort:Some("medium".into()) }).is_err());
    config.available_models.push(config.available_models[0].clone()); assert!(config.validate().is_err());
    Ok(())
}

/// Invalid selections are rejected before credit admission, leaving no partial billing or request records.
#[test]
fn model_selection_rejection_does_not_admit_or_charge()->Result<()> {
    let db=Db::open(std::path::Path::new(":memory:"))?;
    let user=db.user("selection@example.test",None,100)?;
    let mut config=config(); config.allow_model_selection=true;
    for generation in [
        GenerationOptions { model:Some("unlisted".into()),reasoning_effort:None },
        GenerationOptions { model:Some("model-b".into()),reasoning_effort:Some("medium".into()) },
        GenerationOptions { model:Some("model-c".into()),reasoning_effort:Some("high".into()) },
    ] {
        assert!(db.reserve(user,&request(generation),"rejected",&config).is_err());
    }
    assert_eq!(db.balance(user)?,100);
    let records:i64=db.0.lock().unwrap().query_row("SELECT (SELECT COUNT(*) FROM requests)+(SELECT COUNT(*) FROM billing_tasks)+(SELECT COUNT(*) FROM request_generation)",[],|row|row.get(0))?;
    assert_eq!(records,0);
    Ok(())
}

/// Requested values stay auditable, but restart and later enablement cannot change an admitted task.
#[test]
fn admitted_options_survive_restart_and_catalog_changes()->Result<()> {
    let directory=tempfile::tempdir()?; let path=directory.path().join("assistant.db");
    let db=Db::open(&path)?;
    let user=db.user("owner@example.test",None,100)?; let other=db.user("other@example.test",None,100)?;
    let mut config=config();
    let requested=GenerationOptions { model:Some("model-b".into()),reasoning_effort:Some("high".into()) };
    let input=request(requested.clone());
    let fingerprint=crate::db::hash(&serde_json::to_string(&input)?);
    assert!(matches!(db.reserve(user,&input,&fingerprint,&config)?,Reservation::New));
    let effective=db.generation(user,input.request_id)?;
    assert_eq!(effective.model.as_deref(),Some("model-a"));
    let recorded:String=db.0.lock().unwrap().query_row("SELECT requested FROM request_generation WHERE user_id=?1 AND request_id=?2",rusqlite::params![user,input.request_id.to_string()],|row|row.get(0))?;
    assert_eq!(serde_json::from_str::<GenerationOptions>(&recorded)?,requested);
    assert!(db.generation(other,input.request_id).is_err());
    drop(db); let db=Db::open(&path)?;
    config.allow_model_selection=true; config.model=Some("model-c".into()); config.reasoning_effort=None;
    assert!(matches!(db.reserve(user,&input,&fingerprint,&config)?,Reservation::Pending));
    assert_eq!(db.generation(user,input.request_id)?,effective);
    let mut changed=input.clone(); changed.generation=GenerationOptions::default();
    assert!(db.reserve(user,&changed,&crate::db::hash(&serde_json::to_string(&changed)?),&config).is_err());
    assert_eq!(db.balance(user)?,100);
    Ok(())
}

/// Catalog discovery works before login and advertises the same defaults that admission resolves.
#[tokio::test]
async fn public_catalog_matches_admission_and_stays_dormant()->Result<()> {
    let config=config();
    let disabled=crate::App::testing(config.clone())?;
    let manifest=crate::appearance::manifest(axum::extract::State(disabled)).await.0;
    assert!(!manifest.model_catalog.enabled);
    let mut config=config; config.allow_model_selection=true;
    let expected=resolve(&config,&GenerationOptions::default())?;
    let app=crate::App::testing(config)?;
    let manifest=crate::appearance::manifest(axum::extract::State(app)).await.0;
    assert!(manifest.model_catalog.enabled);
    assert_eq!(manifest.model_catalog.defaults,expected);
    assert_eq!(manifest.model_catalog.models[1].id,"model-b");
    assert_eq!(manifest.model_catalog.models[1].reasoning_levels,["low","high"]);
    Ok(())
}
