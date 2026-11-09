//! MCP 服务端：让 AI 客户端（Claude Code 等）在已保存的 SSH 会话上执行命令。
//!
//! 传输为 MCP Streamable HTTP（2025-03-26 ~ 2025-11-25 子集）：单端点 POST /mcp，
//! 每次请求独立 JSON 响应（不开 SSE 流）；仅绑定 127.0.0.1。
//! 工具面：list_sessions（列会话档案，不含凭据）、ssh_exec（一次性 Bulk
//! 连接跑命令，收集 stdout/stderr/exit code）、terminal_*（open/send/read/
//! close 交互式有状态 shell，实现见 mcp_terminal.rs）、sftp_*（home/list/stat/read/
//! write/mkdir/delete/rename/chmod，与 UI 共享 SFTP Bulk 连接池，写操作落
//! audit 且 actor=mcp；upload/download 入队 UI 同款 TransferQueue 后台执行，
//! 进度用 transfer_list 轮询）。鉴权为 Bearer token
//!（设置键 mcp.token，空 = 不鉴权；首次启用自动生成随机 token）。
//! 工具分组权限由设置键 mcp.allow.{list_sessions,ssh_exec,sftp_read,
//! sftp_write,sftp_transfer,terminal} 控制（缺省全开，唯 terminal 缺省关闭：
//! 交互字节流无法过命令分类器，开启即等于放开任意命令；tools/list 同步过滤，
//! tools/call 命中禁用组返回 isError）。会话编辑器可逐组覆盖
//!（sessions.mcp_perms 稀疏映射），会话级覆盖优先于全局。
//! ssh_exec 命令经 core-policy 分类器判定，档位（mcp.level：readonly/standard/full）与
//! 生产标记按 会话 → 分组策略（mcp.group_policies）→ 全局 解析（mcp_policy.rs）；
//! 需人工确认者推应用内审批弹窗（mcp_ui.rs），用户未决返回 pending，窗口不可用 fail-closed；
//! 并按会话限速（设置键 mcp.rate_limit_per_minute，默认 60/分钟）；全部尝试落审计。
//! 生命周期由 lib.rs 装配：setup 时按 mcp.enabled/mcp.port/mcp.token 启动，
//! mcp_restart 命令供设置变更后重载。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

use core_policy::{PolicyEngine, RateLimiter, Verdict};
use core_sftp::OnExists;
use core_store::Store;

/// 默认监听端口（设置键 mcp.port 未配时）
const DEFAULT_PORT: u16 = 17345;
/// ssh_exec 默认超时
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// ssh_exec 超时上限
const MAX_TIMEOUT_MS: u64 = 120_000;
/// stdout/stderr 各自的截断上限（超限保留头尾各一半）
const OUTPUT_CAP: usize = 64 * 1024;
/// 每会话每分钟 ssh_exec 命令数默认上限（设置键 mcp.rate_limit_per_minute）
const DEFAULT_RATE_PER_MINUTE: u32 = 60;
/// 停止时等待优雅退出的上限，超时后 abort
const STOP_GRACE: Duration = Duration::from_secs(2);

const SESSION_ID_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");
const PROTOCOL_VERSION_HEADER: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// 支持的协议版本（新→旧）。服务端无会话状态、不发服务端请求，各版本差异
///（批量请求移除、MCP-Protocol-Version 头、工具注解等）对本实现均为增量兼容。
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// initialize 版本协商：客户端版本受支持则回显，否则给出本端最新版本由客户端决定是否断开
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|v| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|s| **s == v))
        .copied()
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

/// MCP 服务运行参数（读自 core-store settings KV）
struct McpConfig {
    enabled: bool,
    port: u16,
    token: Option<String>,
    perms: McpPerms,
    rate_per_minute: u32,
}

/// 工具分组权限（设置键 mcp.allow.*，缺省 = 允许：旧库无这些键时行为不变）。
/// 粒度为分组而非单工具：18 个工具逐一切换噪音过大，读写分离已覆盖风险面。
#[derive(Clone, Copy)]
pub struct McpPerms {
    list_sessions: bool,
    ssh_exec: bool,
    /// sftp_home/list/stat/read
    sftp_read: bool,
    /// sftp_write/mkdir/delete/rename/chmod
    sftp_write: bool,
    /// sftp_upload/download（触及本地文件系统，单独成组）
    sftp_transfer: bool,
    /// terminal_*：交互字节流绕过命令分类器，单独成组且缺省关闭
    terminal: bool,
}

impl Default for McpPerms {
    fn default() -> Self {
        Self {
            list_sessions: true,
            ssh_exec: true,
            sftp_read: true,
            sftp_write: true,
            sftp_transfer: true,
            terminal: false,
        }
    }
}

/// 工具名 → 权限分组键（全局 mcp.allow.* 与会话 mcp_perms 覆盖共用同一组键名）
fn tool_group_key(name: &str) -> Option<&'static str> {
    match name {
        "list_sessions" => Some("list_sessions"),
        // 后台命令执行的同样是任意命令：与 ssh_exec 同组
        "ssh_exec" | "job_start" | "job_read" | "job_stop" | "job_list" | "recipe_list"
        | "recipe_run" => Some("ssh_exec"),
        "terminal_open" | "terminal_send" | "terminal_read" | "terminal_close"
        | "terminal_list" => Some("terminal"),
        "sftp_home" | "sftp_list" | "sftp_stat" | "sftp_read" | "file_view" | "file_grep"
        | "file_glob" | "file_backups" => Some("sftp_read"),
        "sftp_write" | "sftp_mkdir" | "sftp_delete" | "sftp_rename" | "sftp_chmod"
        | "file_edit" | "file_rollback" => Some("sftp_write"),
        "sftp_upload" | "sftp_download" | "sftp_transfer_list" => Some("sftp_transfer"),
        _ => None,
    }
}

impl McpPerms {
    /// 工具名 → 全局是否放行；未知名不拦截（交给原有「未知工具」分支）
    fn allows(&self, name: &str) -> bool {
        tool_group_key(name).is_none_or(|g| self.allows_group(g))
    }

    /// 按权限分组键判定（未知键放行）
    fn allows_group(&self, group: &str) -> bool {
        match group {
            "list_sessions" => self.list_sessions,
            "ssh_exec" => self.ssh_exec,
            "sftp_read" => self.sftp_read,
            "sftp_write" => self.sftp_write,
            "sftp_transfer" => self.sftp_transfer,
            "terminal" => self.terminal,
            _ => true,
        }
    }
}

/// 有效权限：会话级覆盖（sessions.mcp_perms）→ 最近祖先分组策略 → 全局快照。
/// 会话不存在时按全局放行——工具自身会报 NotFound。
async fn tool_allowed(st: &ServerState, name: &str, args: &Value) -> bool {
    let global = st.perms.allows(name);
    let Some(key) = tool_group_key(name) else {
        return global;
    };
    // terminal_send/read/close 只带 terminalId：按打开时的会话取覆盖，否则会话级 deny 失效
    let sid = match args.get("session_id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => match args
            .get("terminalId")
            .and_then(Value::as_str)
            .and_then(|id| st.terms.session_of(id))
            .or_else(|| {
                args.get("job_id")
                    .and_then(Value::as_str)
                    .and_then(|id| st.jobs.session_of(id))
            }) {
            Some(s) => s,
            None => return global,
        },
    };
    match st.store.sessions().get(&sid).await {
        Ok(rec) => {
            let groups = crate::mcp_policy::load_group_policies(&st.store).await;
            crate::mcp_policy::resolve_perm(&rec, &groups, key, global)
        }
        Err(_) => global,
    }
}

/// 用户共享标签前的预检：MCP 已启用，且该会话（会话 → 分组 → 全局）放行 terminal 组
pub(crate) async fn terminal_usable(store: &Store, session_id: Option<&str>) -> bool {
    let settings = store.settings();
    let get = async |key: &str| settings.get(key).await.ok().flatten();
    if !parse_bool_setting(get("mcp.enabled").await.as_deref()) {
        return false;
    }
    let global = parse_bool_setting(get("mcp.allow.terminal").await.as_deref());
    let Some(sid) = session_id else {
        return global;
    };
    match store.sessions().get(sid).await {
        Ok(rec) => {
            let groups = crate::mcp_policy::load_group_policies(store).await;
            crate::mcp_policy::resolve_perm(&rec, &groups, "terminal", global)
        }
        Err(_) => global,
    }
}

/// mcp.allow.* 缺省视为允许（向后兼容），其余走 bool 解析
fn parse_allow_setting(raw: Option<&str>) -> bool {
    raw.is_none_or(|r| parse_bool_setting(Some(r)))
}

/// 设置值容忍 JSON bool/string/number 与裸文本（settings_set 一律 JSON 编码，
/// 但容忍外部直写库的情况）。
fn parse_bool_setting(raw: Option<&str>) -> bool {
    match raw {
        None => false,
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Bool(b)) => b,
            Ok(Value::String(s)) => s == "true",
            _ => raw.trim() == "true",
        },
    }
}

fn parse_port_setting(raw: Option<&str>) -> u16 {
    let n = match raw {
        None => return DEFAULT_PORT,
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Number(n)) => n.as_u64(),
            Ok(Value::String(s)) => s.trim().parse::<u64>().ok(),
            _ => raw.trim().parse::<u64>().ok(),
        },
    };
    match n {
        Some(v) if (1..=65535).contains(&v) => v as u16,
        _ => DEFAULT_PORT,
    }
}

/// 每会话每分钟 ssh_exec 命令数上限（0/越界回落默认；容忍 JSON 编码与裸文本）
fn parse_rate_setting(raw: Option<&str>) -> u32 {
    let n = match raw {
        None => return DEFAULT_RATE_PER_MINUTE,
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Number(n)) => n.as_u64(),
            Ok(Value::String(s)) => s.trim().parse::<u64>().ok(),
            _ => raw.trim().parse::<u64>().ok(),
        },
    };
    match n {
        Some(v) if (1..=3600).contains(&v) => v as u32,
        _ => DEFAULT_RATE_PER_MINUTE,
    }
}

fn parse_token_setting(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let token = match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(s)) => s,
        _ => raw.trim().to_string(),
    };
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// 读 mcp.enabled / mcp.port / mcp.token、mcp.allow.* 权限键与 mcp.rate_limit_per_minute
async fn read_mcp_config(store: &Store) -> Result<McpConfig, String> {
    let settings = store.settings();
    let get = async |key: &str| settings.get(key).await.map_err(|e| e.to_string());
    let enabled = get("mcp.enabled").await?;
    let port = get("mcp.port").await?;
    let token = get("mcp.token").await?;
    let perms = McpPerms {
        list_sessions: parse_allow_setting(get("mcp.allow.list_sessions").await?.as_deref()),
        ssh_exec: parse_allow_setting(get("mcp.allow.ssh_exec").await?.as_deref()),
        sftp_read: parse_allow_setting(get("mcp.allow.sftp_read").await?.as_deref()),
        sftp_write: parse_allow_setting(get("mcp.allow.sftp_write").await?.as_deref()),
        sftp_transfer: parse_allow_setting(get("mcp.allow.sftp_transfer").await?.as_deref()),
        // 缺省关闭：未显式开启的旧库升级后 terminal_* 不可用
        terminal: parse_bool_setting(get("mcp.allow.terminal").await?.as_deref()),
    };
    let rate_per_minute = parse_rate_setting(get("mcp.rate_limit_per_minute").await?.as_deref());
    Ok(McpConfig {
        enabled: parse_bool_setting(enabled.as_deref()),
        port: parse_port_setting(port.as_deref()),
        token: parse_token_setting(token.as_deref()),
        perms,
        rate_per_minute,
    })
}

/// 生成 32 字节随机 hex token（rand 为 workspace 既有随机源）
fn gen_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// token 缺省时生成随机 token 并尝试入库（入库失败仅告警，本次仍生效）
async fn ensure_token(store: &Store, token: Option<String>) -> String {
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        return t;
    }
    let token = gen_token();
    let stored = serde_json::to_string(&Value::String(token.clone())).map_err(|e| e.to_string());
    match stored {
        Ok(text) => {
            if let Err(e) = store.settings().set("mcp.token", &text).await {
                tracing::warn!(error = %e, "MCP token 入库失败（本次运行仍使用生成的 token）");
            } else {
                tracing::info!("MCP 首次启用：已生成随机 token 存入 mcp.token");
            }
        }
        Err(e) => tracing::warn!(error = %e, "MCP token 序列化失败"),
    }
    token
}

/// MCP 服务依赖的应用级共享状态（随应用同生命周期，跨 MCP 重启保留）
#[derive(Clone)]
pub struct McpDeps {
    pub store: Arc<Store>,
    pub sftp: Arc<crate::sftp::SftpManagerState>,
    pub exec: Arc<crate::exec::ExecManagerState>,
    pub terms: Arc<crate::mcp_terminal::McpTerminalState>,
    pub ui: Arc<crate::mcp_ui::McpUiHub>,
    /// UI 终端管理器（agent 终端以标签打开 / 用户共享标签）
    pub term_mgr: Arc<crate::terminal::TerminalManager>,
    /// AI 文件改动备份（回滚用）
    pub backups: Arc<crate::mcp_files::BackupStore>,
    /// 后台命令注册表
    pub jobs: Arc<crate::mcp_jobs::McpJobs>,
    /// 会话录像（AI 终端自动录制）
    pub recs: Arc<crate::recording::RecordingStore>,
}

/// MCP 服务生命周期管理：持有 tokio task + shutdown watch
#[derive(Default)]
pub struct McpManager {
    inner: Mutex<McpInner>,
}

#[derive(Default)]
struct McpInner {
    shutdown: Option<watch::Sender<bool>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    port: u16,
    token: String,
    /// 交互式终端注册表（start 时挂入，stop 清场）
    terms: Option<Arc<crate::mcp_terminal::McpTerminalState>>,
    /// 前端交互枢纽（stop 时通知前端摘除 AI 控制标记）
    ui: Option<Arc<crate::mcp_ui::McpUiHub>>,
    /// 后台命令注册表（stop 时终止全部运行中的命令）
    jobs: Option<Arc<crate::mcp_jobs::McpJobs>>,
}

impl McpManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 启动服务（已有实例先停）。绑定失败返回清晰错误，不 panic。
    pub async fn start(
        self: &Arc<Self>,
        deps: McpDeps,
        port: u16,
        token: String,
        perms: McpPerms,
        rate_per_minute: u32,
    ) -> Result<(), String> {
        self.stop().await;
        let McpDeps {
            store,
            sftp,
            exec,
            terms,
            ui,
            term_mgr,
            backups,
            jobs,
            recs,
        } = deps;
        let state = Arc::new(ServerState {
            store,
            sftp,
            exec,
            terms: terms.clone(),
            ui: ui.clone(),
            term_mgr,
            backups,
            jobs: jobs.clone(),
            recs,
            token: token.clone(),
            perms,
            // 安全模型执行面（M0 只读档）：判定管线 + 每会话速率闸，随服务同生命周期
            policy: PolicyEngine::new(),
            rate: RateLimiter::new(rate_per_minute),
        });
        let router = Router::new()
            .route("/mcp", post(mcp_post))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("MCP 端口 127.0.0.1:{port} 绑定失败: {e}"))?;
        let (tx, mut rx) = watch::channel(false);
        let task = tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    loop {
                        match rx.changed().await {
                            // 发送端被丢弃同样视为停止信号
                            Ok(()) if *rx.borrow() => break,
                            // 初始值 false：等下一次变化；发送端被丢弃同样视为停止
                            Ok(()) => {}
                            Err(_) => break,
                        }
                    }
                })
                .await;
            if let Err(e) = result {
                tracing::error!(error = %e, "MCP HTTP 服务异常退出");
            }
        });
        let mut inner = self.inner.lock();
        inner.shutdown = Some(tx);
        inner.task = Some(task);
        inner.port = port;
        inner.token = token;
        inner.terms = Some(terms);
        inner.ui = Some(ui);
        inner.jobs = Some(jobs);
        tracing::info!(port, "MCP 服务已启动（127.0.0.1）");
        Ok(())
    }

    /// 停止服务：先发 shutdown 信号走优雅退出，超过 STOP_GRACE 则 abort
    pub async fn stop(&self) {
        let (tx, task, terms, ui, jobs) = {
            let mut inner = self.inner.lock();
            inner.port = 0;
            (
                inner.shutdown.take(),
                inner.task.take(),
                inner.terms.take(),
                inner.ui.take(),
                inner.jobs.take(),
            )
        };
        if let Some(jobs) = jobs {
            jobs.stop_all().await;
        }
        // 交互式终端清场：关通道 + abort 读任务，不留孤儿 Bulk 连接；UI 标签只收回控制
        if let Some(terms) = terms {
            for tab in terms.close_all().await {
                if let Some(ui) = &ui {
                    ui.emit(&json!({ "type": "agent_terminal_closed", "tabId": tab }));
                }
            }
        }
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
        if let Some(mut task) = task {
            match tokio::time::timeout(STOP_GRACE, &mut task).await {
                Ok(_) => tracing::info!("MCP 服务已停止"),
                Err(_) => {
                    // 有挂起连接拖住优雅退出时直接 abort（终端已清场，其余请求无状态）
                    task.abort();
                    tracing::warn!("MCP 服务优雅退出超时，强制结束");
                }
            }
        }
    }

    /// 当前状态：{running, port, token_set}
    pub fn status(&self) -> Value {
        let inner = self.inner.lock();
        // 句柄存在即视为运行中（stop 会取走句柄；服务崩溃属异常，下次 status 仍报 running）
        let running = inner.task.is_some();
        json!({
            "running": running,
            "port": if running { inner.port } else { 0 },
            "tokenSet": !inner.token.is_empty(),
        })
    }
}

pub async fn boot_from_settings(mgr: Arc<McpManager>, deps: McpDeps) {
    let cfg = match read_mcp_config(&deps.store).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "读取 MCP 设置失败，跳过启动");
            return;
        }
    };
    if !cfg.enabled {
        return;
    }
    let token = ensure_token(&deps.store, cfg.token).await;
    if let Err(e) = mgr
        .start(deps, cfg.port, token, cfg.perms, cfg.rate_per_minute)
        .await
    {
        tracing::error!(error = %e, "MCP 服务启动失败");
    }
}

pub async fn restart_from_settings(mgr: &Arc<McpManager>, deps: McpDeps) -> Result<Value, String> {
    mgr.stop().await;
    let cfg = read_mcp_config(&deps.store).await?;
    if cfg.enabled {
        let token = ensure_token(&deps.store, cfg.token).await;
        // 绑定失败直接回报前端，同时把 token 记入状态便于排查
        mgr.inner.lock().token = token.clone();
        mgr.start(deps, cfg.port, token, cfg.perms, cfg.rate_per_minute)
            .await?;
    }
    Ok(mgr.status())
}

#[tauri::command]
pub async fn mcp_status(state: tauri::State<'_, Arc<McpManager>>) -> Result<Value, String> {
    Ok(state.status())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mcp_restart(
    state: tauri::State<'_, Arc<McpManager>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
    sftp: tauri::State<'_, Arc<crate::sftp::SftpManagerState>>,
    exec: tauri::State<'_, Arc<crate::exec::ExecManagerState>>,
    terms: tauri::State<'_, Arc<crate::mcp_terminal::McpTerminalState>>,
    ui: tauri::State<'_, Arc<crate::mcp_ui::McpUiHub>>,
    term_mgr: tauri::State<'_, Arc<crate::terminal::TerminalManager>>,
    backups: tauri::State<'_, Arc<crate::mcp_files::BackupStore>>,
    jobs: tauri::State<'_, Arc<crate::mcp_jobs::McpJobs>>,
    recs: tauri::State<'_, Arc<crate::recording::RecordingStore>>,
) -> Result<Value, String> {
    let deps = McpDeps {
        store: sessions.store.clone(),
        sftp: sftp.inner().clone(),
        exec: exec.inner().clone(),
        terms: terms.inner().clone(),
        ui: ui.inner().clone(),
        term_mgr: term_mgr.inner().clone(),
        backups: backups.inner().clone(),
        jobs: jobs.inner().clone(),
        recs: recs.inner().clone(),
    };
    restart_from_settings(&state, deps).await
}

/// HTTP 服务共享状态
struct ServerState {
    store: Arc<Store>,
    /// SFTP 连接池（与 UI 共享；agent 高频小操作复用 Bulk 连接，不反复握手）
    sftp: Arc<crate::sftp::SftpManagerState>,
    /// Exec Transport 组（ssh_exec 复用；PR-14 起与 Monitor 共享，MCP 配额 4）
    exec: Arc<crate::exec::ExecManagerState>,
    /// 交互式终端注册表（terminal_* 工具；stop 时 close_all 清场）
    terms: Arc<crate::mcp_terminal::McpTerminalState>,
    /// 前端交互枢纽（危险命令审批弹窗、agent 终端开标签）
    ui: Arc<crate::mcp_ui::McpUiHub>,
    term_mgr: Arc<crate::terminal::TerminalManager>,
    /// AI 文件改动备份（file_edit / sftp_write / sftp_delete 前落盘，file_rollback 恢复）
    backups: Arc<crate::mcp_files::BackupStore>,
    /// 后台命令（job_*；命令门同 ssh_exec）
    jobs: Arc<crate::mcp_jobs::McpJobs>,
    recs: Arc<crate::recording::RecordingStore>,
    token: String,
    /// 工具分组权限（启动时快照；改设置后 mcp_restart 生效）
    perms: McpPerms,
    /// 命令分类器（安全模型第 1/2 条；M0 用内置白/黑名单，用户编辑版 M6 接入）
    policy: PolicyEngine,
    /// 每会话每分钟命令数上限（安全模型第 6 条）
    rate: RateLimiter,
}

/// POST /mcp 入口：鉴权 → JSON-RPC 解析 → 分发
async fn mcp_post(
    State(st): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !st.token.is_empty() {
        let ok = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| t == st.token);
        if !ok {
            tracing::warn!("MCP 请求鉴权失败");
            return respond(
                StatusCode::UNAUTHORIZED,
                Some(rpc_error(
                    Value::Null,
                    -32001,
                    "未授权：需 Authorization: Bearer <token>",
                )),
                &headers,
            );
        }
    }

    // 2025-06-18 起客户端在 initialize 之后的请求带 MCP-Protocol-Version；不支持的版本按规范回 400
    if let Some(v) = headers
        .get(&PROTOCOL_VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&v) {
            return respond(
                StatusCode::BAD_REQUEST,
                Some(rpc_error(
                    Value::Null,
                    -32600,
                    &format!("不支持的 MCP-Protocol-Version：{v}"),
                )),
                &headers,
            );
        }
    }

    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return respond(
                StatusCode::OK,
                Some(rpc_error(Value::Null, -32700, "JSON 解析失败")),
                &headers,
            );
        }
    };

    let id = req.get("id").cloned();
    let is_notification = id.is_none();
    let id = id.unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str);
    let Some(method) = method.filter(|_| req.is_object()) else {
        return respond(
            StatusCode::OK,
            Some(rpc_error(id, -32600, "非法 JSON-RPC 请求")),
            &headers,
        );
    };

    let outcome = dispatch(&st, method, &req).await;
    match outcome {
        Outcome::Accepted => respond(StatusCode::ACCEPTED, None, &headers),
        Outcome::Result(result) => {
            if is_notification {
                respond(StatusCode::ACCEPTED, None, &headers)
            } else {
                respond(
                    StatusCode::OK,
                    Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
                    &headers,
                )
            }
        }
        Outcome::Error(code, msg) => {
            if is_notification {
                respond(StatusCode::ACCEPTED, None, &headers)
            } else {
                respond(StatusCode::OK, Some(rpc_error(id, code, &msg)), &headers)
            }
        }
    }
}

/// 构造响应：JSON 体 + 回显 Mcp-Session-Id（若有）
fn respond(status: StatusCode, value: Option<Value>, req_headers: &HeaderMap) -> Response {
    let mut resp = match value {
        Some(v) => (status, Json(v)).into_response(),
        None => status.into_response(),
    };
    if let Some(hv) = req_headers
        .get(&SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| HeaderValue::from_str(s).ok())
    {
        resp.headers_mut().insert(&SESSION_ID_HEADER, hv);
    }
    resp
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

enum Outcome {
    /// 通知：202 空响应
    Accepted,
    /// JSON-RPC result 载荷
    Result(Value),
    /// JSON-RPC error（code, message）
    Error(i64, String),
}

/// JSON-RPC 方法分发（Streamable HTTP 子集，版本见 SUPPORTED_PROTOCOL_VERSIONS）
async fn dispatch(st: &ServerState, method: &str, req: &Value) -> Outcome {
    match method {
        "initialize" => Outcome::Result(json!({
            "protocolVersion": negotiate_protocol_version(
                req.pointer("/params/protocolVersion").and_then(Value::as_str)
            ),
            "serverInfo": { "name": "myssh", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "tools": {} },
        })),
        // 客户端就绪通知：直接 202
        "notifications/initialized" => Outcome::Accepted,
        "ping" => Outcome::Result(json!({})),
        "tools/list" => Outcome::Result(tools_list(&st.perms)),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str);
            let Some(name) = name else {
                return Outcome::Error(-32602, "tools/call 缺少 params.name".into());
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Outcome::Result(call_tool(st, name, &args).await)
        }
        // 其他通知直接吞掉；有 id 的未知方法报 -32601
        m if m.starts_with("notifications/") => Outcome::Accepted,
        _ => Outcome::Error(-32601, format!("未知方法：{method}")),
    }
}

/// 工具面 JSON Schema；按分组权限过滤后返回（禁用的工具对客户端不可见）
fn tools_list(perms: &McpPerms) -> Value {
    let mut v = json!({
        "tools": [
            {
                "name": "list_sessions",
                "description": "列出 mySSH 已保存的会话档案（id/name/host/port/user/group/kind，不含凭据），并给出每个会话对 AI 的有效授权（ai.level 档位、ai.production 生产标记、ai.tools 各工具组是否放行）、用户打开的标签数 openTabs，以及 AI 可直接使用的终端 aiTerminals（含用户共享给 AI 的标签）。调用其他工具前先看这里，避免对未放行的会话试错。",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "ssh_exec",
                "description": "在已保存的 SSH 会话上执行一条 shell 命令，返回 stdout/stderr/exit code。stdout/stderr 各上限 64KiB，超出时保留头尾各一半、中间省略（truncated=true，omittedBytes 为省略字节数）；长时间运行的命令改用 job_start。仅 SSH 会话；本地会话会被拒绝。命令经安全策略分类（档位按会话/分组/全局设置）：readonly 档仅只读白名单内命令放行；黑名单、无法静态判定、生产会话的非只读命令需用户在 mySSH 窗口确认——此时可能返回 status=pending_approval，用完全相同的参数再次调用即继续等待。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "会话档案 id（list_sessions 返回的 id）"
                        },
                        "command": {
                            "type": "string",
                            "description": "要在远端执行的 shell 命令"
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "超时毫秒数，默认 30000，上限 120000",
                            "minimum": 1,
                            "maximum": 120000,
                            "default": 30000
                        },
    "purpose": {
                            "type": "string",
                            "description": "可选：一句话说明这条命令要做什么、为什么（≤300 字）。需用户确认时显示在审批框里（标注为 AI 自述），不影响安全判定"
                        },
                        "strip_ansi": {
                            "type": "boolean",
                            "default": true,
                            "description": "剥离 ANSI 控制序列、折叠 CRLF 与进度条刷新；需要原始字节时置 false"
                        }
                    },
                    "required": ["session_id", "command"],
                    "additionalProperties": false
                }
            },
            {
                "name": "recipe_list",
                "description": "列出内置运维 recipe（参数化命令模板）：id、说明、风险等级（read/change/danger）、参数及校验规则、命令模板。read 级在只读档也可执行（模板固定、参数严格校验），比自由命令更易获批",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "recipe_run",
                "description": "在 SSH 会话上执行内置 recipe。参数按白名单校验后单引号代入。风险判定：read 任意档位放行；change 只读档拒绝、标准档及生产会话需确认、完全档放行；danger 只读档拒绝、其余档位一律需用户确认（可能返回 status=pending_approval，用相同参数重调继续等待）。输出同 ssh_exec，并附实际执行的 command",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "会话档案 id（list_sessions 返回的 id）" },
                        "recipe": { "type": "string", "description": "recipe id（recipe_list 返回）" },
                        "params": { "type": "object", "description": "recipe 参数，键为参数名", "additionalProperties": { "type": ["string", "integer"] } },
                        "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 120000, "default": 30000 }
                    },
                    "required": ["session_id", "recipe"],
                    "additionalProperties": false
                }
            },
            {
                "name": "job_start",
                "description": "在已保存的 SSH 会话上后台启动一条长时间运行的命令（构建、tail -f、长下载等），立即返回 jobId，不等待结束。命令门与 ssh_exec 相同（需确认时返回 status=pending_approval，用相同参数重调继续等待）。每会话最多 3 个运行中任务。输出用 job_read 分段读取，job_stop 停止。仅 SSH 会话。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "会话档案 id（list_sessions 返回的 id）" },
                        "command": { "type": "string", "description": "要在远端后台执行的 shell 命令" },
                        "purpose": { "type": "string", "description": "可选：一句话说明这条命令要做什么、为什么（≤300 字）。需用户确认时显示在审批框里（标注为 AI 自述），不影响安全判定" },
                        "pty": { "type": "boolean", "default": true, "description": "分配伪终端（TERM=dumb）：job_stop 时远端进程能收到挂断；false 则 stdout/stderr 原样不经终端" }
                    },
                    "required": ["session_id", "command"],
                    "additionalProperties": false
                }
            },
            {
                "name": "job_read",
                "description": "按字节游标分段读取后台任务输出。首次 offset=0，之后传上次返回的 nextOffset；more=true 表示还有已缓冲未读的输出。默认剥离 ANSI 控制序列并折叠进度条刷新。缓冲上限内最旧输出会被丢弃（truncated=true）。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "job_id": { "type": "string", "description": "job_start 返回的 jobId" },
                        "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "起始字节游标" },
                        "max_bytes": { "type": "integer", "minimum": 1, "maximum": 262144, "default": 16384, "description": "本次最多读取的原始字节数" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 60, "default": 0, "description": "无新输出且任务未结束时最多等待的秒数" },
                        "strip_ansi": { "type": "boolean", "default": true, "description": "剥离 ANSI/OSC 控制序列" }
                    },
                    "required": ["job_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "job_stop",
                "description": "停止后台任务（发 TERM 信号并关闭通道），返回任务最终状态。已结束的任务直接返回状态。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "job_id": { "type": "string", "description": "job_start 返回的 jobId" }
                    },
                    "required": ["job_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "job_list",
                "description": "列出后台任务（含已结束的最近任务）：jobId、会话、命令、状态、退出码、已输出字节数，按启动时间倒序。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "仅列出该会话的任务" }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_open",
                "description": "在已保存的 SSH 会话上打开交互式终端（有状态 shell：cd、进 mysql、激活 venv 等状态跨调用保持），返回 terminalId。mySSH 窗口在线时以普通标签页打开（visible=true）：用户可旁观、键入时 AI 写入被暂拒、可随时收回控制；否则（或 headless=true）走独立后台连接（xterm 120x32，keyboard-interactive 认证会被拒绝）。仅 SSH 会话。输出用 terminal_read 轮询，用完用 terminal_close 释放。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "会话档案 id（list_sessions 返回的 id）" },
                        "headless": { "type": "boolean", "default": false, "description": "true = 不在 mySSH 窗口显示，走后台连接" }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_send",
                "description": "向 terminal_open 打开的终端写入 UTF-8 文本（命令、交互应答等）。换行需自行包含在 data 中（\\n），或置 append_newline=true 末尾自动补一个（模拟回车）。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string", "description": "terminal_open 返回的终端 id" },
                        "data": { "type": "string", "description": "要写入的 UTF-8 文本" },
                        "append_newline": { "type": "boolean", "default": false, "description": "末尾追加 \\n" }
                    },
                    "required": ["terminalId", "data"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_read",
                "description": "读取终端输出。offset 游标模型：返回 offset 之后的输出与 nextOffset（下次传入继续读；首次传 0）。输出缓冲为 1MiB 环形，超界丢最旧；offset 早于可用起点时 truncated=true 并从最早可用处返回。wait_seconds>0 时服务端长轮询（最长 600s）直到就绪或终端关闭：默认有新输出即返回；给 wait_for（正则，匹配剥离 ANSI 后的新输出，如提示符 `\\$ $`）则等到匹配（返回 matched），给 idle_ms 则等到输出静默该时长——用于判定命令执行完毕，避免反复轮询。默认剥离 ANSI 控制序列；单次最多返回 max_bytes（默认 64KiB），more=true 表示还有未读输出。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 0, "description": "上次返回的 nextOffset；首次传 0" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0 },
                        "wait_for": { "type": "string", "description": "就绪判定正则（Rust regex 语法，匹配 offset 之后剥离 ANSI 的输出）" },
                        "idle_ms": { "type": "integer", "minimum": 1, "description": "有新输出后静默该毫秒数即视为就绪（粒度约 0.5s）" },
                        "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536, "description": "本次最多读取的原始字节数" },
                        "strip_ansi": { "type": "boolean", "default": true, "description": "剥离 ANSI/OSC 控制序列（全屏程序如 vim/top 需原始字节时置 false）" }
                    },
                    "required": ["terminalId", "offset"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_list",
                "description": "列出当前可用的终端（terminalId/sessionId/kind/closed）。kind=shared_tab 表示用户主动把 mySSH 标签共享给了 AI；agent_tab 为 terminal_open 打开的标签；headless 为后台终端",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "terminal_close",
                "description": "关闭终端：后台终端断通道并释放连接；mySSH 标签只收回 AI 控制、标签保留给用户（落审计）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string" }
                    },
                    "required": ["terminalId"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_home",
                "description": "解析会话远端家目录绝对路径",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_list",
                "description": "列出远端目录内容（name/path/kind/size/mtime/permissions）。SFTP 操作与 UI 共享 Bulk 连接池。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string", "description": "远端目录绝对路径" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_stat",
                "description": "远端路径元数据（不跟随软链接）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_read",
                "description": "读远端文本文件（默认上限 256KiB，max_bytes 最大 1MiB；超出置 truncated=true）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 262144 }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_write",
                "description": "覆盖写远端文件（不存在则创建；内容上限 1MiB）。注意：会整体替换文件内容；局部修改优先用 file_edit。原内容自动备份（返回 backupId，可 file_rollback）。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "content": { "type": "string", "description": "完整新内容（UTF-8）" }
                    },
                    "required": ["session_id", "path", "content"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_mkdir",
                "description": "远端新建目录",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_delete",
                "description": "删除远端文件/目录；目录需 recursive=true 才递归删除。删除文件前自动备份（返回 backupId，可 file_rollback）；目录删除不备份",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "recursive": { "type": "boolean", "default": false }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_rename",
                "description": "远端移动/重命名",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "from": { "type": "string" },
                        "to": { "type": "string" }
                    },
                    "required": ["session_id", "from", "to"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_chmod",
                "description": "改远端权限，mode 为八进制字符串（如 \"755\"）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "mode": { "type": "string", "description": "八进制权限，如 755 / 644" }
                    },
                    "required": ["session_id", "path", "mode"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_upload",
                "description": "上传本地文件到远端（覆盖写；入队后台传输队列执行，与 UI 传输面板共享，无大小上限）。wait_seconds>0 时同步等待终态返回（最长 600s，等待不占调用方上下文）；否则立即返回 queued 态，进度用 sftp_transfer_list 查询。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "local_path": { "type": "string", "description": "本地文件绝对路径" },
                        "remote_path": { "type": "string", "description": "远端目标绝对路径" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0, "description": "同步等待传输终态的秒数；0 = 立即返回" }
                    },
                    "required": ["session_id", "local_path", "remote_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_download",
                "description": "下载远端文件到本地（覆盖写本地目标；入队后台传输队列执行，无大小上限）。wait_seconds 语义同 sftp_upload。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "remote_path": { "type": "string", "description": "远端文件绝对路径" },
                        "local_path": { "type": "string", "description": "本地目标绝对路径" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0 }
                    },
                    "required": ["session_id", "remote_path", "local_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_transfer_list",
                "description": "列出会话当前传输任务（id/方向/状态 queued|running|paused|done|failed|canceled/进度/错误），用于轮询 sftp_upload/download 的进度",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_view",
                "description": "按行查看远端文本文件（带行号，默认 400 行、最多 2000 行；超长行截断）。返回 etag，可传给 file_edit 防止覆盖他人并发改动。文件上限 8MiB",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 1, "default": 1, "description": "起始行（1 起）" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 400 }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_edit",
                "description": "精确字符串替换编辑远端文件：old_str 须与文件内容逐字一致且唯一（否则报错，可加上下文或 replace_all=true）。old_str 为空且文件不存在 = 新建。改前自动备份（返回 backupId，可 file_rollback）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "old_str": { "type": "string" },
                        "new_str": { "type": "string" },
                        "replace_all": { "type": "boolean", "default": false },
                        "expected_etag": { "type": "string", "description": "file_view 返回的 etag；文件已变化则拒绝" }
                    },
                    "required": ["session_id", "path", "old_str", "new_str"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_grep",
                "description": "在远端文件/目录中按正则搜索（经 SFTP 遍历，不执行远端命令），输出 path:行号:内容。跳过二进制、>2MiB 文件与 .git/node_modules；遍历最多 20000 项 / 20s",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string", "description": "文件或目录" },
                        "pattern": { "type": "string", "description": "正则（Rust regex 语法）" },
                        "glob": { "type": "string", "description": "文件过滤，如 *.conf 或 **/*.{yml,yaml}" },
                        "case_insensitive": { "type": "boolean", "default": false },
                        "files_only": { "type": "boolean", "default": false, "description": "只列出含匹配的文件" },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
                    },
                    "required": ["session_id", "path", "pattern"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_glob",
                "description": "按 glob 查找远端文件（** 跨目录，* ? 不跨 /，{a,b} 选择；不含 / 的模式按文件名匹配）。输出 路径<TAB>大小",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string", "description": "起始目录" },
                        "pattern": { "type": "string" },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
                    },
                    "required": ["session_id", "path", "pattern"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_backups",
                "description": "列出该会话的 AI 文件改动备份（新→旧）：id/路径/触发工具/改前是否存在/时间",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "file_rollback",
                "description": "把文件恢复到某条备份的改前状态（改前不存在则删除）。若该文件在那次改动后又被修改过，默认拒绝，force=true 才覆盖。回滚前的内容也会备份",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "backup_id": { "type": "string" },
                        "force": { "type": "boolean", "default": false }
                    },
                    "required": ["session_id", "backup_id"],
                    "additionalProperties": false
                }
            }
        ]
    });
    if let Some(tools) = v.get_mut("tools").and_then(Value::as_array_mut) {
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| perms.allows(n))
        });
        for tool in tools.iter_mut() {
            if let Some(a) = tool
                .get("name")
                .and_then(Value::as_str)
                .map(tool_annotations)
            {
                tool["annotations"] = a;
            }
        }
    }
    v
}

/// 工具行为注解（MCP 2025-03-26 起）：供客户端决定是否自动批准、如何提示风险。
/// 只是提示，不是安全边界——真正的拦截在权限闸与命令策略。
/// ssh_exec / terminal_send 能执行任意命令，按可破坏性标注；未列出的工具不给注解。
fn tool_annotations(name: &str) -> Value {
    // (readOnly, destructive, idempotent, openWorld)
    let (ro, destructive, idempotent, open_world) = match name {
        "list_sessions" => (true, false, true, false),
        "sftp_home" | "sftp_list" | "sftp_stat" | "sftp_read" | "sftp_transfer_list"
        | "terminal_read" => (true, false, true, true),
        "terminal_list" | "file_backups" | "job_list" | "recipe_list" => (true, false, true, false),
        "recipe_run" => (false, true, false, true),
        "job_read" => (true, false, true, true),
        "job_start" => (false, true, false, true),
        "job_stop" => (false, true, true, true),
        "file_view" | "file_grep" | "file_glob" => (true, false, true, true),
        "file_edit" | "file_rollback" => (false, true, false, true),
        "ssh_exec" | "terminal_send" => (false, true, false, true),
        "terminal_open" | "terminal_close" | "sftp_mkdir" => (false, false, false, true),
        "sftp_write" | "sftp_delete" | "sftp_chmod" | "sftp_upload" | "sftp_download" => {
            (false, true, true, true)
        }
        "sftp_rename" => (false, true, false, true),
        _ => return Value::Null,
    };
    json!({
        "readOnlyHint": ro,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world,
    })
}

/// MCP content 结果包装
fn ok_content(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn err_content(msg: impl Into<String>) -> Value {
    let mut v = ok_content(msg.into());
    v["isError"] = Value::Bool(true);
    v
}

/// tools/call 分发：先过权限闸（设置 → MCP 工具权限），未知工具报 JSON-RPC 层错误之外，
/// 工具执行失败走 isError 内容
async fn call_tool(st: &ServerState, name: &str, args: &Value) -> Value {
    if !tool_allowed(st, name, args).await {
        return err_content(format!(
            "MCP 工具已被禁用：{name}（mySSH 设置 → MCP 工具权限，或会话编辑器的 MCP 权限覆盖）"
        ));
    }
    match name {
        "list_sessions" => match list_sessions_tool(st).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        "ssh_exec" => match ssh_exec_tool(st, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        "recipe_list" => ok_content(crate::mcp_recipes::list_json().to_string()),
        "recipe_run" => match recipe_run_tool(st, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        n if n.starts_with("job_") => match job_tool(st, n, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        // 交互式终端工具族（有状态 shell；实现见 mcp_terminal.rs）
        n if n.starts_with("terminal_") => {
            let ctx = crate::mcp_terminal::TermCtx {
                store: &st.store,
                terms: &st.terms,
                ui: &st.ui,
                mgr: &st.term_mgr,
                recs: &st.recs,
            };
            match crate::mcp_terminal::terminal_tool(&ctx, n, args).await {
                Ok(text) => ok_content(text),
                Err(e) => err_content(e),
            }
        }
        // 远程文件工具族（view/edit/grep/glob + 备份回滚；实现见 mcp_files.rs）
        n if n.starts_with("file_") => {
            let ctx = crate::mcp_files::FileCtx {
                store: &st.store,
                sftp: &st.sftp,
                backups: &st.backups,
            };
            match tokio::time::timeout(
                FILE_TOOL_TIMEOUT,
                crate::mcp_files::file_tool(&ctx, n, args),
            )
            .await
            {
                Ok(Ok(text)) => ok_content(text),
                Ok(Err(e)) => err_content(e),
                Err(_) => err_content(format!("文件操作超时（{}s）", FILE_TOOL_TIMEOUT.as_secs())),
            }
        }
        // SFTP 工具族：30s 超时兜底，错误走 isError 内容
        n if n.starts_with("sftp_") => match sftp_tool(st, n, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        other => err_content(format!("未知工具：{other}")),
    }
}

/// 权限分组键（list_sessions 按会话展示有效放行情况）
const PERM_GROUPS: [&str; 6] = [
    "list_sessions",
    "ssh_exec",
    "sftp_read",
    "sftp_write",
    "sftp_transfer",
    "terminal",
];

/// list_sessions：会话档案最小投影（绝不带凭据材料）+ 每个会话对 AI 的有效授权与连接状态，
/// 让 agent 不必试错就知道哪些会话能用、哪些命令会被拦或需确认
async fn list_sessions_tool(st: &ServerState) -> Result<String, String> {
    let list = st
        .store
        .sessions()
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let groups = crate::mcp_policy::load_group_policies(&st.store).await;
    let global_level = crate::mcp_policy::load_global_level(&st.store).await;
    let tabs = st.term_mgr.tabs_by_session();
    let mut ai_terms = st.terms.live_by_session();
    let items: Vec<Value> = list
        .iter()
        .map(|r| {
            let eff = crate::mcp_policy::resolve_effective(r, &groups, global_level);
            let tools: serde_json::Map<String, Value> = PERM_GROUPS
                .iter()
                .map(|k| {
                    let global = st.perms.allows_group(k);
                    let ok = crate::mcp_policy::resolve_perm(r, &groups, k, global);
                    ((*k).to_string(), Value::Bool(ok))
                })
                .collect();
            json!({
                "id": r.id,
                "name": r.name,
                "host": r.host,
                "port": r.port,
                "user": r.user,
                "group": r.group_path,
                "kind": r.kind.as_str(),
                "ai": {
                    "level": crate::mcp_policy::level_str(eff.level),
                    "production": eff.production,
                    "tools": tools,
                },
                "openTabs": tabs.get(&r.id).copied().unwrap_or(0),
                "aiTerminals": ai_terms.remove(&r.id).unwrap_or_default(),
            })
        })
        .collect();
    let body = json!({
        "sessions": items,
        "windowOnline": st.ui.has_ui(),
        "legend": {
            "ai.level": "readonly = 仅只读白名单命令直接执行，其余拒绝；standard = 无法证明只读的命令需用户确认；full = 仅黑名单需确认",
            "ai.production": "true 时非只读命令一律需用户确认",
            "ai.tools": "各工具组对该会话是否放行（false 时调用会被拒绝）；ssh_exec 组含 job_* / recipe_*，terminal 组为 terminal_*",
            "openTabs": "用户在 mySSH 窗口中为该会话打开的终端标签数",
            "aiTerminals": "该会话上 AI 可用的终端：agent = AI 打开的标签，shared = 用户共享给 AI 的标签，headless = 后台终端；可直接用于 terminal_send/terminal_read",
            "windowOnline": "mySSH 窗口是否在线；离线时需确认的命令会被拒绝",
        },
    });
    serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
}

/// ssh_exec：resolve → Exec Transport（PR-14 复用连接，MCP 配额）→ exec channel 收集
async fn ssh_exec_tool(st: &ServerState, args: &Value) -> Result<String, String> {
    let store = &st.store;
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "ssh_exec 缺少参数 session_id".to_string())?;
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "ssh_exec 缺少参数 command".to_string())?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);
    let strip = args
        .get("strip_ansi")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let purpose = purpose_arg(args);
    if let Gate::Pending(resp) = exec_gate(
        st,
        session_id,
        command,
        "ssh_exec",
        None,
        purpose.as_deref(),
    )
    .await?
    {
        return Ok(resp);
    }

    let target = crate::sessions::resolve_session_target(store, session_id).await?;
    if matches!(target, crate::sessions::ResolvedTarget::Local(_)) {
        return Err("本地会话不支持 ssh_exec（仅 SSH 会话）".into());
    }
    let body = run_exec(st, session_id, command, timeout_ms, strip, "mcp_ssh_exec").await?;
    serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
}

/// 已过命令门的一次性执行：Exec Transport（PR-14 复用连接，MCP 配额）→ exec channel 收集；
/// 结果（含超时/错误）落审计
async fn run_exec(
    st: &ServerState,
    session_id: &str,
    command: &str,
    timeout_ms: u64,
    strip: bool,
    action: &str,
) -> Result<Value, String> {
    let store = &st.store;
    tracing::info!(session_id, timeout_ms, action, "MCP 命令开始执行");
    let mirror = exec_mirror(st, session_id, action, command).await;
    let started = Instant::now();
    let run = async {
        let ctx = crate::exec::ensure_exec_ctx(&st.exec, store, session_id).await?;
        let _permit = ctx.acquire_mcp().await?;
        ssh_exec_inner(ctx, command.to_string(), strip, mirror.as_ref()).await
    };
    let result = tokio::time::timeout(Duration::from_millis(timeout_ms), run).await;
    let elapsed = started.elapsed().as_millis() as u64;
    if let Some(m) = mirror {
        match &result {
            Ok(Ok(body)) => m.finish("ok", body["exitCode"].as_u64(), elapsed, None),
            Ok(Err(e)) => m.finish("error", None, elapsed, Some(e)),
            Err(_) => m.finish("timeout", None, elapsed, None),
        }
    }

    let (result_flag, detail) = match &result {
        Ok(Ok(body)) => (
            "ok",
            json!({ "exitCode": body["exitCode"], "truncated": body["truncated"] }),
        ),
        Ok(Err(e)) => ("error", json!({ "error": e })),
        Err(_) => ("timeout", json!({ "timeoutMs": timeout_ms })),
    };
    mcp_audit(
        st,
        session_id,
        action,
        &json!({ "command": command, "result": result_flag, "durationMs": elapsed, "detail": detail }),
    )
    .await;
    match result {
        Ok(Ok(mut body)) => {
            body["durationMs"] = json!(elapsed);
            Ok(body)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("命令执行超时（{timeout_ms} ms），连接已断开")),
    }
}

/// 窗口在线且未关闭镜像（mcp.exec_mirror，缺省开启）时，为本次执行建只读镜像
async fn exec_mirror(
    st: &ServerState,
    session_id: &str,
    action: &str,
    command: &str,
) -> Option<crate::mcp_ui::ExecMirror> {
    if !st.ui.has_ui() {
        return None;
    }
    let raw = st
        .store
        .settings()
        .get("mcp.exec_mirror")
        .await
        .ok()
        .flatten();
    if !parse_allow_setting(raw.as_deref()) {
        return None;
    }
    let name = st
        .store
        .sessions()
        .get(session_id)
        .await
        .map(|r| r.name)
        .unwrap_or_default();
    let tool = action.strip_prefix("mcp_").unwrap_or(action);
    crate::mcp_ui::ExecMirror::begin(&st.ui, session_id, &name, tool, command)
}

/// recipe_run：渲染内置模板 → 按风险等级过命令门 → 与 ssh_exec 同一执行通路
async fn recipe_run_tool(st: &ServerState, args: &Value) -> Result<String, String> {
    let session_id = req_str(args, "session_id", "recipe_run")?;
    let id = req_str(args, "recipe", "recipe_run")?;
    let params = args.get("params").cloned().unwrap_or(Value::Null);
    let (recipe, command) = crate::mcp_recipes::render(id, &params)?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);
    if let Gate::Pending(resp) = exec_gate(
        st,
        session_id,
        &command,
        "recipe_run",
        Some(recipe.risk),
        None,
    )
    .await?
    {
        return Ok(resp);
    }
    let target = crate::sessions::resolve_session_target(&st.store, session_id).await?;
    if matches!(target, crate::sessions::ResolvedTarget::Local(_)) {
        return Err("本地会话不支持 recipe_run（仅 SSH 会话）".into());
    }
    let mut body = run_exec(st, session_id, &command, timeout_ms, true, "mcp_recipe_run").await?;
    body["recipe"] = json!(recipe.id);
    body["risk"] = json!(recipe.risk.as_str());
    body["command"] = json!(command);
    serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
}
/// agent 自述的命令用途（审批框展示，不参与判定）：去空白，截断到 300 字符
fn purpose_arg(args: &Value) -> Option<String> {
    let p = args.get("purpose").and_then(Value::as_str)?.trim();
    (!p.is_empty()).then(|| p.chars().take(300).collect())
}

enum Gate {
    /// 放行，继续执行
    Go,
    /// 等待用户审批：直接把这段 JSON 返回给 agent
    Pending(String),
}

/// 命令门（ssh_exec 与 job_start 共用）：速率闸 → 分类器 → 必要时审批弹窗。
/// 拒绝返回 Err；门在会话解析之前，被拒绝的命令不触网、不建连。
async fn exec_gate(
    st: &ServerState,
    session_id: &str,
    command: &str,
    tool: &str,
    recipe_risk: Option<crate::mcp_recipes::Risk>,
    purpose: Option<&str>,
) -> Result<Gate, String> {
    let action = format!("mcp_{tool}");
    // 安全模型第 6 条：每会话命令速率上限（拒绝也计数，防探测洪泛）
    if let Err(e) = st.rate.check(session_id) {
        mcp_audit(
            st,
            session_id,
            &action,
            &json!({ "command": command, "result": "rate_limited" }),
        )
        .await;
        return Err(format!("MCP 速率限制：{e}"));
    }
    // 安全模型第 1/2 条：按会话有效档位/生产标记（会话 → 分组 → 全局）判定；
    // 需人工确认的命令走应用内审批弹窗，窗口不可用时 fail-closed 拒绝。
    let rec = st.store.sessions().get(session_id).await.ok();
    let eff = session_effective(st, rec.as_ref()).await;
    let level = crate::mcp_policy::level_str(eff.level);
    let verdict = match recipe_risk {
        Some(risk) => crate::mcp_recipes::verdict(risk, eff.level, eff.production),
        None => st.policy.classify(command, eff.level, eff.production),
    };
    let deny_reason = match verdict {
        Verdict::Allow => None,
        Verdict::Deny => Some(format!("命令超出只读白名单（当前为 {level} 档）")),
        // 用户此前对本会话同一命令选过「总是允许」
        Verdict::RequireConfirmation if st.ui.is_always_allowed(session_id, command) => {
            mcp_audit(
                st,
                session_id,
                &action,
                &json!({ "command": command, "result": "approved", "rule": "always" }),
            )
            .await;
            None
        }
        Verdict::RequireConfirmation => {
            let ask = Ask {
                rec: rec.as_ref(),
                eff: &eff,
                action: &action,
                purpose,
            };
            match confirm_exec(st, session_id, command, ask).await {
                Confirm::Approved => None,
                Confirm::Denied(reason) => Some(reason),
                Confirm::Pending(id) => {
                    mcp_audit(
                        st,
                        session_id,
                        &action,
                        &json!({ "command": command, "result": "pending", "approvalId": id }),
                    )
                    .await;
                    let resp = serde_json::to_string_pretty(&json!({
                        "status": "pending_approval",
                        "approvalId": id,
                        "message": format!("命令需用户在 mySSH 窗口中确认，用户尚未决定。请用完全相同的参数再次调用 {tool} 继续等待；勿改写命令绕过确认。"),
                    }))
                    .map_err(|e| e.to_string())?;
                    return Ok(Gate::Pending(resp));
                }
            }
        }
    };
    if let Some(reason) = deny_reason {
        mcp_audit(
            st,
            session_id,
            &action,
            &json!({ "command": command, "result": "rejected", "reason": reason }),
        )
        .await;
        return Err(format!("MCP 安全策略拒绝执行：{reason}"));
    }
    Ok(Gate::Go)
}

/// job_* 分发：job_start 先过命令门，再在 Exec Transport 上开通道
async fn job_tool(st: &ServerState, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "job_start" => {
            let session_id = req_str(args, "session_id", name)?;
            let command = req_str(args, "command", name)?;
            let pty = args.get("pty").and_then(Value::as_bool).unwrap_or(true);
            let purpose = purpose_arg(args);
            if let Gate::Pending(resp) =
                exec_gate(st, session_id, command, name, None, purpose.as_deref()).await?
            {
                return Ok(resp);
            }
            let target = crate::sessions::resolve_session_target(&st.store, session_id).await?;
            if matches!(target, crate::sessions::ResolvedTarget::Local(_)) {
                return Err("本地会话不支持 job_start（仅 SSH 会话）".into());
            }
            let ctx = crate::exec::ensure_exec_ctx(&st.exec, &st.store, session_id).await?;
            let res =
                crate::mcp_jobs::job_start(&st.jobs, &st.store, ctx, session_id, command, pty)
                    .await;
            let detail = match &res {
                Ok(body) => json!({ "command": command, "result": "started", "detail": body }),
                Err(e) => json!({ "command": command, "result": "error", "error": e }),
            };
            mcp_audit(st, session_id, "mcp_job_start", &detail).await;
            res
        }
        "job_read" => crate::mcp_jobs::job_read(&st.jobs, args).await,
        "job_stop" => {
            let res = crate::mcp_jobs::job_stop(&st.jobs, args).await;
            if let (Ok(body), Some(sid)) = (
                &res,
                args.get("job_id")
                    .and_then(Value::as_str)
                    .and_then(|id| st.jobs.session_of(id)),
            ) {
                mcp_audit(st, &sid, "mcp_job_stop", &json!({ "detail": body })).await;
            }
            res
        }
        "job_list" => Ok(crate::mcp_jobs::job_list(&st.jobs, args)),
        other => Err(format!("未知工具：{other}")),
    }
}

/// ssh_exec 单次调用内等待用户审批的时长；未决返回 pending，agent 重试续等
const APPROVAL_WAIT: Duration = Duration::from_secs(60);

/// 会话有效档位/生产标记；会话不存在时按全局档位（后续会话解析会报 NotFound）
async fn session_effective(
    st: &ServerState,
    rec: Option<&core_store::SessionRecord>,
) -> crate::mcp_policy::Effective {
    let global = crate::mcp_policy::load_global_level(&st.store).await;
    match rec {
        Some(rec) => {
            let groups = crate::mcp_policy::load_group_policies(&st.store).await;
            crate::mcp_policy::resolve_effective(rec, &groups, global)
        }
        None => crate::mcp_policy::Effective {
            level: global,
            production: false,
        },
    }
}

enum Confirm {
    Approved,
    Denied(String),
    Pending(String),
}

/// 审批弹窗的上下文
struct Ask<'a> {
    rec: Option<&'a core_store::SessionRecord>,
    eff: &'a crate::mcp_policy::Effective,
    action: &'a str,
    purpose: Option<&'a str>,
}

/// 需人工确认的命令：推审批弹窗（附逐段风险分析与 agent 自述用途）并等待用户决定
async fn confirm_exec(st: &ServerState, session_id: &str, command: &str, ask: Ask<'_>) -> Confirm {
    let Ask {
        rec,
        eff,
        action,
        purpose,
    } = ask;
    // 会话不存在不弹窗：用户无从判断，且后续解析必然失败
    let Some(rec) = rec else {
        return Confirm::Denied("命令需人工确认，且会话不存在".into());
    };
    let parts: Vec<Value> = st
        .policy
        .explain(command)
        .into_iter()
        .map(|p| {
            json!({
                "text": p.text,
                "op": p.op,
                "head": p.head,
                "sub": p.sub,
                "risk": p.risk.as_str(),
            })
        })
        .collect();
    let event = json!({
        "sessionName": rec.name,
        "host": rec.host,
        "user": rec.user,
        "level": crate::mcp_policy::level_str(eff.level),
        "production": eff.production,
        "parts": parts,
        "purpose": purpose,
    });
    let id = match st.ui.request(session_id, command, event) {
        Ok(id) => id,
        Err(e) => return Confirm::Denied(e),
    };
    match st.ui.wait(&id, APPROVAL_WAIT).await {
        crate::mcp_ui::ApprovalWait::Approved => {
            mcp_audit(
                st,
                session_id,
                action,
                &json!({
                    "command": command,
                    "result": "approved",
                    "approvalId": id,
                    "always": st.ui.is_always_allowed(session_id, command),
                }),
            )
            .await;
            Confirm::Approved
        }
        crate::mcp_ui::ApprovalWait::Denied => Confirm::Denied("用户拒绝执行或审批已过期".into()),
        crate::mcp_ui::ApprovalWait::Pending(id) => Confirm::Pending(id),
    }
}

/// 建连 + exec + 输出收集（被外层 timeout 包裹；超时 drop 即断连）
async fn ssh_exec_inner(
    ctx: Arc<crate::exec::ExecCtx>,
    command: String,
    strip: bool,
    mirror: Option<&crate::mcp_ui::ExecMirror>,
) -> Result<Value, String> {
    // Exec Transport 复用连接（PR-14）：建连/host-key/KI 策略由 exec 组工厂统一承载；
    // 超时 drop 只弃本 channel，共享 Transport 存活
    let conn = ctx.conn_handle();
    let mut ch = conn
        .open_session_channel()
        .await
        .map_err(|e| e.to_string())?;
    ch.exec(true, command.as_str())
        .await
        .map_err(|e| format!("exec 通道请求失败: {e}"))?;

    let mut stdout = crate::mcp_text::HeadTail::new(OUTPUT_CAP);
    let mut stderr = crate::mcp_text::HeadTail::new(OUTPUT_CAP);
    let mut exit_status: Option<u32> = None;
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::Data { data } => {
                if let Some(m) = mirror {
                    m.push(false, &data);
                }
                stdout.push(&data);
            }
            russh::ChannelMsg::ExtendedData { data, .. } => {
                if let Some(m) = mirror {
                    m.push(true, &data);
                }
                stderr.push(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
            russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    drop(conn);

    let omitted = stdout.omitted() + stderr.omitted();
    let mut body = json!({
        "exitCode": exit_status,
        "stdout": stdout.render(strip),
        "stderr": stderr.render(strip),
        "truncated": omitted > 0,
    });
    if omitted > 0 {
        body["omittedBytes"] = json!(omitted);
    }
    Ok(body)
}
// ---------- SFTP 工具族 ----------
/// sftp_read 默认/最大返回字节；sftp_write 内容上限（字节要进模型上下文，必须有界）
const SFTP_READ_DEFAULT_CAP: u64 = 256 * 1024;
const SFTP_IO_MAX: u64 = 1024 * 1024;
/// 单操作超时（浏览/元操作/小文件读写；上传/下载入队即返回，wait_seconds 另加预算）
const SFTP_OP_TIMEOUT: Duration = Duration::from_secs(30);
/// file_* 工具超时（grep/glob 遍历自带 20s 预算，余量给读文件与写回）
const FILE_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// 解析 session_id 并取/建 SFTP 上下文（ensure_ctx 内含 KI 拒绝与连接复用）
async fn sftp_ctx(
    st: &ServerState,
    args: &Value,
    tool: &str,
) -> Result<(String, Arc<crate::sftp::SftpCtx>), String> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 session_id"))?;
    let ctx = crate::sftp::ensure_ctx(&st.sftp, &st.store, session_id).await?;
    Ok((session_id.to_string(), ctx))
}

fn req_str<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 {key}"))
}

/// 把 extra 的字段并入 out（两者均为对象时）
fn merge_json(out: &mut Value, extra: Value) {
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
}

/// MCP 侧写操作审计（Actor::Mcp，与 UI 的 Actor::Gui 区分来源）
async fn mcp_audit(st: &ServerState, session_id: &str, action: &str, detail: &Value) {
    let _ = st
        .store
        .audit()
        .append(core_store::Actor::Mcp, Some(session_id), action, detail)
        .await;
}

/// SFTP 工具分发（外层 call_tool 已按 sftp_ 前缀过滤；此处再校验具体名）。
/// 均为快操作；仅上传/下载可带 wait_seconds 同步等待终态，超时预算随之放宽。
async fn sftp_tool(st: &ServerState, name: &str, args: &Value) -> Result<String, String> {
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(600);
    let budget = SFTP_OP_TIMEOUT + Duration::from_secs(wait);
    let run = sftp_tool_inner(st, name, args);
    match tokio::time::timeout(budget, run).await {
        Ok(r) => r,
        Err(_) => Err(format!("SFTP 操作超时（{}s）", budget.as_secs())),
    }
}

async fn sftp_tool_inner(st: &ServerState, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "sftp_home" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let mc = ctx.meta_client().await?;
            let home = crate::sftp::resolve_home_abs(&mc).await?;
            Ok(json!({ "path": home }).to_string())
        }
        "sftp_list" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            let entries = crate::sftp::meta(&ctx, |c| async move { c.list(&p).await }).await?;
            let items: Vec<Value> = entries.iter().map(crate::sftp::entry_to_json).collect();
            Ok(json!({ "path": path, "entries": items }).to_string())
        }
        "sftp_stat" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            let e = crate::sftp::meta(&ctx, |c| async move { c.lstat(&p).await }).await?;
            Ok(crate::sftp::entry_to_json(&e).to_string())
        }
        "sftp_read" => {
            use tokio::io::AsyncReadExt;
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let max = args
                .get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(SFTP_READ_DEFAULT_CAP)
                .clamp(1, SFTP_IO_MAX);
            let p = path.to_string();
            let f = crate::sftp::meta(&ctx, |c| async move { c.open_read(&p).await }).await?;
            let mut buf = Vec::new();
            f.take(max + 1)
                .read_to_end(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            let truncated = buf.len() as u64 > max;
            buf.truncate(max as usize);
            Ok(json!({
                "path": path,
                "bytes": buf.len(),
                "truncated": truncated,
                "content": String::from_utf8_lossy(&buf),
            })
            .to_string())
        }
        "sftp_write" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| "sftp_write 缺少参数 content".to_string())?;
            if content.len() as u64 > SFTP_IO_MAX {
                return Err(format!(
                    "content 超过上限 {} 字节（大文件请用 UI 传输队列）",
                    SFTP_IO_MAX
                ));
            }
            let backup = crate::mcp_files::backup_existing(
                &ctx,
                &st.backups,
                &sid,
                path,
                name,
                Some(content.as_bytes()),
            )
            .await;
            let p = path.to_string();
            let bytes = content.as_bytes();
            if let Err(e) =
                crate::sftp::meta(&ctx, move |c| async move { c.overwrite(&p, bytes).await }).await
            {
                backup.discard(&st.backups).await;
                return Err(e);
            }
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_write",
                &json!({ "path": path, "bytes": content.len(), "backupId": backup.id() }),
            )
            .await;
            let mut out = json!({ "path": path, "bytes": content.len() });
            merge_json(&mut out, backup.to_json());
            Ok(out.to_string())
        }
        "sftp_mkdir" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.mkdir(&p).await }).await?;
            mcp_audit(st, &sid, "mcp_sftp_mkdir", &json!({ "path": path })).await;
            Ok(json!({ "ok": true, "path": path }).to_string())
        }
        "sftp_delete" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let recursive = args
                .get("recursive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let p = path.to_string();
            let st_meta = crate::sftp::meta(&ctx, {
                let p = p.clone();
                move |c| async move { c.lstat(&p).await }
            })
            .await?;
            match st_meta.kind {
                core_sftp::EntryKind::Dir => {
                    if recursive {
                        crate::sftp::meta(
                            &ctx,
                            move |c| async move { c.remove_recursive(&p).await },
                        )
                        .await?;
                    } else {
                        crate::sftp::meta(&ctx, move |c| async move { c.remove_dir(&p).await })
                            .await?;
                    }
                }
                _ => {
                    // 文件删除前备份（目录删除不备份：体量不可控）
                    let backup = crate::mcp_files::backup_existing(
                        &ctx,
                        &st.backups,
                        &sid,
                        path,
                        name,
                        None,
                    )
                    .await;
                    if let Err(e) =
                        crate::sftp::meta(&ctx, move |c| async move { c.remove_file(&p).await })
                            .await
                    {
                        backup.discard(&st.backups).await;
                        return Err(e);
                    }
                    mcp_audit(
                        st,
                        &sid,
                        "mcp_sftp_delete",
                        &json!({ "path": path, "recursive": false, "backupId": backup.id() }),
                    )
                    .await;
                    let mut out = json!({ "ok": true, "path": path });
                    merge_json(&mut out, backup.to_json());
                    return Ok(out.to_string());
                }
            }
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_delete",
                &json!({ "path": path, "recursive": recursive }),
            )
            .await;
            Ok(json!({ "ok": true, "path": path }).to_string())
        }
        "sftp_rename" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let from = req_str(args, "from", name)?;
            let to = req_str(args, "to", name)?;
            let f = from.to_string();
            let t = to.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.rename(&f, &t).await }).await?;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_rename",
                &json!({ "from": from, "to": to }),
            )
            .await;
            Ok(json!({ "ok": true, "from": from, "to": to }).to_string())
        }
        "sftp_chmod" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let mode_raw = req_str(args, "mode", name)?;
            let mode = u32::from_str_radix(mode_raw, 8)
                .map_err(|_| format!("mode 需为八进制字符串（如 755），收到: {mode_raw}"))?;
            let p = path.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.chmod(&p, mode).await }).await?;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_chmod",
                &json!({ "path": path, "mode": mode_raw }),
            )
            .await;
            Ok(json!({ "ok": true, "path": path, "mode": mode_raw }).to_string())
        }
        "sftp_upload" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let local = req_str(args, "local_path", name)?;
            let remote = req_str(args, "remote_path", name)?;
            let meta = tokio::fs::metadata(local)
                .await
                .map_err(|e| format!("本地文件读取失败 {local}: {e}"))?;
            if !meta.is_file() {
                return Err(format!("本地路径不是文件（目录上传请用 UI）：{local}"));
            }
            // 入队后台传输队列（与 UI 同一队列：进度进传输面板、终态落 transfers 表）；
            // 无大小上限——字节不走模型上下文，全程文件到文件流式
            let id = ctx
                .queue()
                .enqueue_upload(
                    PathBuf::from(local),
                    remote.to_string(),
                    meta.len(),
                    OnExists::Overwrite,
                )
                .await;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_upload",
                &json!({ "local": local, "remote": remote, "bytes": meta.len(), "transferId": id }),
            )
            .await;
            wait_transfer(&ctx, &id, args).await
        }
        "sftp_download" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let remote = req_str(args, "remote_path", name)?;
            let local = req_str(args, "local_path", name)?;
            // 预检远端存在性与大小（入队需要 bytes_total；缺失/超限在入队前报错）
            let r = remote.to_string();
            let meta = crate::sftp::meta(&ctx, move |c| async move { c.stat(&r).await }).await?;
            if meta.kind == core_sftp::EntryKind::Dir {
                return Err(format!("远端路径是目录（目录下载请用 UI）：{remote}"));
            }
            let id = ctx
                .queue()
                .enqueue_download(
                    remote.to_string(),
                    PathBuf::from(local),
                    meta.size,
                    OnExists::Overwrite,
                )
                .await;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_download",
                &json!({ "remote": remote, "local": local, "bytes": meta.size, "transferId": id }),
            )
            .await;
            wait_transfer(&ctx, &id, args).await
        }
        "sftp_transfer_list" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let items: Vec<Value> = ctx
                .queue()
                .list()
                .iter()
                .map(crate::sftp::transfer_to_json)
                .collect();
            Ok(json!({ "transfers": items }).to_string())
        }
        other => Err(format!("未知工具：{other}")),
    }
}

/// 入队后按需同步等待终态（wait_seconds 缺省 0 = 立即返回排队态）。
/// 等待在服务端轮询完成，不往模型上下文塞进度；超时未完结返回当前态（可再用
/// sftp_transfer_list 轮询）。
async fn wait_transfer(
    ctx: &Arc<crate::sftp::SftpCtx>,
    id: &str,
    args: &Value,
) -> Result<String, String> {
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(600);
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        let t = ctx
            .queue()
            .list()
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("传输 {id} 不存在"))?;
        if t.state.is_terminal() || Instant::now() >= deadline {
            return Ok(crate::sftp::transfer_to_json(&t).to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    // 测试代码豁免 unwrap/expect（工作区红线仅约束非测试代码）
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn perms_default_allows_everything_but_terminal() {
        let p = McpPerms::default();
        for name in [
            "terminal_open",
            "terminal_send",
            "terminal_read",
            "terminal_close",
        ] {
            assert!(!p.allows(name), "terminal 组缺省应关闭：{name}");
        }
        for name in [
            "list_sessions",
            "ssh_exec",
            "sftp_home",
            "sftp_list",
            "sftp_stat",
            "sftp_read",
            "sftp_write",
            "sftp_mkdir",
            "sftp_delete",
            "sftp_rename",
            "sftp_chmod",
            "sftp_upload",
            "sftp_download",
            "sftp_transfer_list",
        ] {
            assert!(p.allows(name), "默认应放行 {name}");
        }
    }

    #[test]
    fn perms_group_mapping() {
        let p = McpPerms {
            list_sessions: false,
            ssh_exec: false,
            sftp_read: false,
            sftp_write: false,
            sftp_transfer: false,
            terminal: true,
        };
        assert!(!p.allows("list_sessions"));
        assert!(!p.allows("ssh_exec"));
        assert!(!p.allows("sftp_read"));
        assert!(!p.allows("sftp_stat"));
        assert!(!p.allows("sftp_delete"));
        assert!(!p.allows("sftp_upload"));
        assert!(!p.allows("sftp_download"));
        assert!(!p.allows("sftp_transfer_list"));
        // terminal_* 独立成组：ssh_exec 关闭不影响，terminal 开关单独控制
        assert!(p.allows("terminal_open"));
        assert!(p.allows("terminal_send"));
        let p = McpPerms {
            terminal: false,
            ..McpPerms::default()
        };
        assert!(p.allows("ssh_exec"));
        assert!(!p.allows("terminal_open"));
        assert!(!p.allows("terminal_send"));
        assert!(!p.allows("terminal_read"));
        assert!(!p.allows("terminal_close"));
        // 未知名不被权限闸拦截（走「未知工具」分支）
        assert!(p.allows("sftp_nope"));
    }

    #[test]
    fn allow_setting_absent_means_allow() {
        assert!(parse_allow_setting(None));
        assert!(parse_allow_setting(Some("true")));
        assert!(!parse_allow_setting(Some("false")));
        // settings_set 落库为 JSON 编码
        assert!(parse_allow_setting(Some("\"true\"")));
    }

    #[test]
    fn tools_list_filters_disabled_groups() {
        // 提取工具名列表；结构不符时给出空表，由后续 len 断言失败暴露
        fn names_of(v: &Value) -> Vec<&str> {
            v["tools"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect()
        }
        assert_eq!(names_of(&tools_list(&McpPerms::default())).len(), 26);
        let all = tools_list(&all_perms());
        let names = names_of(&all);
        assert_eq!(names.len(), 31);
        // file_* 跟随 sftp_read / sftp_write 权限组
        let ro = McpPerms {
            sftp_write: false,
            ..all_perms()
        };
        let ro_names = names_of(&tools_list(&ro)).join(",");
        assert!(ro_names.contains("file_view") && ro_names.contains("file_grep"));
        assert!(!ro_names.contains("file_edit") && !ro_names.contains("file_rollback"));
        assert!(names.contains(&"terminal_list"));
        assert!(names.contains(&"sftp_upload"));
        assert!(names.contains(&"sftp_download"));
        assert!(names.contains(&"terminal_open"));
        assert!(names.contains(&"terminal_send"));
        assert!(names.contains(&"terminal_read"));
        assert!(names.contains(&"terminal_close"));

        let p = McpPerms {
            ssh_exec: false,
            sftp_transfer: false,
            terminal: false,
            ..all_perms()
        };
        let filtered = tools_list(&p);
        let names = names_of(&filtered);
        assert!(!names.contains(&"ssh_exec"));
        assert!(!names.contains(&"job_start") && !names.contains(&"job_read"));
        assert!(!names.contains(&"recipe_run") && !names.contains(&"recipe_list"));
        assert!(!names.contains(&"sftp_upload"));
        assert!(!names.contains(&"sftp_download"));
        assert!(!names.contains(&"sftp_transfer_list"));
        assert!(!names.contains(&"terminal_open"));
        assert!(!names.contains(&"terminal_send"));
        assert!(!names.contains(&"terminal_read"));
        assert!(!names.contains(&"terminal_close"));
        assert_eq!(names.len(), 16);
    }

    /// 策略门 E2E：拒绝路径不触网（门在会话解析之前），临时库即可
    #[tokio::test]
    async fn e2e_ssh_exec_policy_gate() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let (mgr, url) = boot_mcp(store).await;

        // 黑名单命中 → RequireConfirmation 在无确认通道下降级为拒绝
        let (text, is_err) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "nope", "command": "rm -rf /tmp/x" }),
        )
        .await;
        assert!(is_err, "黑名单命令应被拒绝: {text}");
        assert!(text.contains("安全策略"), "应带策略拒绝原因: {text}");

        // 只读白名单外 → readonly 档拒绝
        let (text, is_err) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "nope", "command": "touch /tmp/x" }),
        )
        .await;
        assert!(is_err, "白名单外命令应被拒绝: {text}");
        assert!(text.contains("安全策略"), "应带策略拒绝原因: {text}");

        // 白名单命令穿过策略门，落到会话解析报错——证明门未误杀
        let (text, is_err) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "nope", "command": "ls -la" }),
        )
        .await;
        assert!(is_err, "不存在的会话仍应报错: {text}");
        assert!(
            !text.contains("安全策略"),
            "白名单命令不应被策略门拦截: {text}"
        );

        // recipe：read 级在只读档穿过门（docker 不在白名单，自由命令会被拒）；
        // change / danger 级在只读档被拒；参数注入在渲染期即被拒
        let (text, is_err) = call_text(
            &url,
            "recipe_run",
            json!({ "session_id": "nope", "recipe": "docker_ps" }),
        )
        .await;
        assert!(
            is_err && !text.contains("安全策略"),
            "read recipe 不应被拦: {text}"
        );
        let (text, _) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "nope", "command": "docker ps -a" }),
        )
        .await;
        assert!(
            text.contains("安全策略"),
            "同命令走 ssh_exec 在只读档应被拒: {text}"
        );
        for (recipe, params) in [
            ("service_restart", json!({ "name": "nginx" })),
            ("kill_process", json!({ "pid": 1 })),
        ] {
            let (text, is_err) = call_text(
                &url,
                "recipe_run",
                json!({ "session_id": "nope", "recipe": recipe, "params": params }),
            )
            .await;
            assert!(
                is_err && text.contains("安全策略"),
                "{recipe} 应被拒: {text}"
            );
        }
        let (text, is_err) = call_text(
            &url,
            "recipe_run",
            json!({ "session_id": "nope", "recipe": "service_status", "params": { "name": "x;id" } }),
        )
        .await;
        assert!(is_err && text.contains("不合法"), "注入参数应被拒: {text}");
        let (text, _) = call_text(&url, "recipe_list", json!({})).await;
        assert!(text.contains("\"risk\":\"danger\""), "{text}");
        mgr.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rate_limit_setting_parse() {
        assert_eq!(parse_rate_setting(None), DEFAULT_RATE_PER_MINUTE);
        assert_eq!(parse_rate_setting(Some("30")), 30);
        assert_eq!(parse_rate_setting(Some("\"45\"")), 45);
        // 0 与越界回落默认
        assert_eq!(parse_rate_setting(Some("0")), DEFAULT_RATE_PER_MINUTE);
        assert_eq!(parse_rate_setting(Some("99999")), DEFAULT_RATE_PER_MINUTE);
    }

    fn all_perms() -> McpPerms {
        McpPerms {
            terminal: true,
            ..McpPerms::default()
        }
    }

    #[test]
    fn every_tool_carries_annotations() {
        let all = tools_list(&all_perms());
        for tool in all["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let a = &tool["annotations"];
            assert!(a.is_object(), "{name} 缺 annotations");
            let ro = a["readOnlyHint"].as_bool().unwrap();
            let destructive = a["destructiveHint"].as_bool().unwrap();
            assert!(!(ro && destructive), "{name} 不能既只读又可破坏");
        }
        let hint = |n: &str| tool_annotations(n)["readOnlyHint"].as_bool().unwrap();
        assert!(hint("sftp_read") && hint("terminal_read") && hint("list_sessions"));
        assert!(!hint("ssh_exec") && !hint("terminal_send") && !hint("sftp_write"));
    }

    #[test]
    fn protocol_version_negotiation() {
        assert_eq!(negotiate_protocol_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        // 未知/缺省 → 本端最新
        assert_eq!(negotiate_protocol_version(Some("1999-01-01")), "2025-11-25");
        assert_eq!(negotiate_protocol_version(None), "2025-11-25");
    }

    #[test]
    fn terminal_setting_absent_means_deny() {
        assert!(!parse_bool_setting(None));
        assert!(parse_bool_setting(Some("true")));
    }

    fn test_state(store: Arc<Store>, ui: Arc<crate::mcp_ui::McpUiHub>) -> ServerState {
        ServerState {
            store: store.clone(),
            sftp: crate::sftp::SftpManagerState::new(store),
            exec: crate::exec::ExecManagerState::new(tokio::runtime::Handle::current()),
            terms: crate::mcp_terminal::McpTerminalState::new(),
            ui,
            term_mgr: Arc::new(crate::terminal::TerminalManager::default()),
            backups: crate::mcp_files::BackupStore::new(
                std::env::temp_dir().join(format!("myssh-mcp-bk-{}", std::process::id())),
            ),
            jobs: crate::mcp_jobs::McpJobs::new(),
            recs: crate::recording::RecordingStore::new(
                std::env::temp_dir().join(format!("myssh-mcp-rec-{}", std::process::id())),
                None,
            ),
            token: String::new(),
            perms: all_perms(),
            policy: PolicyEngine::new(),
            rate: RateLimiter::new(DEFAULT_RATE_PER_MINUTE),
        }
    }

    /// 分组策略（生产 + standard）经 ssh_exec 生效：非只读命令弹审批，批准才放行，
    /// 拒绝/无窗口 fail-closed；全局 readonly 的普通会话仍直接拒绝。
    #[tokio::test]
    async fn ssh_exec_group_policy_and_approval() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-approval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        for (id, group) in [("prod", "生产/华东"), ("dev", "开发")] {
            let rec: core_store::SessionRecord = serde_json::from_value(json!({
                "id": id, "name": id, "host": "127.0.0.1", "port": 1, "username": "u",
                "authType": "password", "groupPath": group, "tags": [],
                "createdAt": "", "updatedAt": ""
            }))
            .unwrap();
            store.sessions().upsert(&rec).await.unwrap();
        }
        let policies = json!({ "生产": { "level": "standard", "production": true } });
        store
            .settings()
            .set(
                crate::mcp_policy::GROUP_POLICIES_KEY,
                &serde_json::to_string(&policies).unwrap(),
            )
            .await
            .unwrap();
        let args = |sid: &str| {
            json!({
                "session_id": sid,
                "command": "touch /tmp/x && ls",
                "timeout_ms": 3000,
                "purpose": "  建一个标记文件  ",
            })
        };

        // 无前端订阅：需确认的命令 fail-closed
        let st = test_state(store.clone(), crate::mcp_ui::McpUiHub::new());
        let e = ssh_exec_tool(&st, &args("prod")).await.unwrap_err();
        assert!(e.contains("审批通道不可用"), "{e}");
        // 全局 readonly 的会话：白名单外直接拒绝，不弹窗
        let e = ssh_exec_tool(&st, &args("dev")).await.unwrap_err();
        assert!(e.contains("readonly"), "{e}");

        // 有前端订阅：收到带生产标记的审批帧，按 allow 决定；always = 「总是允许」
        for (allow, always) in [(false, false), (true, false), (true, true)] {
            let hub = crate::mcp_ui::McpUiHub::new();
            let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
            let fs = frames.clone();
            hub.set_sink(Arc::new(move |v: &Value| {
                fs.lock().push(v.clone());
                true
            }));
            let h = hub.clone();
            let fr = frames.clone();
            let decider = tokio::spawn(async move {
                loop {
                    let id = fr
                        .lock()
                        .first()
                        .map(|v| v["id"].as_str().unwrap().to_string());
                    if let Some(id) = id {
                        if always {
                            assert!(h.allow_always(&id));
                        } else {
                            assert!(h.decide(&id, allow));
                        }
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
            let st = test_state(store.clone(), hub);
            let r = ssh_exec_tool(&st, &args("prod")).await;
            decider.await.unwrap();
            let frame = frames.lock()[0].clone();
            assert_eq!(frame["production"], true);
            assert_eq!(frame["level"], "standard");
            assert_eq!(frame["sessionName"], "prod");
            // 逐段分析 + agent 自述用途（去空白）
            assert_eq!(frame["purpose"], "建一个标记文件");
            assert_eq!(
                frame["parts"],
                json!([
                    { "text": "touch /tmp/x", "op": "&&", "head": "touch", "sub": "/tmp/x", "risk": "write" },
                    { "text": "ls", "op": null, "head": "ls", "sub": null, "risk": "readonly" },
                ])
            );
            if always {
                // 同会话同命令再调用：不再弹窗、不被策略拦截
                let e = ssh_exec_tool(&st, &args("prod")).await.unwrap_err();
                assert!(!e.contains("安全策略"), "总是允许后不应再拦截: {e}");
                let asks = frames
                    .lock()
                    .iter()
                    .filter(|f| f["type"] == "approval_request")
                    .count();
                assert_eq!(asks, 1, "总是允许后不应再弹审批");
                // 其他命令照常弹窗等待确认
                let mut other = args("prod");
                other["command"] = json!("touch /tmp/y");
                let waited =
                    tokio::time::timeout(Duration::from_millis(100), ssh_exec_tool(&st, &other))
                        .await;
                assert!(waited.is_err(), "未决定的审批不应返回");
                let asks = frames
                    .lock()
                    .iter()
                    .filter(|f| f["type"] == "approval_request")
                    .count();
                assert_eq!(asks, 2, "其他命令仍需确认");
            }
            let e = r.unwrap_err();
            if allow {
                // 放行后进入建连（端口 1 必然失败），不再是策略拒绝
                assert!(!e.contains("安全策略"), "批准后不应被策略拦截: {e}");
            } else {
                assert!(
                    !frames.lock().iter().any(|f| f["type"] == "exec_start"),
                    "被拒绝的命令不执行也不镜像"
                );
                assert!(e.contains("用户拒绝"), "{e}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// list_sessions 给出每个会话的有效授权：会话覆盖 → 分组策略 → 全局逐级解析
    #[tokio::test]
    async fn list_sessions_reports_effective_access() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-ls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        for (id, group, extra) in [
            ("prod", "生产", json!({})),
            (
                "dev",
                "开发",
                json!({ "mcpPerms": { "terminal": true }, "mcpLevel": "full" }),
            ),
        ] {
            let mut v = json!({
                "id": id, "name": id, "host": "127.0.0.1", "port": 22, "username": "u",
                "authType": "password", "groupPath": group, "tags": [],
                "createdAt": "", "updatedAt": ""
            });
            merge_json(&mut v, extra);
            let rec: core_store::SessionRecord = serde_json::from_value(v).unwrap();
            store.sessions().upsert(&rec).await.unwrap();
        }
        let policies = json!({
            "生产": { "level": "standard", "production": true, "perms": { "ssh_exec": false } }
        });
        store
            .settings()
            .set(
                crate::mcp_policy::GROUP_POLICIES_KEY,
                &serde_json::to_string(&policies).unwrap(),
            )
            .await
            .unwrap();
        let mut st = test_state(store, crate::mcp_ui::McpUiHub::new());
        st.perms.terminal = false;
        let body: Value = serde_json::from_str(&list_sessions_tool(&st).await.unwrap()).unwrap();
        let by_id = |id: &str| {
            body["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == id)
                .unwrap()
                .clone()
        };
        let prod = by_id("prod");
        assert_eq!(prod["ai"]["level"], "standard");
        assert_eq!(prod["ai"]["production"], true);
        assert_eq!(prod["ai"]["tools"]["ssh_exec"], false, "分组禁 ssh_exec");
        assert_eq!(prod["ai"]["tools"]["sftp_read"], true);
        assert_eq!(prod["ai"]["tools"]["terminal"], false, "全局默认关闭");
        assert_eq!(prod["openTabs"], 0);
        assert_eq!(prod["aiTerminals"], json!([]));
        let dev = by_id("dev");
        assert_eq!(dev["ai"]["level"], "full", "会话覆盖档位");
        assert_eq!(dev["ai"]["production"], false);
        assert_eq!(
            dev["ai"]["tools"]["terminal"], true,
            "会话覆盖开启 terminal"
        );
        assert_eq!(dev["ai"]["tools"]["ssh_exec"], true);
        assert_eq!(body["windowOnline"], false);
        assert!(!body.to_string().contains("password\":"), "不含凭据");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 执行镜像：窗口在线时 run_exec 推开始/结束帧（建连失败也要收尾）；关闭设置或无窗口不推
    #[tokio::test]
    async fn run_exec_mirrors_to_window() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-mirror-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let rec: core_store::SessionRecord = serde_json::from_value(json!({
            "id": "s1", "name": "web-01", "host": "127.0.0.1", "port": 1, "username": "u",
            "authType": "password", "groupPath": "", "tags": [],
            "createdAt": "", "updatedAt": ""
        }))
        .unwrap();
        store.sessions().upsert(&rec).await.unwrap();
        let hub = crate::mcp_ui::McpUiHub::new();
        let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
        let fs = frames.clone();
        hub.set_sink(Arc::new(move |v: &Value| {
            fs.lock().push(v.clone());
            true
        }));
        let st = test_state(store.clone(), hub);
        assert!(run_exec(&st, "s1", "uptime", 3000, true, "mcp_ssh_exec")
            .await
            .is_err());
        let got = frames.lock().clone();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0]["type"], "exec_start");
        assert_eq!(got[0]["tool"], "ssh_exec");
        assert_eq!(got[0]["sessionName"], "web-01");
        assert_eq!(got[0]["command"], "uptime");
        assert_eq!(got[1]["type"], "exec_end");
        assert_eq!(got[1]["execId"], got[0]["execId"]);
        assert_eq!(got[1]["status"], "error");
        assert!(got[1]["error"].is_string());

        store
            .settings()
            .set("mcp.exec_mirror", "false")
            .await
            .unwrap();
        frames.lock().clear();
        let _ = run_exec(&st, "s1", "uptime", 3000, true, "mcp_ssh_exec").await;
        assert!(frames.lock().is_empty(), "设置关闭后不镜像");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 轴一 1.3：MCP HTTP 面 E2E（真服务 + 真 JSON-RPC 往返） ----

    /// 起一台真实 MCP 服务（临时库 + 空闲端口）；调用方负责 mgr.stop()
    async fn boot_mcp(store: Arc<Store>) -> (Arc<McpManager>, String) {
        let mgr = McpManager::new();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let deps = McpDeps {
            store: store.clone(),
            sftp: crate::sftp::SftpManagerState::new(store),
            exec: crate::exec::ExecManagerState::new(tokio::runtime::Handle::current()),
            terms: crate::mcp_terminal::McpTerminalState::new(),
            ui: crate::mcp_ui::McpUiHub::new(),
            term_mgr: Arc::new(crate::terminal::TerminalManager::default()),
            backups: crate::mcp_files::BackupStore::new(
                std::env::temp_dir().join(format!("myssh-mcp-bk-{}", std::process::id())),
            ),
            jobs: crate::mcp_jobs::McpJobs::new(),
            recs: crate::recording::RecordingStore::new(
                std::env::temp_dir().join(format!("myssh-mcp-rec-{}", std::process::id())),
                None,
            ),
        };
        mgr.start(
            deps,
            port,
            String::new(), // 测试不启用 token
            all_perms(),
            DEFAULT_RATE_PER_MINUTE,
        )
        .await
        .expect("MCP 服务启动失败");
        (mgr, format!("http://127.0.0.1:{port}/mcp"))
    }

    async fn rpc(url: &str, method: &str, params: Value) -> Value {
        let resp = reqwest::Client::new()
            .post(url)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .send()
            .await
            .expect("HTTP 请求失败");
        assert_eq!(resp.status(), 200, "HTTP 状态异常");
        let v: Value = resp.json().await.expect("响应非 JSON");
        if let Some(err) = v.get("error") {
            panic!("RPC {method} 返回错误: {err}");
        }
        v["result"].clone()
    }

    /// tools/call 的文本载荷与 isError 标志
    async fn call_text(url: &str, name: &str, args: Value) -> (String, bool) {
        let r = rpc(
            url,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        )
        .await;
        (
            r["content"][0]["text"].as_str().unwrap_or("").to_string(),
            r["isError"].as_bool().unwrap_or(false),
        )
    }

    #[tokio::test]
    async fn e2e_http_initialize_and_tools_list() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let (mgr, url) = boot_mcp(store).await;

        let init = rpc(
            &url,
            "initialize",
            json!({ "protocolVersion": "2025-06-18" }),
        )
        .await;
        assert_eq!(init["serverInfo"]["name"], "myssh");
        assert_eq!(init["protocolVersion"], "2025-06-18");

        // 不支持的 MCP-Protocol-Version 头 → 400
        let resp = reqwest::Client::new()
            .post(&url)
            .header("MCP-Protocol-Version", "1999-01-01")
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let tools = rpc(&url, "tools/list", json!({})).await;
        assert_eq!(tools["tools"].as_array().unwrap().len(), 31);

        mgr.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ssh_exec / terminal_* 真机往返（CI Windows + OpenSSH.Server；vault 为 DPAPI，
    /// 仅 Windows 可跑）。目标主机密钥须已学入 %LOCALAPPDATA%/myssh/known_hosts
    /// （MCP 面无弹窗通路，host key fail-closed）——CI 步骤用 ssh-keyscan 预置。
    #[tokio::test]
    async fn e2e_ssh_exec_and_terminal_roundtrip() {
        let Ok(host) = std::env::var("MYSSH_E2E_HOST") else {
            return;
        };
        let port: u16 = std::env::var("MYSSH_E2E_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let user = std::env::var("MYSSH_E2E_USER").unwrap_or_else(|_| "tester".into());
        let password = std::env::var("MYSSH_E2E_PASSWORD").expect("MYSSH_E2E_PASSWORD 未设置");

        let dir = std::env::temp_dir().join(format!("myssh-mcp-e2e-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let rec = core_store::SessionRecord {
            id: "e2e".into(),
            name: "e2e".into(),
            kind: core_store::SessionKind::Ssh,
            host,
            port,
            user,
            auth_type: core_store::AuthType::Password,
            key_path: None,
            shell: None,
            workdir: None,
            jump_chain: vec![],
            group_path: String::new(),
            color: None,
            encoding: "utf-8".into(),
            su_user: None,
            login_macro: None,
            mcp_perms: Default::default(),
            mcp_level: None,
            mcp_production: None,
            tags: vec![],
            command: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        store.sessions().upsert(&rec).await.unwrap();
        store
            .credentials()
            .put(
                "e2e",
                core_store::CredentialKind::Password,
                &core_store::Secret::new(password.into_bytes()),
            )
            .await
            .unwrap();

        let (mgr, url) = boot_mcp(store).await;

        // ssh_exec 往返
        let (text, is_err) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "e2e", "command": "echo mcp-e2e-ok" }),
        )
        .await;
        assert!(!is_err, "ssh_exec 报错: {text}");
        assert!(
            text.contains("mcp-e2e-ok"),
            "ssh_exec 输出缺 marker: {text}"
        );

        // terminal_open → send → read → close 往返
        let (text, is_err) = call_text(&url, "terminal_open", json!({ "session_id": "e2e" })).await;
        assert!(!is_err, "terminal_open 报错: {text}");
        let term_id = serde_json::from_str::<Value>(&text).unwrap()["terminalId"]
            .as_str()
            .expect("terminal_open 未返回 terminalId")
            .to_string();

        // 读首轮（banner/提示符）拿到 offset 游标
        let (text, is_err) = call_text(
            &url,
            "terminal_read",
            json!({ "terminalId": term_id, "offset": 0, "wait_seconds": 5 }),
        )
        .await;
        assert!(!is_err, "terminal_read 报错: {text}");
        let mut offset = serde_json::from_str::<Value>(&text).unwrap()["nextOffset"]
            .as_u64()
            .expect("terminal_read 未返回 nextOffset");

        let (_, is_err) = call_text(
            &url,
            "terminal_send",
            json!({ "terminalId": term_id, "data": "echo term-e2e-ok\r" }),
        )
        .await;
        assert!(!is_err, "terminal_send 报错");

        let mut echoed = String::new();
        for _ in 0..10 {
            let (text, is_err) = call_text(
                &url,
                "terminal_read",
                json!({ "terminalId": term_id, "offset": offset, "wait_seconds": 5 }),
            )
            .await;
            assert!(!is_err, "terminal_read 报错: {text}");
            let snap = serde_json::from_str::<Value>(&text).unwrap();
            offset = snap["nextOffset"].as_u64().unwrap();
            echoed.push_str(snap["data"].as_str().unwrap_or(""));
            if echoed.contains("term-e2e-ok") {
                break;
            }
        }
        assert!(
            echoed.contains("term-e2e-ok"),
            "终端输出缺 marker: {echoed:?}"
        );

        let (_, is_err) = call_text(&url, "terminal_close", json!({ "terminalId": term_id })).await;
        assert!(!is_err, "terminal_close 报错");

        // job_* 往返：后台启动 → 游标读到结束 → job_list 可见
        let (text, is_err) = call_text(
            &url,
            "job_start",
            json!({ "session_id": "e2e", "command": "echo job-e2e-ok", "pty": false }),
        )
        .await;
        assert!(!is_err, "job_start 报错: {text}");
        let job_id = serde_json::from_str::<Value>(&text).unwrap()["jobId"]
            .as_str()
            .expect("job_start 未返回 jobId")
            .to_string();
        let (mut out, mut offset, mut status) = (String::new(), 0u64, String::new());
        for _ in 0..10 {
            let (text, is_err) = call_text(
                &url,
                "job_read",
                json!({ "job_id": job_id, "offset": offset, "wait_seconds": 5 }),
            )
            .await;
            assert!(!is_err, "job_read 报错: {text}");
            let v = serde_json::from_str::<Value>(&text).unwrap();
            offset = v["nextOffset"].as_u64().unwrap();
            out.push_str(v["data"].as_str().unwrap_or(""));
            status = v["status"].as_str().unwrap_or("").to_string();
            if status != "running" && !v["more"].as_bool().unwrap_or(false) {
                break;
            }
        }
        assert!(out.contains("job-e2e-ok"), "job 输出缺 marker: {out:?}");
        assert_eq!(status, "exited");
        let (text, _) = call_text(&url, "job_list", json!({ "session_id": "e2e" })).await;
        assert!(text.contains(&job_id), "job_list 缺任务: {text}");
        // file_* 往返：新建 → 查看 → 编辑 → grep/glob → 回滚编辑 → 回滚新建（删除）
        let call = |name: &'static str, args: Value| {
            let url = url.clone();
            async move {
                let (text, is_err) = call_text(&url, name, args).await;
                assert!(!is_err, "{name} 报错: {text}");
                text
            }
        };
        let dir_r = format!("myssh-e2e-files-{}", std::process::id());
        let file_r = format!("{dir_r}/app.conf");
        call("sftp_mkdir", json!({ "session_id": "e2e", "path": dir_r })).await;
        let created = call(
            "file_edit",
            json!({ "session_id": "e2e", "path": file_r, "old_str": "", "new_str": "port=80\nname=web\n" }),
        )
        .await;
        let create_bk = serde_json::from_str::<Value>(&created).unwrap()["backupId"]
            .as_str()
            .unwrap()
            .to_string();
        let view = call("file_view", json!({ "session_id": "e2e", "path": file_r })).await;
        assert!(view.contains("     1\tport=80"), "{view}");
        let edited = call(
            "file_edit",
            json!({ "session_id": "e2e", "path": file_r, "old_str": "port=80", "new_str": "port=8080" }),
        )
        .await;
        let edit_bk = serde_json::from_str::<Value>(&edited).unwrap()["backupId"]
            .as_str()
            .unwrap()
            .to_string();
        let grep = call(
            "file_grep",
            json!({ "session_id": "e2e", "path": dir_r, "pattern": "port=\\d+" }),
        )
        .await;
        assert!(grep.contains("app.conf:1:port=8080"), "{grep}");
        let glob = call(
            "file_glob",
            json!({ "session_id": "e2e", "path": dir_r, "pattern": "*.conf" }),
        )
        .await;
        assert!(glob.contains("app.conf"), "{glob}");
        call(
            "file_rollback",
            json!({ "session_id": "e2e", "backup_id": edit_bk }),
        )
        .await;
        let view = call("file_view", json!({ "session_id": "e2e", "path": file_r })).await;
        assert!(
            view.contains("port=80\n") && !view.contains("8080"),
            "{view}"
        );
        // 新建备份的写后哈希 = 新建内容；编辑后又回滚回同内容，哈希一致 → 回滚即删除
        call(
            "file_rollback",
            json!({ "session_id": "e2e", "backup_id": create_bk }),
        )
        .await;
        let (text, is_err) = call_text(
            &url,
            "file_view",
            json!({ "session_id": "e2e", "path": file_r }),
        )
        .await;
        assert!(
            is_err && text.contains("不存在"),
            "回滚新建应删除文件: {text}"
        );
        call("sftp_delete", json!({ "session_id": "e2e", "path": dir_r })).await;

        mgr.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
