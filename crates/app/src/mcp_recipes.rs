//! Recipes：内置的参数化运维操作，每条带风险等级。
//!
//! 模板固定、参数按白名单正则校验后单引号代入，命令形状由构造保证——因此 read 级
//! recipe 可在只读档放行（如 `docker ps`、`systemctl status`，分类器无法静态证明只读）；
//! change 级按「无法证明只读」处理（只读档拒绝、标准档确认、完全档放行、生产会话确认）；
//! danger 级在非只读档一律弹窗确认。

use core_policy::{AccessLevel, Verdict};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Risk {
    Read,
    Change,
    Danger,
}

impl Risk {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Change => "change",
            Self::Danger => "danger",
        }
    }
}

struct Param {
    name: &'static str,
    description: &'static str,
    /// 整串匹配的白名单正则（外层自动加 ^(?:...)$）
    pattern: &'static str,
    default: Option<&'static str>,
}

pub(crate) struct Recipe {
    pub(crate) id: &'static str,
    description: &'static str,
    pub(crate) risk: Risk,
    params: &'static [Param],
    /// `{name}` 占位符，代入时加单引号
    template: &'static str,
}

/// systemd 单元名：不允许 - 开头（防选项注入）
const UNIT: &str = r"[A-Za-z0-9@_.:][A-Za-z0-9@._:-]{0,127}";
/// 绝对路径：/ 开头天然不会被当成选项
const ABS_PATH: &str = r"/[A-Za-z0-9._/@+=,-]{0,1023}";
const CONTAINER: &str = r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}";
const COUNT: &str = r"[1-9][0-9]{0,4}";

const fn unit(description: &'static str) -> Param {
    Param {
        name: "name",
        description,
        pattern: UNIT,
        default: None,
    }
}

const fn lines(default: &'static str) -> Param {
    Param {
        name: "lines",
        description: "行数",
        pattern: COUNT,
        default: Some(default),
    }
}

const fn container() -> Param {
    Param {
        name: "container",
        description: "容器名或 ID",
        pattern: CONTAINER,
        default: None,
    }
}

static RECIPES: &[Recipe] = &[
    Recipe {
        id: "system_overview",
        description: "系统概况：内核、负载、内存、磁盘",
        risk: Risk::Read,
        params: &[],
        template: "uname -a; uptime; free -h; df -h",
    },
    Recipe {
        id: "disk_usage",
        description: "目录下一级子项占用排行（不跨文件系统，前 25）",
        risk: Risk::Read,
        params: &[Param {
            name: "path",
            description: "绝对路径",
            pattern: ABS_PATH,
            default: Some("/"),
        }],
        template: "du -xh --max-depth=1 {path} 2>/dev/null | sort -rh | head -n 25",
    },
    Recipe {
        id: "top_processes",
        description: "按 CPU 或内存排序的进程",
        risk: Risk::Read,
        params: &[
            Param {
                name: "sort",
                description: "cpu 或 mem",
                pattern: "cpu|mem",
                default: Some("cpu"),
            },
            Param {
                name: "count",
                description: "显示行数",
                pattern: COUNT,
                default: Some("15"),
            },
        ],
        template: "ps aux --sort=-%{sort} | head -n {count}",
    },
    Recipe {
        id: "listening_ports",
        description: "监听中的 TCP/UDP 端口及进程",
        risk: Risk::Read,
        params: &[],
        template: "ss -tulnp",
    },
    Recipe {
        id: "failed_services",
        description: "失败的 systemd 单元",
        risk: Risk::Read,
        params: &[],
        template: "systemctl --failed --no-pager",
    },
    Recipe {
        id: "service_status",
        description: "systemd 服务状态",
        risk: Risk::Read,
        params: &[unit("systemd 单元名，如 nginx")],
        template: "systemctl status {name} --no-pager -l",
    },
    Recipe {
        id: "service_logs",
        description: "systemd 服务最近日志",
        risk: Risk::Read,
        params: &[unit("systemd 单元名"), lines("200")],
        template: "journalctl -u {name} -n {lines} --no-pager",
    },
    Recipe {
        id: "recent_errors",
        description: "系统日志中最近的 err 及以上级别记录",
        risk: Risk::Read,
        params: &[lines("100")],
        template: "journalctl -p err -n {lines} --no-pager",
    },
    Recipe {
        id: "tail_file",
        description: "文件末尾若干行",
        risk: Risk::Read,
        params: &[
            Param {
                name: "path",
                description: "绝对路径",
                pattern: ABS_PATH,
                default: None,
            },
            lines("200"),
        ],
        template: "tail -n {lines} {path}",
    },
    Recipe {
        id: "docker_ps",
        description: "全部 Docker 容器及状态",
        risk: Risk::Read,
        params: &[],
        template: "docker ps -a",
    },
    Recipe {
        id: "docker_logs",
        description: "Docker 容器最近日志（含 stderr）",
        risk: Risk::Read,
        params: &[container(), lines("200")],
        template: "docker logs --tail {lines} {container} 2>&1",
    },
    Recipe {
        id: "service_restart",
        description: "重启 systemd 服务",
        risk: Risk::Change,
        params: &[unit("systemd 单元名")],
        template: "systemctl restart {name}",
    },
    Recipe {
        id: "service_reload",
        description: "重载 systemd 服务配置（不中断连接）",
        risk: Risk::Change,
        params: &[unit("systemd 单元名")],
        template: "systemctl reload {name}",
    },
    Recipe {
        id: "docker_restart",
        description: "重启 Docker 容器",
        risk: Risk::Change,
        params: &[container()],
        template: "docker restart {container}",
    },
    Recipe {
        id: "kill_process",
        description: "向进程发信号（默认 TERM）",
        risk: Risk::Danger,
        params: &[
            Param {
                name: "pid",
                description: "进程号",
                pattern: r"[1-9][0-9]{0,9}",
                default: None,
            },
            Param {
                name: "signal",
                description: "TERM / KILL / HUP / INT",
                pattern: "TERM|KILL|HUP|INT",
                default: Some("TERM"),
            },
        ],
        template: "kill -{signal} {pid}",
    },
    Recipe {
        id: "journal_vacuum",
        description: "清理早于 N 天的 systemd 日志",
        risk: Risk::Danger,
        params: &[Param {
            name: "days",
            description: "保留天数",
            pattern: COUNT,
            default: Some("14"),
        }],
        template: "journalctl --vacuum-time={days}d",
    },
];

pub(crate) fn list_json() -> Value {
    let items: Vec<Value> = RECIPES
        .iter()
        .map(|r| {
            let params: Vec<Value> = r
                .params
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name,
                        "description": p.description,
                        "pattern": p.pattern,
                        "default": p.default,
                        "required": p.default.is_none(),
                    })
                })
                .collect();
            json!({
                "id": r.id,
                "description": r.description,
                "risk": r.risk.as_str(),
                "params": params,
                "command": r.template,
            })
        })
        .collect();
    json!({ "recipes": items })
}

/// 校验参数并渲染命令。未声明的参数报错（防拼写错误被静默忽略）
pub(crate) fn render(id: &str, params: &Value) -> Result<(&'static Recipe, String), String> {
    let recipe = RECIPES
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("未知 recipe：{id}（用 recipe_list 查看）"))?;
    let empty = Map::new();
    let given = match params {
        Value::Null => &empty,
        Value::Object(m) => m,
        _ => return Err("params 必须是对象".into()),
    };
    if let Some(k) = given
        .keys()
        .find(|k| !recipe.params.iter().any(|p| p.name == k.as_str()))
    {
        return Err(format!("recipe {id} 没有参数 {k}"));
    }
    let mut cmd = recipe.template.to_string();
    for p in recipe.params {
        let value = match given.get(p.name) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(_) => return Err(format!("参数 {} 必须是字符串或数字", p.name)),
            None => p
                .default
                .ok_or_else(|| format!("recipe {id} 缺少参数 {}", p.name))?
                .to_string(),
        };
        let re = regex::Regex::new(&format!("^(?:{})$", p.pattern)).map_err(|e| e.to_string())?;
        if !re.is_match(&value) || value.contains('\'') {
            return Err(format!(
                "参数 {} 的值不合法（须匹配 {}）",
                p.name, p.pattern
            ));
        }
        cmd = cmd.replace(&format!("{{{}}}", p.name), &format!("'{value}'"));
    }
    Ok((recipe, cmd))
}

/// 风险等级 × 档位 → 判定（取代分类器；见模块注释）
pub(crate) fn verdict(risk: Risk, level: AccessLevel, production: bool) -> Verdict {
    match (risk, level) {
        (Risk::Read, _) => Verdict::Allow,
        (_, AccessLevel::Readonly) => Verdict::Deny,
        (Risk::Danger, _) => Verdict::RequireConfirmation,
        (Risk::Change, _) if production => Verdict::RequireConfirmation,
        (Risk::Change, AccessLevel::Standard) => Verdict::RequireConfirmation,
        (Risk::Change, AccessLevel::Full) => Verdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn templates_and_params_are_consistent() {
        let mut ids = std::collections::HashSet::new();
        for r in RECIPES {
            assert!(ids.insert(r.id), "重复 id {}", r.id);
            for p in r.params {
                assert!(
                    r.template.contains(&format!("{{{}}}", p.name)),
                    "{} 未使用参数 {}",
                    r.id,
                    p.name
                );
                if let Some(d) = p.default {
                    let re = regex::Regex::new(&format!("^(?:{})$", p.pattern)).unwrap();
                    assert!(re.is_match(d), "{} 默认值不合法", r.id);
                }
            }
            // 代入全部参数后不应残留占位符
            let sample: Map<String, Value> = r
                .params
                .iter()
                .map(|p| {
                    let v = p.default.unwrap_or(match p.pattern {
                        ABS_PATH => "/var/log/syslog",
                        _ if p.name == "pid" => "42",
                        _ => "nginx",
                    });
                    (p.name.to_string(), json!(v))
                })
                .collect();
            let (_, cmd) = render(r.id, &Value::Object(sample)).unwrap();
            assert!(!cmd.contains('{'), "{} 残留占位符: {cmd}", r.id);
        }
    }

    #[test]
    fn render_quotes_and_applies_defaults() {
        let (r, cmd) = render("service_logs", &json!({ "name": "nginx" })).unwrap();
        assert_eq!(r.risk, Risk::Read);
        assert_eq!(cmd, "journalctl -u 'nginx' -n '200' --no-pager");
        let (_, cmd) = render("kill_process", &json!({ "pid": 1234 })).unwrap();
        assert_eq!(cmd, "kill -'TERM' '1234'");
        let (_, cmd) = render("system_overview", &Value::Null).unwrap();
        assert_eq!(cmd, "uname -a; uptime; free -h; df -h");
    }

    #[test]
    fn render_rejects_injection_and_bad_input() {
        for bad in ["nginx; rm -rf /", "-h", "a b", "x'y", "$(id)", "a`id`", ""] {
            assert!(
                render("service_status", &json!({ "name": bad })).is_err(),
                "应拒绝 {bad:?}"
            );
        }
        assert!(render("tail_file", &json!({ "path": "relative/x" })).is_err());
        assert!(render("tail_file", &json!({ "path": "/a\n/b" })).is_err());
        assert!(render("tail_file", &json!({})).is_err(), "缺必填参数");
        assert!(render("top_processes", &json!({ "sort": "io" })).is_err());
        assert!(
            render("top_processes", &json!({ "srot": "cpu" })).is_err(),
            "未知参数"
        );
        assert!(render("nope", &json!({})).is_err());
        assert!(render("docker_ps", &json!([])).is_err());
    }

    #[test]
    fn verdict_matrix() {
        use AccessLevel::*;
        assert_eq!(verdict(Risk::Read, Readonly, true), Verdict::Allow);
        assert_eq!(verdict(Risk::Change, Readonly, false), Verdict::Deny);
        assert_eq!(
            verdict(Risk::Change, Standard, false),
            Verdict::RequireConfirmation
        );
        assert_eq!(verdict(Risk::Change, Full, false), Verdict::Allow);
        assert_eq!(
            verdict(Risk::Change, Full, true),
            Verdict::RequireConfirmation
        );
        assert_eq!(verdict(Risk::Danger, Readonly, false), Verdict::Deny);
        assert_eq!(
            verdict(Risk::Danger, Full, false),
            Verdict::RequireConfirmation
        );
    }
}
