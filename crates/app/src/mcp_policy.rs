//! MCP 有效策略解析：会话覆盖 → 最近的祖先分组策略 → 全局默认。
//!
//! 分组策略存设置键 `mcp.group_policies`（JSON：分组路径 → GroupPolicy），每次调用现读，
//! 改分组策略无需重启 MCP 服务。三类字段各自独立继承：分组权限（mcp.allow.* 同名键）、
//! 访问档位（readonly/standard/full）、生产标记。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use core_policy::AccessLevel;
use core_store::{SessionRecord, Store};

pub(crate) const GROUP_POLICIES_KEY: &str = "mcp.group_policies";
/// 全局访问档位设置键（缺省 readonly）
pub(crate) const LEVEL_KEY: &str = "mcp.level";

/// 单个分组的策略覆盖；字段缺省 = 继续向上继承
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
#[serde(rename_all = "camelCase")]
pub struct GroupPolicy {
    #[serde(default)]
    pub perms: HashMap<String, bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub level: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub production: Option<bool>,
}

pub(crate) type GroupPolicies = HashMap<String, GroupPolicy>;

pub(crate) fn parse_level(s: &str) -> Option<AccessLevel> {
    match s {
        "readonly" => Some(AccessLevel::Readonly),
        "standard" => Some(AccessLevel::Standard),
        "full" => Some(AccessLevel::Full),
        _ => None,
    }
}

pub(crate) fn level_str(l: AccessLevel) -> &'static str {
    match l {
        AccessLevel::Readonly => "readonly",
        AccessLevel::Standard => "standard",
        AccessLevel::Full => "full",
    }
}

/// 设置值容忍 JSON 字符串与裸文本
fn setting_str(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::String(s)) => Some(s),
        _ => Some(raw.trim().to_string()),
    }
}

pub(crate) async fn load_group_policies(store: &Store) -> GroupPolicies {
    let raw = store
        .settings()
        .get(GROUP_POLICIES_KEY)
        .await
        .ok()
        .flatten();
    let Some(raw) = raw else {
        return GroupPolicies::new();
    };
    // settings_set 一律 JSON 编码：值可能是对象本身，也可能是对象的 JSON 字符串
    let parsed = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::String(s)) => serde_json::from_str(&s),
        Ok(v) => serde_json::from_value(v),
        Err(e) => Err(e),
    };
    parsed.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "mcp.group_policies 解析失败，按无分组策略处理");
        GroupPolicies::new()
    })
}

pub(crate) async fn load_global_level(store: &Store) -> AccessLevel {
    let raw = store.settings().get(LEVEL_KEY).await.ok().flatten();
    setting_str(raw)
        .as_deref()
        .and_then(parse_level)
        .unwrap_or(AccessLevel::Readonly)
}

/// 由近及远的祖先分组路径："a/b/c" → ["a/b/c", "a/b", "a"]；根（""）无分组策略
fn ancestors(group_path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut p = group_path;
    while !p.is_empty() {
        out.push(p);
        p = match p.rfind('/') {
            Some(i) => &p[..i],
            None => "",
        };
    }
    out
}

/// 某会话的有效策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Effective {
    pub level: AccessLevel,
    pub production: bool,
}

/// 分组权限：会话覆盖 → 分组链 → 全局
pub(crate) fn resolve_perm(
    rec: &SessionRecord,
    groups: &GroupPolicies,
    key: &str,
    global: bool,
) -> bool {
    if let Some(v) = rec.mcp_perms.get(key) {
        return *v;
    }
    ancestors(&rec.group_path)
        .into_iter()
        .find_map(|g| groups.get(g).and_then(|p| p.perms.get(key).copied()))
        .unwrap_or(global)
}

pub(crate) fn resolve_effective(
    rec: &SessionRecord,
    groups: &GroupPolicies,
    global_level: AccessLevel,
) -> Effective {
    let chain = ancestors(&rec.group_path);
    let level = rec
        .mcp_level
        .as_deref()
        .and_then(parse_level)
        .or_else(|| {
            chain.iter().find_map(|g| {
                groups
                    .get(*g)
                    .and_then(|p| p.level.as_deref())
                    .and_then(parse_level)
            })
        })
        .unwrap_or(global_level);
    let production = rec
        .mcp_production
        .or_else(|| {
            chain
                .iter()
                .find_map(|g| groups.get(*g).and_then(|p| p.production))
        })
        .unwrap_or(false);
    Effective { level, production }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn rec(group: &str) -> SessionRecord {
        serde_json::from_value(serde_json::json!({
            "id": "s", "name": "s", "host": "h", "port": 22, "username": "u",
            "authType": "password", "groupPath": group, "tags": [],
            "createdAt": "", "updatedAt": ""
        }))
        .unwrap()
    }

    fn groups() -> GroupPolicies {
        let mut g = GroupPolicies::new();
        g.insert(
            "生产".into(),
            GroupPolicy {
                perms: HashMap::from([("sftp_write".into(), false)]),
                level: Some("readonly".into()),
                production: Some(true),
            },
        );
        g.insert(
            "生产/华东".into(),
            GroupPolicy {
                perms: HashMap::from([("ssh_exec".into(), false)]),
                level: Some("standard".into()),
                production: None,
            },
        );
        g
    }

    #[test]
    fn ancestors_nearest_first() {
        assert_eq!(ancestors("a/b/c"), vec!["a/b/c", "a/b", "a"]);
        assert!(ancestors("").is_empty());
    }

    #[test]
    fn perms_inherit_from_nearest_group_then_global() {
        let g = groups();
        let r = rec("生产/华东/子组");
        assert!(
            !resolve_perm(&r, &g, "ssh_exec", true),
            "最近分组 华东 禁 ssh_exec"
        );
        assert!(
            !resolve_perm(&r, &g, "sftp_write", true),
            "祖先 生产 禁 sftp_write"
        );
        assert!(
            resolve_perm(&r, &g, "sftp_read", true),
            "无分组覆盖回落全局"
        );
        let mut r = r;
        r.mcp_perms.insert("ssh_exec".into(), true);
        assert!(resolve_perm(&r, &g, "ssh_exec", false), "会话覆盖优先");
    }

    #[test]
    fn level_and_production_inherit_independently() {
        let g = groups();
        let e = resolve_effective(&rec("生产/华东"), &g, AccessLevel::Full);
        assert_eq!(e.level, AccessLevel::Standard, "最近分组档位");
        assert!(e.production, "华东未设生产标记，继承 生产");
        let e = resolve_effective(&rec("测试"), &g, AccessLevel::Full);
        assert_eq!(
            e,
            Effective {
                level: AccessLevel::Full,
                production: false
            }
        );
        let mut r = rec("生产");
        r.mcp_level = Some("full".into());
        r.mcp_production = Some(false);
        let e = resolve_effective(&r, &g, AccessLevel::Readonly);
        assert_eq!(
            e,
            Effective {
                level: AccessLevel::Full,
                production: false
            }
        );
    }
}
