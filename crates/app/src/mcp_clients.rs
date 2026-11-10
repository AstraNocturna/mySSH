//! MCP 客户端登记与断开检测：AI 客户端关掉后自动退出 AI 托管。
//!
//! initialize 时分配 `Mcp-Session-Id` 作为客户端号，之后每个请求带回该号。请求处理期间
//! 经 task-local 标记「当前客户端」（[`scope`] / [`current`]），AI 终端、后台命令、进行中的
//! 命令、审批项创建时据此记下归属，镜像过的会话也记在客户端名下。
//!
//! 判定断开：
//! - 长连接客户端（开着 GET /mcp 事件流）：流全部断开且 [`STREAM_GRACE`] 内未重连，
//!   或发来 DELETE —— 直接清理它名下的资源并通知前端；
//! - 没开流的客户端：[`IDLE_LIMIT`] 内没有任何请求、名下也没有在跑的命令 —— 弹框请用户
//!   确认是否清理（无法确证客户端已关闭）。开过流又断流、但仍在发请求的客户端降级到这一档。
//!
//! 用户共享给 AI 的标签不属于任何客户端：最后一个已登记客户端清理后一并收回。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use core_store::Store;

/// 事件流断开后等待重连的宽限
pub(crate) const STREAM_GRACE: Duration = Duration::from_secs(30);
/// 没开流的客户端：多久没有请求视为可能已关闭
pub(crate) const IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// 到期巡检间隔
pub(crate) const REAP_INTERVAL: Duration = Duration::from_secs(5);

tokio::task_local! {
    static CURRENT: Option<String>;
}

/// 当前请求所属的客户端号（请求作用域外、旧客户端不带会话号时为 None）
pub(crate) fn current() -> Option<String> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

/// 在客户端作用域内处理一次请求
pub(crate) async fn scope<F: Future>(client: Option<String>, f: F) -> F::Output {
    CURRENT.scope(client, f).await
}

struct Client {
    name: String,
    last_seen: Instant,
    /// 当前打开的事件流数
    streams: usize,
    /// 处于长连接判定档（开着流，或断流后还没发过请求）
    long_lived: bool,
    /// 最后一条流断开的时刻
    gone_since: Option<Instant>,
    /// 客户端发来 DELETE
    ended: bool,
    /// 已弹「是否清理」确认框，等用户回答
    prompting: bool,
    /// 镜像过命令的会话档案 id
    mirrors: HashSet<String>,
}

impl Client {
    fn new(name: String, now: Instant) -> Self {
        Self {
            name,
            last_seen: now,
            streams: 0,
            long_lived: false,
            gone_since: None,
            ended: false,
            prompting: false,
            mirrors: HashSet::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Due {
    /// 确认已断开（ended = 客户端主动发 DELETE）
    Gone { id: String, ended: bool },
    /// 空闲到期，需用户确认
    Idle { id: String },
}

#[derive(Default)]
pub struct McpClients {
    map: Mutex<HashMap<String, Client>>,
}

fn random_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl McpClients {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// initialize：登记新客户端，返回客户端号（即 Mcp-Session-Id）
    pub(crate) fn register(&self, name: &str) -> String {
        let id = random_id();
        self.map
            .lock()
            .insert(id.clone(), Client::new(name.to_string(), Instant::now()));
        id
    }

    /// 收到请求。未知号（mySSH 重启后客户端沿用旧号）直接收编；仍开流的客户端发请求
    /// 只刷新时刻，断流后还在发请求说明它没关、只是不再开流，降级到空闲判定档。
    /// 返回 true = 此前正在等用户确认是否清理（调用方通知前端关掉确认框）。
    pub(crate) fn touch(&self, id: &str) -> bool {
        let now = Instant::now();
        let mut map = self.map.lock();
        let c = map
            .entry(id.to_string())
            .or_insert_with(|| Client::new(String::new(), now));
        c.last_seen = now;
        if c.streams == 0 {
            c.long_lived = false;
            c.gone_since = None;
        }
        std::mem::take(&mut c.prompting)
    }

    /// GET /mcp 事件流打开；守卫 drop 即视为该流断开
    pub(crate) fn stream_open(self: &Arc<Self>, id: &str) -> StreamGuard {
        let now = Instant::now();
        {
            let mut map = self.map.lock();
            let c = map
                .entry(id.to_string())
                .or_insert_with(|| Client::new(String::new(), now));
            c.streams += 1;
            c.long_lived = true;
            c.gone_since = None;
            c.last_seen = now;
        }
        StreamGuard {
            clients: self.clone(),
            id: id.to_string(),
        }
    }

    /// DELETE /mcp：客户端主动结束会话
    pub(crate) fn end(&self, id: &str) {
        if let Some(c) = self.map.lock().get_mut(id) {
            c.ended = true;
        }
    }

    /// 当前客户端在该会话上镜像过命令（断开时据此关闭镜像分屏）
    pub(crate) fn note_mirror(&self, session_id: &str) {
        let Some(id) = current() else { return };
        if let Some(c) = self.map.lock().get_mut(&id) {
            c.mirrors.insert(session_id.to_string());
        }
    }

    pub(crate) fn name(&self, id: &str) -> String {
        self.map
            .lock()
            .get(id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    pub(crate) fn is_prompting(&self, id: &str) -> bool {
        self.map.lock().get(id).is_some_and(|c| c.prompting)
    }

    /// 到期判定；busy(id) = 名下有在跑的命令（空闲计时随之顺延）。
    /// 空闲到期项同时置 prompting，避免重复弹框。
    pub(crate) fn due(&self, now: Instant, busy: impl Fn(&str) -> bool) -> Vec<Due> {
        let mut out = Vec::new();
        for (id, c) in self.map.lock().iter_mut() {
            if c.ended {
                out.push(Due::Gone {
                    id: id.clone(),
                    ended: true,
                });
            } else if c.long_lived {
                let gone = c.streams == 0
                    && c.gone_since
                        .is_some_and(|t| now.saturating_duration_since(t) >= STREAM_GRACE);
                if gone {
                    out.push(Due::Gone {
                        id: id.clone(),
                        ended: false,
                    });
                }
            } else if busy(id) {
                c.last_seen = now;
            } else if !c.prompting && now.saturating_duration_since(c.last_seen) >= IDLE_LIMIT {
                c.prompting = true;
                out.push(Due::Idle { id: id.clone() });
            }
        }
        out
    }

    /// 用户选择保留：空闲计时重新开始
    pub(crate) fn keep(&self, id: &str) {
        if let Some(c) = self.map.lock().get_mut(id) {
            c.prompting = false;
            c.last_seen = Instant::now();
        }
    }

    /// 注销客户端；返回其名称与镜像过的会话
    fn remove(&self, id: &str) -> (String, HashSet<String>) {
        self.map
            .lock()
            .remove(id)
            .map(|c| (c.name, c.mirrors))
            .unwrap_or_default()
    }

    /// 仍在登记中的客户端镜像过的会话（这些会话的镜像分屏不关）
    fn mirrors_in_use(&self) -> HashSet<String> {
        self.map
            .lock()
            .values()
            .flat_map(|c| c.mirrors.iter().cloned())
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.map.lock().is_empty()
    }

    #[cfg(test)]
    pub(crate) fn streams(&self, id: &str) -> Option<usize> {
        self.map.lock().get(id).map(|c| c.streams)
    }

    /// MCP 服务停止：清空登记；返回正在等确认的客户端号（前端据此关确认框）
    pub(crate) fn clear(&self) -> Vec<String> {
        self.map
            .lock()
            .drain()
            .filter(|(_, c)| c.prompting)
            .map(|(id, _)| id)
            .collect()
    }
}

/// 一条打开中的事件流
pub(crate) struct StreamGuard {
    clients: Arc<McpClients>,
    id: String,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Some(c) = self.clients.map.lock().get_mut(&self.id) {
            c.streams = c.streams.saturating_sub(1);
            if c.streams == 0 {
                c.gone_since = Some(Instant::now());
            }
        }
    }
}

/// 清理客户端名下资源所需的应用状态
pub(crate) struct ReleaseCtx<'a> {
    pub clients: &'a McpClients,
    pub ui: &'a crate::mcp_ui::McpUiHub,
    pub terms: &'a crate::mcp_terminal::McpTerminalState,
    pub jobs: &'a crate::mcp_jobs::McpJobs,
    pub store: &'a Arc<Store>,
}

/// 客户端名下的资源数（空闲确认框展示用）
pub(crate) fn owned_counts(ctx: &ReleaseCtx<'_>, id: &str) -> Value {
    let approvals = ctx.ui.owned_approvals(id);
    let execs = ctx.ui.owned_execs(id);
    let jobs = ctx.jobs.owned_running(id);
    let terminals = ctx.terms.owned_count(id);
    let mirrors = ctx
        .clients
        .map
        .lock()
        .get(id)
        .map_or(0, |c| c.mirrors.len());
    json!({
        "approvals": approvals,
        "execs": execs,
        "jobs": jobs,
        "terminals": terminals,
        "mirrors": mirrors,
        "total": approvals + execs + jobs + terminals + mirrors,
    })
}

/// 退出某客户端的 AI 托管：拒绝它的待确认项、停止它在跑的命令、关闭它的终端
///（UI 标签只收回控制）、关闭只有它用过的镜像分屏；它是最后一个客户端时，用户共享的
/// 标签一并收回。reason：disconnected / ended / idle。返回清理摘要（已推给前端）。
pub(crate) async fn release(ctx: &ReleaseCtx<'_>, id: &str, reason: &str) -> Value {
    let (name, mirrors) = ctx.clients.remove(id);
    let approvals = ctx.ui.drop_owned_approvals(id);
    ctx.ui.forget_owner_rules(id);
    let execs = ctx.ui.cancel_owned_execs(id);
    let jobs = ctx.jobs.stop_owned(id);
    let mut tabs = ctx.terms.close_owned(Some(id)).await;
    if ctx.clients.is_empty() {
        tabs.extend(ctx.terms.close_owned(None).await);
    }
    let terminals = tabs.len();
    for tab in tabs.into_iter().flatten() {
        // 不带 closeTab：前端只摘 AI 标记，标签留给用户
        ctx.ui
            .emit(&json!({ "type": "agent_terminal_closed", "tabId": tab }));
    }
    let in_use = ctx.clients.mirrors_in_use();
    let mut closed_mirrors = 0;
    for sid in mirrors.difference(&in_use) {
        ctx.ui
            .emit(&json!({ "type": "mirror_close", "sessionId": sid }));
        closed_mirrors += 1;
    }
    let summary = json!({
        "type": "ai_client_released",
        "clientId": id,
        "name": name,
        "reason": reason,
        "approvals": approvals,
        "execs": execs,
        "jobs": jobs,
        "terminals": terminals,
        "mirrors": closed_mirrors,
    });
    let total = approvals + execs + jobs + terminals + closed_mirrors;
    if total > 0 {
        ctx.ui.emit(&summary);
        ctx.ui.activity_changed();
    }
    let _ = ctx
        .store
        .audit()
        .append(
            core_store::Actor::Mcp,
            None,
            "mcp_client_released",
            &summary,
        )
        .await;
    tracing::info!(client = %name, reason, total, "MCP 客户端已断开，退出 AI 托管");
    summary
}

/// 巡检一轮：断开的直接清理，空闲到期的请前端确认（名下没有资源的静默注销）
pub(crate) async fn reap(ctx: &ReleaseCtx<'_>, now: Instant) {
    let due = ctx.clients.due(now, |id| {
        ctx.ui.owned_execs(id) > 0 || ctx.jobs.owned_running(id) > 0
    });
    for d in due {
        match d {
            Due::Gone { id, ended } => {
                release(ctx, &id, if ended { "ended" } else { "disconnected" }).await;
            }
            Due::Idle { id } => {
                let counts = owned_counts(ctx, &id);
                if counts["total"].as_u64() == Some(0) {
                    ctx.clients.remove(&id);
                    continue;
                }
                let mut frame = json!({
                    "type": "ai_client_idle",
                    "clientId": id,
                    "name": ctx.clients.name(&id),
                    "idleMinutes": IDLE_LIMIT.as_secs() / 60,
                });
                if let (Some(f), Value::Object(extra)) = (frame.as_object_mut(), counts) {
                    f.extend(extra);
                }
                // 窗口不在线：保留，下一个空闲周期再问
                if !ctx.ui.emit(&frame) {
                    ctx.clients.keep(&id);
                }
            }
        }
    }
}

/// 用户对空闲确认框的回答；客户端期间又发过请求（确认框已失效）时忽略
#[tauri::command]
pub async fn mcp_client_idle_answer(
    client_id: String,
    release_it: bool,
    clients: tauri::State<'_, Arc<McpClients>>,
    ui: tauri::State<'_, Arc<crate::mcp_ui::McpUiHub>>,
    terms: tauri::State<'_, Arc<crate::mcp_terminal::McpTerminalState>>,
    jobs: tauri::State<'_, Arc<crate::mcp_jobs::McpJobs>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<(), String> {
    if !clients.is_prompting(&client_id) {
        return Ok(());
    }
    if release_it {
        let ctx = ReleaseCtx {
            clients: &clients,
            ui: &ui,
            terms: &terms,
            jobs: &jobs,
            store: &sessions.store,
        };
        release(&ctx, &client_id, "idle").await;
    } else {
        clients.keep(&client_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // 测试代码豁免 unwrap/expect（工作区红线仅约束非测试代码）
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn stream_client_gone_after_grace_and_reconnect_resets() {
        let c = McpClients::new();
        let id = c.register("claude-code");
        let g = c.stream_open(&id);
        let now = Instant::now();
        assert!(
            c.due(now + STREAM_GRACE * 2, |_| false).is_empty(),
            "流开着"
        );
        drop(g);
        assert!(
            c.due(Instant::now(), |_| false).is_empty(),
            "宽限期内不算断开"
        );
        let _g2 = c.stream_open(&id);
        assert!(
            c.due(Instant::now() + STREAM_GRACE * 2, |_| false)
                .is_empty(),
            "宽限期内重连"
        );
        drop(_g2);
        assert_eq!(
            c.due(Instant::now() + STREAM_GRACE, |_| false),
            vec![Due::Gone {
                id: id.clone(),
                ended: false
            }]
        );
    }

    #[test]
    fn delete_ends_immediately() {
        let c = McpClients::new();
        let id = c.register("opencode");
        c.end(&id);
        assert_eq!(
            c.due(Instant::now(), |_| false),
            vec![Due::Gone { id, ended: true }]
        );
    }

    #[test]
    fn idle_needs_limit_not_busy_and_prompts_once() {
        let c = McpClients::new();
        let id = c.register("omp");
        let later = Instant::now() + IDLE_LIMIT;
        assert!(
            c.due(later - Duration::from_secs(1), |_| false).is_empty(),
            "未满空闲时长"
        );
        assert!(c.due(later, |_| true).is_empty(), "有命令在跑不算空闲");
        // 忙碌顺延了空闲计时
        assert!(c.due(later + Duration::from_secs(1), |_| false).is_empty());
        let later = later + IDLE_LIMIT;
        assert_eq!(c.due(later, |_| false), vec![Due::Idle { id: id.clone() }]);
        assert!(c.due(later, |_| false).is_empty(), "等确认期间不重复弹");
        assert!(c.touch(&id), "客户端又发请求：确认框作废");
        assert!(!c.is_prompting(&id));
    }

    #[test]
    fn request_after_stream_loss_degrades_to_idle_mode() {
        let c = McpClients::new();
        let id = c.register("x");
        drop(c.stream_open(&id));
        c.touch(&id);
        assert!(
            c.due(Instant::now() + STREAM_GRACE * 2, |_| false)
                .is_empty(),
            "仍在发请求的客户端不按断流清理"
        );
        assert_eq!(
            c.due(Instant::now() + IDLE_LIMIT, |_| false),
            vec![Due::Idle { id }]
        );
    }

    #[test]
    fn unknown_id_is_adopted() {
        let c = McpClients::new();
        assert!(!c.touch("old-session"));
        assert_eq!(c.name("old-session"), "");
        assert!(!c.is_empty());
    }

    /// 清理只动该客户端名下的：它的审批被拒并关弹窗、它的命令被停，别的客户端不受影响
    #[tokio::test]
    async fn release_only_touches_own_resources() {
        let dir = std::env::temp_dir().join(format!("myssh-cli-rel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.unwrap());
        let ui = crate::mcp_ui::McpUiHub::new();
        let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
        let fs = frames.clone();
        ui.set_sink(Arc::new(move |v: &Value| {
            fs.lock().push(v.clone());
            true
        }));
        let terms = crate::mcp_terminal::McpTerminalState::new();
        let jobs = crate::mcp_jobs::McpJobs::new();
        let clients = McpClients::new();
        let a = clients.register("claude-code");
        let b = clients.register("codex");
        let (a_req, _a_exec) = scope(Some(a.clone()), async {
            (
                ui.request("s1", "rm -rf /tmp/x", json!({})).unwrap(),
                ui.track_exec("s1", "ssh_exec", "sleep 100"),
            )
        })
        .await;
        let b_req = scope(Some(b.clone()), async {
            ui.request("s1", "rm -rf /tmp/x", json!({})).unwrap()
        })
        .await;
        assert_ne!(a_req, b_req, "不同客户端的同一命令各自审批");
        let ctx = ReleaseCtx {
            clients: &clients,
            ui: &ui,
            terms: &terms,
            jobs: &jobs,
            store: &store,
        };
        assert_eq!(owned_counts(&ctx, &a)["total"], 2);
        let sum = release(&ctx, &a, "disconnected").await;
        assert_eq!(sum["approvals"], 1);
        assert_eq!(sum["execs"], 1);
        assert_eq!(sum["name"], "claude-code");
        assert!(matches!(
            ui.wait(&a_req, Duration::from_millis(10)).await,
            crate::mcp_ui::ApprovalWait::Denied
        ));
        let got = frames.lock().clone();
        assert!(got
            .iter()
            .any(|f| f["type"] == "approval_closed" && f["id"] == a_req.as_str()));
        assert!(!got
            .iter()
            .any(|f| f["id"] == b_req.as_str() && f["type"] == "approval_closed"));
        assert!(got.iter().any(|f| f["type"] == "ai_client_released"));
        assert_eq!(ui.owned_approvals(&b), 1, "别的客户端的审批保留");
        assert_eq!(clients.name(&b), "codex");
        assert_eq!(clients.streams(&a), None, "已注销");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn current_follows_request_scope() {
        assert_eq!(current(), None);
        let got = scope(Some("c1".into()), async { current() }).await;
        assert_eq!(got.as_deref(), Some("c1"));
        let c = McpClients::new();
        let id = c.register("x");
        scope(Some(id.clone()), async { c.note_mirror("s1") }).await;
        c.note_mirror("s2");
        assert_eq!(c.remove(&id).1, HashSet::from(["s1".to_string()]));
    }
}
