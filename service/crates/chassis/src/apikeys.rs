//! Port of go-service/internal/apikeys/{keys,middleware}.go.
//! delta 2: identity comes ONLY from `authenticate`'s return value — Go's
//! spoofable `X-API-Key-ID` request header is GONE and must never be read —
//! and window-count + usage-insert happen in ONE SQLite transaction
//! (fail-closed: insert errors propagate as Err → HTTP 500).

use rand::RngCore as _;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

/// Safe-to-display view of an API key (never the hash). Go keys.go KeyInfo
/// serializes {"id","label","created_at","revoked":bool} — the R-1 contract
/// golden keys_list.json pins exactly that shape, so it wins over the spine's
/// `revoked_at` sketch. Label is a plain string (Go scans into `string`;
/// a NULL label fails the list exactly as in Go).
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyInfo {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub struct KeyIdentity {
    pub key_id: String,
    pub user_id: String,
}

fn hash_key(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

/// keys.go CreateKey: 16 random bytes → "pk_" + 32 lowercase hex chars;
/// only the SHA-256 hex hash is stored. Returns the plaintext exactly once.
pub fn create_key(conn: &Connection, user_id: &str, label: &str) -> anyhow::Result<String> {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    let plaintext = format!("pk_{}", hex::encode(b));
    conn.execute(
        "INSERT INTO api_keys (id, user_id, key_hash, label) VALUES (?, ?, ?, ?)",
        params![crate::db::new_id(), user_id, hash_key(&plaintext), label],
    )?;
    Ok(plaintext)
}

/// keys.go lookupKey as a free function: hash the presented key, revoked → None.
pub fn authenticate(conn: &Connection, x_api_key: &str) -> Option<KeyIdentity> {
    if x_api_key.is_empty() {
        return None;
    }
    conn.query_row(
        "SELECT id, user_id FROM api_keys WHERE key_hash = ? AND revoked_at IS NULL",
        params![hash_key(x_api_key)],
        |r| {
            Ok(KeyIdentity {
                key_id: r.get(0)?,
                user_id: r.get(1)?,
            })
        },
    )
    .ok()
}

/// middleware.go RateLimit, redesigned per delta 2: ONE transaction —
/// count the sliding 60s window; if already at the limit return Ok(false)
/// WITHOUT inserting; otherwise insert and return Ok(true).
/// Externally identical to Go: requests pass while prior-window count < per_min,
/// so the (per_min+1)-th request in the window is the first rejection.
pub fn check_and_record(
    conn: &Connection,
    key_id: &str,
    endpoint: &str,
    per_min: u32,
) -> anyhow::Result<bool> {
    check_and_record_at(conn, key_id, endpoint, per_min, &crate::db::now())
}

/// `now` in SQLite datetime format (`YYYY-MM-DD HH:MM:SS` UTC, `db::now()`).
/// Hidden test seam — Go manipulated time via SQL (`datetime('now','-61 seconds')`),
/// Rust tests additionally pin `now` to simulate concurrent callers deterministically.
#[doc(hidden)]
pub fn check_and_record_at(
    conn: &Connection,
    key_id: &str,
    endpoint: &str,
    per_min: u32,
    now: &str,
) -> anyhow::Result<bool> {
    // unchecked_transaction: BEGIN/COMMIT on &Connection (spine signature is &Connection).
    let tx = conn.unchecked_transaction()?;
    let used: i64 = tx.query_row(
        "SELECT COUNT(*) FROM api_usage WHERE key_id = ? AND ts > datetime(?, '-60 seconds')",
        params![key_id, now],
        |r| r.get(0),
    )?;
    if used >= per_min as i64 {
        tx.rollback()?; // rejected calls are NOT metered (delta 2)
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO api_usage (id, key_id, ts, endpoint) VALUES (?, ?, ?, ?)",
        params![crate::db::new_id(), key_id, now, endpoint],
    )?;
    tx.commit()?;
    Ok(true)
}

/// delta 12: delete api_usage rows older than `retention_days` (caller passes 90).
/// Called at server startup and daily (bin/server.rs).
pub fn prune_usage(conn: &Connection, retention_days: u32) -> anyhow::Result<usize> {
    let n = conn.execute(
        "DELETE FROM api_usage WHERE ts < datetime('now', ?)",
        params![format!("-{retention_days} days")],
    )?;
    Ok(n)
}

/// keys.go ListKeys: all keys (including revoked), newest first.
/// `revoked_at IS NOT NULL` → Go's `revoked` bool (contract golden shape).
pub fn list_keys(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<KeyInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, created_at, revoked_at IS NOT NULL FROM api_keys WHERE user_id = ? ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![user_id], |r| {
        Ok(KeyInfo {
            id: r.get(0)?,
            label: r.get(1)?,
            created_at: r.get(2)?,
            revoked: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// keys.go RevokeKey: owner-scoped; error when missing or already revoked.
pub fn revoke_key(conn: &Connection, key_id: &str, user_id: &str) -> anyhow::Result<()> {
    let n = conn.execute(
        "UPDATE api_keys SET revoked_at = datetime('now') WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
        params![key_id, user_id],
    )?;
    if n == 0 {
        anyhow::bail!("key {key_id} not found or already revoked");
    }
    Ok(())
}
