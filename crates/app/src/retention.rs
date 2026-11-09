//! 历史保留策略：AI 文件改动备份与审计日志按天数自动清理（天数在设置里可调）。
//! 启动时跑一次，之后每 6 小时一次；设置变更后前端调 retention_apply 立即生效。

use std::sync::Arc;
use std::time::Duration;

use core_store::Store;
use serde_json::Value;

use crate::mcp_files::BackupStore;

const BACKUP_DAYS_KEY: &str = "history.aiBackupDays";
const AUDIT_DAYS_KEY: &str = "history.auditDays";
const BACKUP_DAYS_DEFAULT: u32 = 7;
const AUDIT_DAYS_DEFAULT: u32 = 30;
const DAYS_MAX: u32 = 365;
const INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// settings 值是 JSON 文本（数字存为 "7"）；缺省或越界（1..=365 之外）用默认值
fn parse_days(raw: Option<String>, default: u32) -> u32 {
    raw.and_then(|r| match serde_json::from_str::<Value>(&r) {
        Ok(v) => v.as_u64(),
        Err(_) => r.trim().parse().ok(),
    })
    .filter(|d| (1..=u64::from(DAYS_MAX)).contains(d))
    .map_or(default, |d| d as u32)
}

async fn load_days(store: &Store, key: &str, default: u32) -> u32 {
    parse_days(store.settings().get(key).await.ok().flatten(), default)
}

pub async fn apply(store: &Store, backups: &BackupStore) {
    let backup_days = load_days(store, BACKUP_DAYS_KEY, BACKUP_DAYS_DEFAULT).await;
    backups.set_retention_days(backup_days);
    match backups.prune().await {
        0 => {}
        n => tracing::info!(
            pruned = n,
            days = backup_days,
            "AI 改动备份：已清理过期记录"
        ),
    }
    let audit_days = load_days(store, AUDIT_DAYS_KEY, AUDIT_DAYS_DEFAULT).await;
    match store.audit().prune_older_than(audit_days).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(pruned = n, days = audit_days, "审计日志：已清理过期记录"),
        Err(e) => tracing::warn!(error = %e, "审计清理失败"),
    }
}

pub fn spawn(store: Arc<Store>, backups: Arc<BackupStore>) {
    tauri::async_runtime::spawn(async move {
        loop {
            apply(&store, &backups).await;
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

#[tauri::command]
pub async fn retention_apply(
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
    backups: tauri::State<'_, Arc<BackupStore>>,
) -> Result<(), String> {
    apply(&sessions.store, &backups).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_fall_back_to_default_when_missing_or_out_of_range() {
        assert_eq!(parse_days(None, 7), 7);
        assert_eq!(parse_days(Some("14".into()), 7), 14);
        assert_eq!(
            parse_days(Some("\"30\"".into()), 7),
            7,
            "字符串 JSON 不当数字"
        );
        assert_eq!(parse_days(Some("0".into()), 7), 7);
        assert_eq!(parse_days(Some("366".into()), 30), 30);
        assert_eq!(parse_days(Some("365".into()), 30), 365);
        assert_eq!(parse_days(Some("abc".into()), 30), 30);
    }
}
