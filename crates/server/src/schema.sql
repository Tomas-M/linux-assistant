PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, google_sub TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS sessions(token_hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS devices(id TEXT PRIMARY KEY, poll_hash TEXT NOT NULL, expires_at INTEGER NOT NULL, user_id INTEGER REFERENCES users(id), email TEXT, code_hash TEXT, attempts INTEGER NOT NULL DEFAULT 0, sent_at INTEGER);
CREATE TABLE IF NOT EXISTS oauth(state_hash TEXT PRIMARY KEY, device_id TEXT NOT NULL REFERENCES devices(id), verifier TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ledger(id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), delta INTEGER NOT NULL, reference TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS ledger_user ON ledger(user_id);
CREATE INDEX IF NOT EXISTS ledger_time ON ledger(created_at);
CREATE TABLE IF NOT EXISTS billing_tasks(user_id INTEGER NOT NULL REFERENCES users(id), id TEXT NOT NULL, conversation_id TEXT NOT NULL,
    input_rate INTEGER NOT NULL, cached_rate INTEGER NOT NULL, output_rate INTEGER NOT NULL, finished_at INTEGER, PRIMARY KEY(user_id,id));
CREATE TABLE IF NOT EXISTS requests(user_id INTEGER NOT NULL REFERENCES users(id), id TEXT NOT NULL, task_id TEXT NOT NULL, conversation_id TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT, error TEXT, steering TEXT NOT NULL DEFAULT '[]', PRIMARY KEY(user_id,id));
CREATE INDEX IF NOT EXISTS requests_task ON requests(user_id,task_id);
CREATE INDEX IF NOT EXISTS requests_conversation ON requests(user_id,conversation_id) WHERE response IS NULL AND error IS NULL;
CREATE TABLE IF NOT EXISTS conversations(user_id INTEGER NOT NULL REFERENCES users(id), id TEXT NOT NULL, account_home TEXT, thread_id TEXT, messages TEXT NOT NULL DEFAULT '[]', checkpoint TEXT NOT NULL DEFAULT '', checkpoint_at INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(user_id,id));
CREATE TABLE IF NOT EXISTS purchases(id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), credits INTEGER NOT NULL CHECK(credits>0), stripe_session TEXT UNIQUE, paid_at INTEGER);
CREATE TABLE IF NOT EXISTS admin_sessions(token_hash TEXT PRIMARY KEY, password_hash TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS user_blocks(user_id INTEGER PRIMARY KEY REFERENCES users(id), created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS credit_adjustments(id TEXT PRIMARY KEY, ledger_id INTEGER NOT NULL UNIQUE REFERENCES ledger(id), note TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS provider_accounts(home TEXT PRIMARY KEY, checked_at INTEGER, email TEXT, plan TEXT, limits TEXT, error TEXT);
CREATE TABLE IF NOT EXISTS provider_pauses(home TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS usage(id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), request_id TEXT NOT NULL, conversation_id TEXT NOT NULL,
    account_home TEXT NOT NULL, purpose TEXT NOT NULL, model TEXT, thread_id TEXT, turn_id TEXT, started_at INTEGER NOT NULL, finished_at INTEGER, error TEXT,
    input_tokens INTEGER, cached_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, total_tokens INTEGER);
CREATE INDEX IF NOT EXISTS usage_time ON usage(started_at);
CREATE INDEX IF NOT EXISTS usage_user ON usage(user_id,started_at);
CREATE INDEX IF NOT EXISTS usage_request ON usage(user_id,request_id);
CREATE TABLE IF NOT EXISTS quota_history(id INTEGER PRIMARY KEY, account_home TEXT NOT NULL, checked_at INTEGER NOT NULL, limits TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS quota_history_time ON quota_history(checked_at);
