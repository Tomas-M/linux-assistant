use axum::extract::{Query, State};
use rusqlite::{types::ValueRef, Connection, Params};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use crate::{db::now, App, ApiError, ApiResult};
use axum::Json;

/// Serialize explicitly selected SQLite columns, preserving SQL NULL as unknown rather than zero.
fn rows(conn: &Connection, sql: &str, params: impl Params) -> anyhow::Result<Vec<Value>> {
    let mut statement=conn.prepare(sql)?;
    let names:Vec<String>=statement.column_names().iter().map(|name|(*name).into()).collect();
    let mut cursor=statement.query(params)?;
    let mut result=Vec::new();
    while let Some(row)=cursor.next()? {
        let mut object=Map::new();
        for (index,name) in names.iter().enumerate() {
            let value=match row.get_ref(index)? {
                ValueRef::Null=>Value::Null,ValueRef::Integer(value)=>json!(value),ValueRef::Real(value)=>json!(value),
                ValueRef::Text(value)=>json!(std::str::from_utf8(value)?),ValueRef::Blob(_)=>anyhow::bail!("Unexpected binary report field"),
            };
            object.insert(name.clone(),value);
        }
        result.push(Value::Object(object));
    }
    Ok(result)
}

#[derive(Deserialize, Default)]
pub(super) struct Filter { from: Option<i64>, to: Option<i64>, user: Option<i64>, account: Option<String>, purpose: Option<String>, page: Option<i64> }

/// Aggregate measured attempts separately from the authoritative credit ledger, with bounded detail pages.
pub(super) async fn report(State(app): State<App>, Query(filter): Query<Filter>) -> ApiResult<Json<Value>> {
    let config=app.config();
    let to=filter.to.unwrap_or(now()+1);
    let from=filter.from.unwrap_or(to-7*86400);
    let page=filter.page.unwrap_or(0);
    if from<0 || to<=from || to-from>366*86400 || !(0..=100_000).contains(&page) { return Err(ApiError::bad("Choose a date range of at most 366 days and a valid page")); }
    let conn=app.db.0.lock().unwrap();
    let params=rusqlite::params![from,to,filter.user,filter.account,filter.purpose];
    let selection="FROM usage x LEFT JOIN users u ON u.id=x.user_id WHERE x.started_at>=?1 AND x.started_at<?2 AND (?3 IS NULL OR x.user_id=?3) AND (?4 IS NULL OR x.account_home=?4) AND (?5 IS NULL OR x.purpose=?5)";
    // Actual debits include recorded failures and allocate task rounding in attempt order.
    let charge="COALESCE((SELECT -delta FROM ledger WHERE reference='usage:'||x.id AND user_id=x.user_id),0)";
    let totals=format!("COUNT(*) AS attempts,SUM(finished_at IS NULL) AS running,SUM(error IS NOT NULL) AS failed,COUNT(total_tokens) AS measured,
        SUM(input_tokens) AS input_tokens,SUM(cached_tokens) AS cached_tokens,SUM(output_tokens) AS output_tokens,SUM(reasoning_tokens) AS reasoning_tokens,SUM(total_tokens) AS total_tokens,SUM({charge}) AS credits_charged");
    let summary=rows(&conn,&format!("SELECT {totals} {selection}"),params)?;
    let grain=if to-from<=2*86400 { "%Y-%m-%d %H:00" } else { "%Y-%m-%d" };
    let timeline=rows(&conn,&format!("SELECT strftime('{grain}',started_at,'unixepoch') AS time,{totals} {selection} GROUP BY time ORDER BY time"),params)?;
    let accounts=rows(&conn,&format!("SELECT account_home AS name,{totals} {selection} GROUP BY account_home ORDER BY SUM(total_tokens) DESC"),params)?;
    let purposes=rows(&conn,&format!("SELECT purpose AS name,{totals} {selection} GROUP BY purpose"),params)?;
    let models=rows(&conn,&format!("SELECT model AS name,{totals} {selection} GROUP BY model"),params)?;
    let activity=rows(&conn,&format!("SELECT x.*,u.email,{charge} AS credits_charged {selection} ORDER BY x.id DESC LIMIT 100 OFFSET {}",page*100),params)?;
    let ledger_filter="FROM ledger l LEFT JOIN users u ON u.id=l.user_id LEFT JOIN credit_adjustments a ON a.ledger_id=l.id WHERE l.created_at>=?1 AND l.created_at<?2 AND (?3 IS NULL OR l.user_id=?3)";
    let ledger_params=rusqlite::params![from,to,filter.user];
    let credit_totals="COALESCE(SUM(CASE WHEN reference LIKE 'request:%' OR reference LIKE 'usage:%' OR reference LIKE 'task:%' THEN -delta ELSE 0 END),0) AS debited,
        COALESCE(SUM(CASE WHEN reference LIKE 'refund:%' THEN delta ELSE 0 END),0) AS refunded,
        COALESCE(SUM(CASE WHEN delta>0 AND reference NOT LIKE 'refund:%' THEN delta ELSE 0 END),0) AS added";
    let credits=rows(&conn,&format!("SELECT COUNT(*) AS entries,{credit_totals} {ledger_filter}"),ledger_params)?;
    let credit_timeline=rows(&conn,&format!("SELECT strftime('{grain}',created_at,'unixepoch') AS time,{credit_totals} {ledger_filter} GROUP BY time ORDER BY time"),ledger_params)?;
    let ledger=rows(&conn,&format!("SELECT l.*,u.email,a.note {ledger_filter} ORDER BY l.id DESC LIMIT 100 OFFSET {}",page*100),ledger_params)?;
    let quotas=rows(&conn,"SELECT account_home,checked_at,limits FROM quota_history WHERE id IN (
        SELECT MAX(id) FROM quota_history WHERE checked_at>=?1 AND checked_at<?2 AND (?3 IS NULL OR account_home=?3)
        GROUP BY account_home,checked_at/3600) ORDER BY checked_at DESC LIMIT 2000",rusqlite::params![from,to,filter.account])?;
    let coverage=rows(&conn,"SELECT MIN(started_at) AS since FROM usage",[])?;
    Ok(Json(json!({"summary":summary[0],"timeline":timeline,"accounts":accounts,"purposes":purposes,"models":models,"activity":activity,
        "credits":credits[0],"credit_timeline":credit_timeline,"ledger":ledger,"quotas":quotas,"coverage":coverage[0],"page":page,"pricing":config.pricing.usd_per_million(),
        "payments":{"stripe":config.stripe_secret.is_some() && !config.dev,"packs":config.credit_packs},"development":config.dev})))
}

#[derive(Deserialize, Default)]
pub(super) struct UserFilter { search: Option<String>, page: Option<i64> }

/// Paginate users with all-time balances and measured token totals, including users with no usage yet.
pub(super) async fn users(State(app): State<App>, Query(filter): Query<UserFilter>) -> ApiResult<Json<Value>> {
    let page=filter.page.unwrap_or(0);
    let search=filter.search.unwrap_or_default();
    if !(0..=100_000).contains(&page) || search.len()>320 { return Err(ApiError::bad("Invalid user search")); }
    let conn=app.db.0.lock().unwrap();
    let total:i64=conn.query_row("SELECT COUNT(*) FROM users",[],|r|r.get(0))?;
    let matching:i64=conn.query_row("SELECT COUNT(*) FROM users WHERE instr(lower(email),lower(?1))>0",[&search],|r|r.get(0))?;
    let users=rows(&conn,"SELECT u.id,u.email,EXISTS(SELECT 1 FROM user_blocks b WHERE b.user_id=u.id) AS blocked,
        (SELECT COALESCE(SUM(delta),0) FROM ledger WHERE user_id=u.id) AS credits,
        (SELECT SUM(total_tokens) FROM usage WHERE user_id=u.id) AS tokens,
        (SELECT COUNT(*) FROM usage WHERE user_id=u.id AND total_tokens IS NULL) AS unknown_attempts,
        (SELECT COUNT(*) FROM requests WHERE user_id=u.id) AS requests,
        (SELECT COUNT(*) FROM requests r WHERE r.user_id=u.id AND NOT EXISTS(SELECT 1 FROM usage x WHERE x.user_id=r.user_id AND x.request_id=r.id)) AS unmeasured_requests
        FROM users u WHERE instr(lower(email),lower(?1))>0 ORDER BY u.id DESC LIMIT 50 OFFSET ?2",rusqlite::params![search,page*50])?;
    Ok(Json(json!({"users":users,"total":total,"matching":matching,"page":page})))
}
