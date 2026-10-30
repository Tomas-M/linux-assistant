use axum::{body::Bytes, extract::State, http::HeaderMap, Json};
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::Sha256;
use crate::{db::{now, secret}, App, ApiError, ApiResult};

/// Expose only the signed-in user's five latest completed purchases, without provider identifiers.
pub async fn purchases(State(app):State<App>,headers:HeaderMap)->ApiResult<Json<Value>> {
    let user=app.user(&headers)?;
    let connection=app.db.0.lock().unwrap();
    let mut statement=connection.prepare("SELECT credits,paid_at FROM purchases WHERE user_id=?1 AND paid_at IS NOT NULL ORDER BY paid_at DESC,id DESC LIMIT 5")?;
    let purchases=statement.query_map([user],|row|Ok(json!({"credits":row.get::<_,i64>(0)?,"paid_at":row.get::<_,i64>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"purchases":purchases})))
}

/// Offer the current configured packs without exposing Stripe credentials.
pub async fn packs(State(app):State<App>,headers:HeaderMap)->ApiResult<Json<Value>> {
    app.user(&headers)?;
    let config=app.config();
    Ok(Json(json!({"packs":if config.stripe_secret.is_some() { &config.credit_packs[..] } else { &[] },"development":config.dev})))
}

/// Clients select only a configured Price ID; the server controls the credits and checkout amount.
pub async fn checkout(State(app): State<App>, headers: HeaderMap,Json(input):Json<protocol::CheckoutRequest>) -> ApiResult<Json<Value>> {
    let user = app.user(&headers)?;
    create_checkout(&app,user,&input.price_id).await
}

/// Share the same server-priced purchase flow between the user API and the operator dashboard.
pub async fn create_checkout(app: &App, user: i64,price_id:&str) -> ApiResult<Json<Value>> {
    let config=app.config();
    if config.dev { return Err(ApiError::bad("Payments are disabled in development mode")); }
    let pack=config.credit_packs.iter().find(|pack|pack.price_id==price_id).ok_or_else(||ApiError::bad("This credit pack is no longer available; refresh the pack list"))?;
    let key = config.stripe_secret.as_ref().ok_or_else(||ApiError::bad("Payments are not configured"))?;
    let email:Option<String>=app.db.0.lock().unwrap().query_row("SELECT email FROM users WHERE id=?1 AND id NOT IN (SELECT user_id FROM user_blocks)",[user],|r|r.get(0)).optional()?;
    let email=email.ok_or_else(||ApiError::bad("User does not exist or is blocked"))?;
    let id = secret();
    app.db.0.lock().unwrap().execute("INSERT INTO purchases(id,user_id,credits) VALUES(?1,?2,?3)",params![id,user,pack.credits])?;
    let session: Value = app.http.post("https://api.stripe.com/v1/checkout/sessions").basic_auth(key,Some(""))
        .header("Idempotency-Key",&id).form(&[("mode","payment"),("line_items[0][price]",pack.price_id.as_str()),
            ("line_items[0][quantity]","1"),("metadata[purchase_id]",&id),("client_reference_id",&id),("customer_email",&email),
            ("success_url",&format!("{}/payment/success",config.public_url)),("cancel_url",&format!("{}/payment/cancel",config.public_url))])
        .send().await?.error_for_status()?.json().await?;
    let session_id = session["id"].as_str().ok_or_else(||ApiError::bad("Stripe did not create a session"))?;
    let url = session["url"].as_str().ok_or_else(||ApiError::bad("Stripe checkout URL missing"))?;
    app.db.0.lock().unwrap().execute("UPDATE purchases SET stripe_session=?1 WHERE id=?2",params![session_id,id])?;
    Ok(Json(json!({"url":url})))
}

/// Only signed, paid Checkout sessions can add credits; duplicate events are harmless.
pub async fn webhook(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    let config=app.config();
    if config.dev { return Err(ApiError::bad("Payments are disabled in development mode")); }
    let key = config.stripe_webhook_secret.as_ref().ok_or_else(||ApiError::bad("Payments are not configured"))?;
    let signature = headers.get("stripe-signature").and_then(|s|s.to_str().ok()).unwrap_or_default();
    if !valid_signature(signature,&body,key,now()) { return Err(ApiError::bad("Invalid webhook signature")); }
    let event: Value = serde_json::from_slice(&body)?;
    if !matches!(event["type"].as_str(),Some("checkout.session.completed"|"checkout.session.async_payment_succeeded")) {
        return Ok(Json(json!({"received":true})));
    }
    let session = &event["data"]["object"];
    if session["payment_status"] != "paid" || session["mode"] != "payment" { return Ok(Json(json!({"received":true}))); }
    let session_id = session["id"].as_str().ok_or_else(||ApiError::bad("Missing checkout session"))?;
    let purchase_id = session["metadata"]["purchase_id"].as_str().ok_or_else(||ApiError::bad("Missing purchase reference"))?;
    fulfill(&app,session_id,purchase_id)?;
    Ok(Json(json!({"received":true})))
}

/// Settle the server-created purchase and its ledger entry in one transaction.
fn fulfill(app: &App, session_id: &str, purchase_id: &str) -> ApiResult<()> {
    let mut conn = app.db.0.lock().unwrap();
    let tx = conn.transaction()?;
    let purchase: Option<(i64,i64,Option<i64>)> = tx.query_row("SELECT user_id,credits,paid_at FROM purchases WHERE id=?1 AND stripe_session=?2",params![purchase_id,session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((user,credits,paid_at)) = purchase else { return Err(ApiError::busy("Purchase not ready; retry webhook")); };
    if paid_at.is_none() {
        tx.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(?1,?2,?3,?4)",params![user,credits,format!("stripe:{session_id}"),now()])?;
        tx.execute("UPDATE purchases SET paid_at=?1 WHERE id=?2",params![now(),purchase_id])?;
    }
    tx.commit()?;
    Ok(())
}

/// Verify the exact request bytes with timestamp tolerance and all rotated signatures.
fn valid_signature(header: &str, body: &[u8], secret: &str, timestamp: i64) -> bool {
    let fields: Vec<_> = header.split(',').filter_map(|part|part.trim().split_once('=')).collect();
    let Some(time) = fields.iter().find(|(key,_)|*key=="t").and_then(|(_,value)|value.parse::<i64>().ok()) else { return false; };
    if timestamp.abs_diff(time)>300 { return false; }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{time}.").as_bytes());
    mac.update(body);
    fields.iter().filter(|(key,_)|*key=="v1").any(|(_,signature)| {
        hex::decode(signature).is_ok_and(|value|mac.clone().verify_slice(&value).is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Catalog reads require a user session, and checkout cannot select unconfigured prices or credit amounts.
    #[tokio::test]
    async fn credit_pack_catalog_and_selection_are_server_controlled()->anyhow::Result<()> {
        use axum::{body::{Body,to_bytes},http::Request};
        use tower::ServiceExt;
        let pack=protocol::CreditPack { price_id:"price_small".into(),usd:"2.99".into(),credits:100 };
        let config=crate::config::Config { stripe_secret:Some("fixture-only".into()),credit_packs:vec![pack],..Default::default() };
        let app=App::testing(config)?;
        let user=app.db.user("buyer@example.test",None,0)?;
        app.db.0.lock().unwrap().execute("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",params![crate::db::hash("fixture-session"),user,now()+60])?;
        let router=crate::router(app.clone());
        let unauthenticated=router.clone().oneshot(Request::builder().uri("/v1/credit-packs").body(Body::empty())?).await?;
        assert_eq!(unauthenticated.status(),axum::http::StatusCode::UNAUTHORIZED);
        let catalog=router.clone().oneshot(Request::builder().uri("/v1/credit-packs").header("authorization","Bearer fixture-session").body(Body::empty())?).await?;
        let catalog:Value=serde_json::from_slice(&to_bytes(catalog.into_body(),4096).await?)?;
        assert_eq!(catalog["packs"],json!([{"price_id":"price_small","usd":"2.99","credits":100}]));
        for (body,status) in [(json!({"price_id":"price_unknown"}),axum::http::StatusCode::BAD_REQUEST),(json!({"price_id":"price_small","credits":999999}),axum::http::StatusCode::UNPROCESSABLE_ENTITY)] {
            let response=router.clone().oneshot(Request::builder().method("POST").uri("/v1/checkout").header("authorization","Bearer fixture-session").header("content-type","application/json").body(Body::from(body.to_string()))?).await?;
            assert_eq!(response.status(),status);
        }
        assert_eq!(app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM purchases",[],|row|row.get::<_,i64>(0))?,0);
        Ok(())
    }
    /// USD display values use exact cents and reject malformed amounts without contacting Stripe.
    #[test]
    fn credit_pack_usd_values_are_exact()->anyhow::Result<()> {
        let pack=protocol::CreditPack { price_id:"price_small".into(),usd:"2.99".into(),credits:100 };
        assert_eq!(pack.cents()?,299);
        for usd in ["0","-1","2.999","NaN","1e2"] { assert!(protocol::CreditPack { usd:usd.into(),..pack.clone() }.cents().is_err()); }
        assert_eq!(protocol::CreditPack { usd:"5.49".into(),..pack }.cents()?,549);
        Ok(())
    }

    /// Different packs grant their captured credits exactly once, even if the catalog later changes.
    #[test]
    fn credit_pack_fulfillment_uses_purchase_snapshot()->anyhow::Result<()> {
        let app=App::testing(crate::config::Config::default())?;
        let user=app.db.user("buyer@example.test",None,0)?;
        for (index,credits) in [100,200,500].into_iter().enumerate() {
            let id=format!("purchase-{index}"); let session=format!("cs-{index}");
            app.db.0.lock().unwrap().execute("INSERT INTO purchases(id,user_id,credits,stripe_session) VALUES(?1,?2,?3,?4)",params![id,user,credits,session])?;
            for _ in 0..2 { fulfill(&app,&session,&id).map_err(|error|anyhow::anyhow!("{}",error.1))?; }
        }
        assert_eq!(app.db.balance(user)?,800);
        Ok(())
    }
    /// Changed payloads and old signatures must never reach the credit ledger.
    #[test]
    fn rejects_forged_and_old_webhooks() {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
        mac.update(b"1000.payload");
        let signature = format!("t=1000,v1={}",hex::encode(mac.finalize().into_bytes()));
        assert!(valid_signature(&signature,b"payload","test-secret",1000));
        assert!(!valid_signature(&signature,b"changed","test-secret",1000));
        assert!(!valid_signature(&signature,b"payload","test-secret",1301));
        assert!(!valid_signature("t=1000,v1=bad",b"payload","test-secret",1000));
    }
}
