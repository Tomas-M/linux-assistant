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

/// The server chooses the product and credit amount; clients cannot set either.
pub async fn checkout(State(app): State<App>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user = app.user(&headers)?;
    create_checkout(&app,user).await
}

/// Share the same server-priced purchase flow between the user API and the operator dashboard.
pub async fn create_checkout(app: &App, user: i64) -> ApiResult<Json<Value>> {
    if app.config.dev { return Err(ApiError::bad("Payments are disabled in development mode")); }
    let key = app.config.stripe_secret.as_ref().ok_or_else(||ApiError::bad("Payments are not configured"))?;
    let email:Option<String>=app.db.0.lock().unwrap().query_row("SELECT email FROM users WHERE id=?1 AND id NOT IN (SELECT user_id FROM user_blocks)",[user],|r|r.get(0)).optional()?;
    let email=email.ok_or_else(||ApiError::bad("User does not exist or is blocked"))?;
    let id = secret();
    app.db.0.lock().unwrap().execute("INSERT INTO purchases(id,user_id,credits) VALUES(?1,?2,?3)",params![id,user,app.config.pack_credits])?;
    let session: Value = app.http.post("https://api.stripe.com/v1/checkout/sessions").basic_auth(key,Some(""))
        .header("Idempotency-Key",&id).form(&[("mode","payment"),("line_items[0][price]",app.config.stripe_price.as_ref().unwrap()),
            ("line_items[0][quantity]","1"),("metadata[purchase_id]",&id),("client_reference_id",&id),("customer_email",&email),
            ("success_url",&format!("{}/payment/success",app.config.public_url)),("cancel_url",&format!("{}/payment/cancel",app.config.public_url))])
        .send().await?.error_for_status()?.json().await?;
    let session_id = session["id"].as_str().ok_or_else(||ApiError::bad("Stripe did not create a session"))?;
    let url = session["url"].as_str().ok_or_else(||ApiError::bad("Stripe checkout URL missing"))?;
    app.db.0.lock().unwrap().execute("UPDATE purchases SET stripe_session=?1 WHERE id=?2",params![session_id,id])?;
    Ok(Json(json!({"url":url})))
}

/// Only signed, paid Checkout sessions can add credits; duplicate events are harmless.
pub async fn webhook(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    if app.config.dev { return Err(ApiError::bad("Payments are disabled in development mode")); }
    let key = app.config.stripe_webhook_secret.as_ref().ok_or_else(||ApiError::bad("Payments are not configured"))?;
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
