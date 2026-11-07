//! append-only 审计（安全模型第 5 条）：只提供 append 与 query，无 UPDATE/DELETE。
//!
//! 哈希链：每行存 prev_hash（上一行 hash）与 hash = sha256(JSON[prev, ts, actor, session_id,
//! action, detail])。改任意一行内容、删中间行或插行都会让 verify_chain 报断裂。没有外部锚点，
//! 尾部截断与「整链重算」防不住——它是防随手篡改的证据链，不是防有权限攻击者的密码学保证。
//! 迁移前的旧行 hash 为 NULL，链从第一条带 hash 的行开始；按保留策略删掉的最旧行不算断裂。

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Gui,
    Cli,
    Mcp,
}

impl Actor {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub session_id: Option<String>,
    pub action: String,
    pub detail: Value,
}

/// 哈希链校验结果
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChainReport {
    pub ok: bool,
    /// 参与校验（带 hash）的行数
    pub checked: u64,
    /// 迁移前无 hash 的旧行数
    pub legacy: u64,
    /// 第一处断裂的行 id
    pub first_bad_id: Option<i64>,
}

fn row_hash(
    prev: &str,
    ts: &str,
    actor: &str,
    session_id: Option<&str>,
    action: &str,
    detail: &str,
) -> String {
    let canon = serde_json::json!([prev, ts, actor, session_id, action, detail]).to_string();
    Sha256::digest(canon.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const VERIFY_BATCH: i64 = 1000;

pub struct AuditRepo {
    pool: SqlitePool,
    /// 串行化 append：读上一行 hash 与插入之间不能穿插其它写入
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl AuditRepo {
    pub(crate) fn new(pool: SqlitePool, lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self { pool, lock }
    }

    pub async fn append(
        &self,
        actor: Actor,
        session_id: Option<&str>,
        action: &str,
        detail: &impl Serialize,
    ) -> Result<(), StoreError> {
        let detail = serde_json::to_value(detail)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?
            .to_string();
        let q = |e: sqlx::Error| StoreError::Query(e.to_string());
        let _guard = self.lock.lock().await;
        let prev: Option<String> = sqlx::query_scalar(
            "SELECT hash FROM audit WHERE hash IS NOT NULL ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(q)?;
        let prev = prev.unwrap_or_default();
        let ts: String = sqlx::query_scalar("SELECT datetime('now')")
            .fetch_one(&self.pool)
            .await
            .map_err(q)?;
        let hash = row_hash(&prev, &ts, actor.as_str(), session_id, action, &detail);
        sqlx::query(
            "INSERT INTO audit (ts, actor, session_id, action, detail, prev_hash, hash)
             VALUES (?,?,?,?,?,?,?)",
        )
        .bind(&ts)
        .bind(actor.as_str())
        .bind(session_id)
        .bind(action)
        .bind(&detail)
        .bind(&prev)
        .bind(&hash)
        .execute(&self.pool)
        .await
        .map_err(q)?;
        Ok(())
    }

    fn record_of(r: &sqlx::sqlite::SqliteRow) -> AuditRecord {
        let detail_raw: String = r.get("detail");
        AuditRecord {
            id: r.get("id"),
            ts: r.get("ts"),
            actor: r.get("actor"),
            session_id: r.get("session_id"),
            action: r.get("action"),
            detail: serde_json::from_str(&detail_raw).unwrap_or(Value::Null),
        }
    }

    /// 游标分页：按 id 倒序，cursor 为上一页末条 id
    pub async fn query(
        &self,
        cursor: Option<i64>,
        limit: u32,
    ) -> Result<(Vec<AuditRecord>, Option<i64>), StoreError> {
        let limit = limit.clamp(1, 500) as i64;
        let rows = match cursor {
            Some(c) => sqlx::query(
                "SELECT id,ts,actor,session_id,action,detail FROM audit
                 WHERE id < ? ORDER BY id DESC LIMIT ?",
            )
            .bind(c)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?,
            None => sqlx::query(
                "SELECT id,ts,actor,session_id,action,detail FROM audit
                 ORDER BY id DESC LIMIT ?",
            )
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?,
        };
        let mut records: Vec<AuditRecord> = rows.iter().map(Self::record_of).collect();
        let next_cursor = if records.len() as i64 > limit {
            records.pop().map(|r| r.id)
        } else {
            None
        };
        Ok((records, next_cursor))
    }

    /// 按动作与 detail.id 取最新一条（录像校验取结束时落下的摘要）
    pub async fn find_by_detail_id(
        &self,
        action: &str,
        id: &str,
    ) -> Result<Option<AuditRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id,ts,actor,session_id,action,detail FROM audit
             WHERE action = ? AND json_extract(detail, '$.id') = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(action)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Query(e.to_string()))?;
        Ok(row.as_ref().map(Self::record_of))
    }

    /// 逐行重算哈希链（分批读，不一次性载入全表）
    pub async fn verify_chain(&self) -> Result<ChainReport, StoreError> {
        let mut report = ChainReport {
            ok: true,
            checked: 0,
            legacy: 0,
            first_bad_id: None,
        };
        let mut expected: Option<String> = None;
        let mut after = 0i64;
        loop {
            let rows = sqlx::query(
                "SELECT id,ts,actor,session_id,action,detail,prev_hash,hash FROM audit
                 WHERE id > ? ORDER BY id LIMIT ?",
            )
            .bind(after)
            .bind(VERIFY_BATCH)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?;
            if rows.is_empty() {
                return Ok(report);
            }
            for r in &rows {
                let id: i64 = r.get("id");
                after = id;
                let hash: Option<String> = r.get("hash");
                let Some(hash) = hash else {
                    if expected.is_some() {
                        report.ok = false;
                        report.first_bad_id = Some(id);
                        return Ok(report);
                    }
                    report.legacy += 1;
                    continue;
                };
                let prev: String = r.get::<Option<String>, _>("prev_hash").unwrap_or_default();
                let ts: String = r.get("ts");
                let actor: String = r.get("actor");
                let sid: Option<String> = r.get("session_id");
                let action: String = r.get("action");
                let detail: String = r.get("detail");
                let recomputed = row_hash(&prev, &ts, &actor, sid.as_deref(), &action, &detail);
                let linked = expected.as_deref().is_none_or(|e| e == prev);
                if recomputed != hash || !linked {
                    report.ok = false;
                    report.first_bad_id = Some(id);
                    return Ok(report);
                }
                report.checked += 1;
                expected = Some(hash);
            }
        }
    }

    /// 保留策略：删除 ts 早于 days 天前的行（audit 只增不减，启动时调用防无限膨胀）。
    /// ts 为 datetime('now') 文本格式，与 datetime('now', '-N days') 可直接比较
    pub async fn prune_older_than(&self, days: u32) -> Result<u64, StoreError> {
        let r = sqlx::query("DELETE FROM audit WHERE ts < datetime('now', ?)")
            .bind(format!("-{days} days"))
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?;
        Ok(r.rows_affected())
    }
}
