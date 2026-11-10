//! MCP 交互式终端工具：terminal_open / terminal_send / terminal_read / terminal_close。
//!
//! 与 ssh_exec 的一次性 exec 不同，本模块持有有状态 PTY shell——agent 可 cd、
//! 进 mysql、激活 venv，状态跨调用保持。terminal_open 建独立 Bulk 连接并开
//! xterm PTY（120x32），读任务单消费者循环把输出追加进 1MiB 环形缓冲
//!（超界丢最旧并置 truncated 标志）；terminal_read 以 offset 全局字节游标
//! 轮询（wait_seconds 服务端长轮询，上限 600s）；terminal_close 关通道并
//! abort 读任务。MCP 服务 stop/重启时 close_all 清场，不留孤儿连接。
//! 建连安全策略与 ssh_exec 一致：host key fail-closed（未知/变更指纹拒绝，
//! 需先在 UI 首连确认）、keyboard-interactive 无应答通路预拒；open/close
//! 落审计（actor=mcp，动作 mcp_terminal_open/mcp_terminal_close）。
//!
//! mySSH 窗口在线（McpUiHub 有订阅）时，terminal_open 改为请前端开一个普通标签，
//! 输出取自该标签读循环的旁路（AgentTap），输入经 TerminalManager 仲裁：用户键入后
//! 短时间内 AI 写入被拒（用户优先），用户可随时收回控制。用户也可把已有标签共享给
//! AI（mcp_share_tab），agent 用 terminal_list 发现。MCP 停止时收回全部控制，标签保留。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use core_ssh::{PtyChannel, PtyWriter, SshConnection};
use core_store::Store;

/// 终端输出环形缓冲容量：超界丢最旧（输出要进模型上下文，必须有界）
const TERM_BUF_CAP: usize = 1024 * 1024;
/// terminal_read 服务端长轮询间隔（仿 wait_transfer）
const READ_POLL: Duration = Duration::from_millis(500);
/// terminal_read 单次 wait 上限（秒；与 sftp_upload/download 的 wait_seconds 一致）
const READ_WAIT_MAX: u64 = 600;
/// terminal_read 单次默认返回的原始字节数（max_bytes 上限为整个缓冲）
const READ_DEFAULT_BYTES: usize = 64 * 1024;
/// terminal_open 建连 + 开 PTY 的超时（对齐 ssh_exec 默认预算）
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// headless 终端的 PTY 尺寸
const HEADLESS_COLS: u32 = 120;
const HEADLESS_ROWS: u32 = 32;

/// 终端输出环形缓冲。offset 为全局字节游标：缓冲起点 = total - buf.len()。
#[derive(Default)]
pub(crate) struct TermBuf {
    pub(crate) buf: VecDeque<u8>,
    /// 累计写入字节数（只增）
    pub(crate) total: u64,
    /// 曾因容量超界丢弃过最旧数据（旧 offset 可能已失效）
    pub(crate) truncated: bool,
    /// 读任务 EOF/Close、terminal_close、UI 标签关闭或收回控制后置位
    pub(crate) closed: bool,
}

impl TermBuf {
    /// 追加输出；超容量时丢弃最旧字节并置 truncated 标志
    pub(crate) fn append(&mut self, data: &[u8]) {
        self.total += data.len() as u64;
        self.buf.extend(data);
        if self.buf.len() > TERM_BUF_CAP {
            let drop = self.buf.len() - TERM_BUF_CAP;
            self.buf.drain(..drop);
            self.truncated = true;
        }
    }

    /// 缓冲中最早可用字节的全局 offset
    pub(crate) fn start(&self) -> u64 {
        self.total - self.buf.len() as u64
    }
}

/// 一页读取结果（游标模型共用：terminal_read / job_read）
pub(crate) struct Page {
    pub(crate) data: String,
    pub(crate) next: u64,
    pub(crate) more: bool,
    pub(crate) truncated: bool,
}

/// 从 offset 起读至多 max 原始字节。锁内只拷贝，剥离在锁外做，不阻塞输出泵
pub(crate) fn read_page(shared: &Mutex<TermBuf>, offset: u64, max: usize, strip: bool) -> Page {
    let (raw, from, at_end, total, start) = {
        let b = shared.lock();
        let start = b.start();
        let from = offset.max(start);
        let skip = (from - start) as usize;
        let avail = (b.total - from) as usize;
        let take = avail.min(max);
        let raw: Vec<u8> = b.buf.range(skip..skip + take).copied().collect();
        (raw, from, b.closed && take == avail, b.total, start)
    };
    let (data, used) = if strip {
        crate::mcp_text::strip_ansi_page(&raw, at_end)
    } else {
        (String::from_utf8_lossy(&raw).into_owned(), raw.len())
    };
    let next = from + used as u64;
    Page {
        data,
        next,
        more: next < total,
        truncated: offset < start,
    }
}
/// 终端的 I/O 承载
enum TermIo {
    /// 无界面：独立 Bulk 连接 + PTY（无 UI 订阅或 agent 显式要求 headless）
    Headless {
        writer: PtyWriter,
        /// 读任务句柄（close 时 abort）
        task: tauri::async_runtime::JoinHandle<()>,
        /// 保持连接存活：SshConnection drop 即断连，会带走 PTY 通道
        conn: SshConnection,
        /// 录像（建文件失败时为 None，终端照常可用）
        rec: Option<Arc<crate::recording::Recorder>>,
    },
    /// UI 标签承载：输出来自标签读循环旁路，输入经 TerminalManager 仲裁（用户优先）
    Ui {
        tab_id: String,
        tap: Arc<crate::terminal::AgentTap>,
        kind: UiKind,
        /// AI 接入时自动开始的录像及所在槽（退出控制时只停这一份）
        rec: Option<(Arc<crate::recording::Recorder>, crate::recording::RecSlot)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiKind {
    /// agent 经 terminal_open 打开、以 UI 标签展示
    Agent,
    /// 用户把已有标签共享给 AI
    Shared,
}

impl UiKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent_tab",
            Self::Shared => "shared_tab",
        }
    }
}

/// 单个 agent 终端（注册进 McpTerminalState）
struct McpTerm {
    /// 会话档案 id（权限闸与审计用；共享的临时连接标签为 None）
    session_id: Option<String>,
    /// 输出缓冲（UI 承载时即旁路缓冲本身）
    shared: Arc<Mutex<TermBuf>>,
    io: TermIo,
}

impl McpTerm {
    fn ui_tab(&self) -> Option<&str> {
        match &self.io {
            TermIo::Ui { tab_id, .. } => Some(tab_id),
            TermIo::Headless { .. } => None,
        }
    }

    /// 释放：headless 关通道 + abort 读任务；UI 承载只收回控制，标签留给用户
    async fn shutdown(&self) {
        match &self.io {
            TermIo::Headless {
                writer,
                task,
                conn,
                rec,
            } => {
                if !conn.is_closed() {
                    let _ = writer.close().await;
                }
                task.abort();
                if let Some(r) = rec {
                    r.stop();
                }
            }
            TermIo::Ui { tap, rec, .. } => {
                tap.close();
                if let Some((r, slot)) = rec {
                    crate::recording::release_agent(slot, r);
                }
            }
        }
        self.shared.lock().closed = true;
    }
}

/// MCP 终端注册表：terminalId → 终端
#[derive(Default)]
pub struct McpTerminalState {
    terms: Mutex<HashMap<String, Arc<McpTerm>>>,
}

impl McpTerminalState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// terminalId → 打开它的会话档案 id（权限闸按会话覆盖判定用）
    pub(crate) fn session_of(&self, id: &str) -> Option<String> {
        self.terms.lock().get(id).and_then(|s| s.session_id.clone())
    }

    /// 清场：MCP 服务停止/重启时关闭全部终端；返回被收回控制的 UI 标签（供前端摘标记）
    pub async fn close_all(&self) -> Vec<String> {
        let all: Vec<Arc<McpTerm>> = self.terms.lock().drain().map(|(_, s)| s).collect();
        let mut tabs = Vec::new();
        for sess in all {
            sess.shutdown().await;
            if let Some(t) = sess.ui_tab() {
                tabs.push(t.to_string());
            }
        }
        tabs
    }

    fn insert_ui(
        &self,
        tab_id: &str,
        tap: Arc<crate::terminal::AgentTap>,
        session_id: Option<String>,
        kind: UiKind,
        rec: Option<(Arc<crate::recording::Recorder>, crate::recording::RecSlot)>,
    ) -> String {
        let id = gen_term_id();
        let term = McpTerm {
            session_id,
            shared: tap.out.clone(),
            io: TermIo::Ui {
                tab_id: tab_id.to_string(),
                tap,
                kind,
                rec,
            },
        };
        self.terms.lock().insert(id.clone(), Arc::new(term));
        id
    }

    /// 用户把 UI 标签共享给 AI；返回 terminalId
    pub(crate) fn share_tab(
        &self,
        mgr: &crate::terminal::TerminalManager,
        recs: &crate::recording::RecordingStore,
        tab_id: &str,
        title: &str,
    ) -> Result<(String, Option<String>), String> {
        let (tap, session_id) = mgr.attach_tap(tab_id)?;
        let rec = agent_recording(mgr, recs, tab_id, title);
        let id = self.insert_ui(tab_id, tap, session_id.clone(), UiKind::Shared, rec);
        Ok((id, session_id))
    }

    /// 用户收回某 UI 标签的 AI 控制（共享或 agent 打开的均可）；返回被收回的会话 id
    pub(crate) async fn revoke_tab(&self, tab_id: &str) -> Option<Option<String>> {
        let removed: Vec<Arc<McpTerm>> = {
            let mut map = self.terms.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, t)| t.ui_tab() == Some(tab_id))
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        let mut sid = None;
        for t in &removed {
            t.shutdown().await;
            sid = Some(t.session_id.clone());
        }
        sid
    }

    fn get(&self, id: &str) -> Result<Arc<McpTerm>, String> {
        self.terms
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("终端 {id} 不存在或已关闭"))
    }
}

/// AI 接入 UI 标签时自动开始录像（已在录则沿用）；建文件失败只记日志，不挡终端
fn agent_recording(
    mgr: &crate::terminal::TerminalManager,
    recs: &crate::recording::RecordingStore,
    tab_id: &str,
    title: &str,
) -> Option<(Arc<crate::recording::Recorder>, crate::recording::RecSlot)> {
    match mgr.start_recording(tab_id, recs, title, crate::recording::RecSource::Agent) {
        Ok(pair) => Some(pair),
        Err(e) => {
            tracing::warn!(tab_id, error = %e, "AI 终端录像启动失败");
            None
        }
    }
}

/// terminal_* 工具执行所需的应用级状态
pub(crate) struct TermCtx<'a> {
    pub store: &'a Arc<Store>,
    pub terms: &'a Arc<McpTerminalState>,
    pub ui: &'a Arc<crate::mcp_ui::McpUiHub>,
    pub mgr: &'a Arc<crate::terminal::TerminalManager>,
    pub recs: &'a Arc<crate::recording::RecordingStore>,
}

/// terminal_* 工具分发（外层 call_tool 已按 terminal_ 前缀过滤，此处校验具体名）
pub(crate) async fn terminal_tool(
    ctx: &TermCtx<'_>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "terminal_open" => terminal_open(ctx, args).await,
        "terminal_send" => terminal_send(ctx, args).await,
        "terminal_read" => terminal_read(ctx.terms, args).await,
        "terminal_close" => terminal_close(ctx, args).await,
        "terminal_list" => Ok(terminal_list(ctx.terms)),
        other => Err(format!("未知工具：{other}")),
    }
}

/// UI 打开标签的等待上限：覆盖首连主机指纹确认弹窗（120s）与建连
const UI_OPEN_TIMEOUT: Duration = Duration::from_secs(150);

/// terminal_open：有 UI 订阅时以 mySSH 标签打开（用户可旁观/接管），否则
/// resolve → KI 预拒 → Bulk 独立连接（host key fail-closed）→ xterm PTY → 注册
async fn terminal_open(ctx: &TermCtx<'_>, args: &Value) -> Result<String, String> {
    let store = ctx.store;
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "terminal_open 缺少参数 session_id".to_string())?;
    let target = crate::sessions::resolve_session_target(store, session_id).await?;
    let spec = match target {
        crate::sessions::ResolvedTarget::Ssh(s) => s,
        crate::sessions::ResolvedTarget::Local(_) => {
            return Err("本地会话不支持 terminal_open（仅 SSH 会话）".into());
        }
    };
    let headless = args
        .get("headless")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !headless && ctx.ui.has_ui() {
        return terminal_open_ui(ctx, session_id, &spec.host).await;
    }
    // KI 无应答通路：预拒（策略收口于 connect 模块，卡 5）
    crate::connect::reject_background_ki(&spec).map_err(|e| e.to_string())?;
    let host = spec.host.clone();
    let run = terminal_open_inner(spec);
    let (conn, pty) = match tokio::time::timeout(OPEN_TIMEOUT, run).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(format!(
                "终端打开超时（{}s），连接已断开",
                OPEN_TIMEOUT.as_secs()
            ));
        }
    };

    let (mut reader, writer) = pty.split();
    let id = gen_term_id();
    let shared = Arc::new(Mutex::new(TermBuf::default()));
    let rec = ctx
        .recs
        .start(
            &format!("AI · {host}"),
            crate::recording::RecSource::Agent,
            Some(session_id.to_string()),
            HEADLESS_COLS,
            HEADLESS_ROWS,
        )
        .inspect_err(|e| tracing::warn!(error = %e, "AI 终端录像启动失败"))
        .ok();
    // 读任务：PTY 读半单消费者循环追加缓冲；EOF/Close → 置 closed 退出
    let task = {
        let shared = Arc::clone(&shared);
        let rec = rec.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(data) = reader.next_data().await {
                if let Some(r) = &rec {
                    r.output(&data);
                }
                shared.lock().append(&data);
            }
            shared.lock().closed = true;
            if let Some(r) = &rec {
                r.stop();
            }
        })
    };
    let sess = Arc::new(McpTerm {
        session_id: Some(session_id.to_string()),
        shared,
        io: TermIo::Headless {
            writer,
            task,
            conn,
            rec,
        },
    });
    ctx.terms.terms.lock().insert(id.clone(), sess);
    audit(
        store,
        session_id,
        "mcp_terminal_open",
        &json!({ "terminalId": id, "host": host, "visible": false }),
    )
    .await;
    tracing::info!(session_id, terminal_id = %id, host = %host, "MCP 终端已打开（headless）");
    Ok(json!({ "terminalId": id, "visible": false }).to_string())
}

/// 请前端开一个普通标签（走 UI 的 hostkey/KI 弹窗），连上后挂旁路
async fn terminal_open_ui(
    ctx: &TermCtx<'_>,
    session_id: &str,
    host: &str,
) -> Result<String, String> {
    let name = ctx
        .store
        .sessions()
        .get(session_id)
        .await
        .map(|r| r.name)
        .unwrap_or_default();
    let (req, rx) = ctx.ui.request_tab(session_id, &name)?;
    let tab_id = match tokio::time::timeout(UI_OPEN_TIMEOUT, rx).await {
        Ok(Ok(Ok(t))) => t,
        Ok(Ok(Err(e))) => return Err(format!("mySSH 标签连接失败：{e}")),
        Ok(Err(_)) => return Err("mySSH 标签打开被取消".into()),
        Err(_) => {
            ctx.ui.cancel_tab(&req);
            return Err(format!(
                "mySSH 标签打开超时（{}s）",
                UI_OPEN_TIMEOUT.as_secs()
            ));
        }
    };
    let (tap, _) = ctx.mgr.attach_tap(&tab_id)?;
    let rec = agent_recording(ctx.mgr, ctx.recs, &tab_id, &format!("AI · {name}"));
    let id = ctx.terms.insert_ui(
        &tab_id,
        tap,
        Some(session_id.to_string()),
        UiKind::Agent,
        rec,
    );
    ctx.ui.emit(&json!({
        "type": "agent_terminal_attached",
        "tabId": tab_id,
        "terminalId": id,
        "kind": "agent",
    }));
    audit(
        ctx.store,
        session_id,
        "mcp_terminal_open",
        &json!({ "terminalId": id, "host": host, "visible": true, "tabId": tab_id }),
    )
    .await;
    tracing::info!(session_id, terminal_id = %id, tab_id = %tab_id, "MCP 终端已打开（UI 标签）");
    Ok(json!({
        "terminalId": id,
        "visible": true,
        "note": "终端以 mySSH 标签页打开：用户可旁观；用户键入时 AI 写入会被暂时拒绝，用户也可随时收回控制",
    })
    .to_string())
}

/// 建连 + 开 PTY：策略收口于 connect 模块（卡 5）；hostkey 失败补 UI 首连提示
async fn terminal_open_inner(
    spec: crate::connect::TermOpenSpec,
) -> Result<(SshConnection, PtyChannel), String> {
    let conn = crate::connect::background(&spec, crate::connect::ConnectProfile::Control)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("主机密钥") {
                format!("{msg}（未知/变更的主机密钥：请先在 mySSH UI 中连接一次该会话以确认指纹）")
            } else {
                msg
            }
        })?;
    let pty = conn
        .open_pty("xterm", HEADLESS_COLS, HEADLESS_ROWS, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok((conn, pty))
}

/// terminal_send：写 UTF-8 文本；append_newline=true 时末尾补 \n
///（缺省 false，换行由 agent 自行包含在 data 中）
async fn terminal_send(ctx: &TermCtx<'_>, args: &Value) -> Result<String, String> {
    let id = req_term_id(args, "terminal_send")?;
    let data = args
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "terminal_send 缺少参数 data".to_string())?;
    let append_newline = args
        .get("append_newline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sess = ctx.terms.get(id)?;
    if sess.shared.lock().closed {
        return Err(format!(
            "终端 {id} 已关闭（远端 shell 已退出、连接断开或用户已收回控制）"
        ));
    }
    let mut bytes = data.as_bytes().to_vec();
    if append_newline {
        bytes.push(b'\n');
    }
    match &sess.io {
        TermIo::Headless { writer, rec, .. } => {
            writer.write(&bytes).await.map_err(|e| e.to_string())?;
            if let Some(r) = rec {
                r.input(&bytes);
            }
        }
        TermIo::Ui { tab_id, tap, .. } => ctx.mgr.agent_write(tab_id, tap, &bytes).await?,
    }
    Ok(json!({ "ok": true }).to_string())
}

/// terminal_read：offset 游标轮询。wait_seconds>0 时服务端长轮询到就绪/终端关闭/
/// 等待到期（上限 600s），不往模型上下文塞空轮询。就绪默认 = 有新输出；给了
/// wait_for（正则，匹配剥离 ANSI 后的新输出）或 idle_ms（输出静默时长）则按其判定，
/// 二者任一满足即返回。旧 offset 因环形丢弃失效时 truncated=true 并从最早可用处返回。
async fn terminal_read(terms: &Arc<McpTerminalState>, args: &Value) -> Result<String, String> {
    let id = req_term_id(args, "terminal_read")?;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| "terminal_read 缺少参数 offset".to_string())?;
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(READ_WAIT_MAX);
    let max = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .map_or(READ_DEFAULT_BYTES, |v| {
            usize::try_from(v).unwrap_or(TERM_BUF_CAP)
        })
        .clamp(1, TERM_BUF_CAP);
    let strip = args
        .get("strip_ansi")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let wait_for = args
        .get("wait_for")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(|p| {
            regex::RegexBuilder::new(p)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| format!("wait_for 不是合法正则: {e}"))
        })
        .transpose()?;
    let idle = args
        .get("idle_ms")
        .and_then(Value::as_u64)
        .filter(|&ms| ms > 0)
        .map(|ms| Duration::from_millis(ms.min(READ_WAIT_MAX * 1000)));
    let sess = terms.get(id)?;
    let deadline = Instant::now() + Duration::from_secs(wait);
    let mut seen = offset;
    let mut changed_at = Instant::now();
    let mut matched = false;
    loop {
        let (total, closed) = {
            let b = sess.shared.lock();
            (b.total, b.closed)
        };
        let now = Instant::now();
        if total != seen {
            seen = total;
            changed_at = now;
            if let Some(re) = &wait_for {
                let all = read_page(&sess.shared, offset, TERM_BUF_CAP, true);
                matched = re.is_match(&all.data);
            }
        }
        let ready = offset < total
            && if wait_for.is_none() && idle.is_none() {
                true
            } else {
                matched || idle.is_some_and(|d| now.duration_since(changed_at) >= d)
            };
        if ready || closed || now >= deadline {
            let page = read_page(&sess.shared, offset, max, strip);
            let mut v = json!({
                "data": page.data,
                "nextOffset": page.next,
                "more": page.more,
                "closed": closed,
                "truncated": page.truncated,
            });
            if wait_for.is_some() {
                v["matched"] = json!(matched);
            }
            return Ok(v.to_string());
        }
        tokio::time::sleep(READ_POLL).await;
    }
}
/// terminal_close：移出注册表 → 释放（落审计）。AI 自己打开的标签请前端一并关闭；
/// 用户共享的标签只收回控制、标签留给用户
async fn terminal_close(ctx: &TermCtx<'_>, args: &Value) -> Result<String, String> {
    let id = req_term_id(args, "terminal_close")?;
    close_terminal(ctx.terms, ctx.ui, ctx.store, id, core_store::Actor::Mcp)
        .await
        .map(|v| v.to_string())
}

/// 关闭一个 AI 终端（agent 的 terminal_close 与用户在活动面板停止共用）
pub(crate) async fn close_terminal(
    terms: &McpTerminalState,
    ui: &crate::mcp_ui::McpUiHub,
    store: &Arc<Store>,
    id: &str,
    actor: core_store::Actor,
) -> Result<Value, String> {
    let sess = terms
        .terms
        .lock()
        .remove(id)
        .ok_or_else(|| format!("终端 {id} 不存在或已关闭"))?;
    sess.shutdown().await;
    let mut out = json!({ "closed": true });
    if let TermIo::Ui { tab_id, kind, .. } = &sess.io {
        let close_tab = *kind == UiKind::Agent;
        let sent = ui.emit(&json!({
            "type": "agent_terminal_closed",
            "tabId": tab_id,
            "closeTab": close_tab,
        }));
        let (tab, note) = match (close_tab, sent) {
            (true, true) => (
                "closed",
                "AI 打开的 mySSH 标签已一并关闭，该标签的 SSH 连接随之断开",
            ),
            (true, false) => ("kept", "mySSH 窗口不在线，标签未能关闭，需用户手动关闭"),
            (false, _) => (
                "kept",
                "该标签是用户共享给 AI 的：只收回 AI 控制，标签与连接保留给用户",
            ),
        };
        out["tab"] = json!(tab);
        out["note"] = json!(note);
    }
    if let Some(sid) = &sess.session_id {
        let _ = store
            .audit()
            .append(
                actor,
                Some(sid),
                "mcp_terminal_close",
                &json!({ "terminalId": id }),
            )
            .await;
    }
    tracing::info!(terminal_id = %id, "MCP 终端已关闭");
    Ok(out)
}

impl McpTerminalState {
    /// 未关闭的 AI 终端，供 AI 活动面板（UI 承载的带 tabId，便于前端对应到标签）
    pub(crate) fn live_snapshot(&self) -> Vec<Value> {
        self.terms
            .lock()
            .iter()
            .filter(|(_, t)| !t.shared.lock().closed)
            .map(|(id, t)| {
                let kind = match &t.io {
                    TermIo::Headless { .. } => "headless",
                    TermIo::Ui { kind, .. } => kind.as_str(),
                };
                json!({
                    "terminalId": id,
                    "sessionId": t.session_id,
                    "kind": kind,
                    "tabId": t.ui_tab(),
                })
            })
            .collect()
    }

    /// 会话档案 id → 未关闭的 AI 终端（terminalId + kind），供 list_sessions 展示
    pub(crate) fn live_by_session(&self) -> HashMap<String, Vec<Value>> {
        let mut out: HashMap<String, Vec<Value>> = HashMap::new();
        for (id, t) in self.terms.lock().iter() {
            let Some(sid) = &t.session_id else { continue };
            if t.shared.lock().closed {
                continue;
            }
            let kind = match &t.io {
                TermIo::Headless { .. } => "headless",
                TermIo::Ui { kind, .. } => kind.as_str(),
            };
            out.entry(sid.clone())
                .or_default()
                .push(json!({ "terminalId": id, "kind": kind }));
        }
        out
    }
}

/// terminal_list：当前可用终端（含用户共享给 AI 的标签）
fn terminal_list(terms: &Arc<McpTerminalState>) -> String {
    let map = terms.terms.lock();
    let items: Vec<Value> = map
        .iter()
        .map(|(id, t)| {
            let kind = match &t.io {
                TermIo::Headless { .. } => "headless",
                TermIo::Ui { kind, .. } => kind.as_str(),
            };
            json!({
                "terminalId": id,
                "sessionId": t.session_id,
                "kind": kind,
                "closed": t.shared.lock().closed,
            })
        })
        .collect();
    json!({ "terminals": items }).to_string()
}

/// 用户把当前标签共享给 AI（标签右键菜单）；须 MCP 已启用且该会话 terminal 权限放行
#[tauri::command]
pub async fn mcp_share_tab(
    tab_id: String,
    title: Option<String>,
    terms: tauri::State<'_, Arc<McpTerminalState>>,
    mgr: tauri::State<'_, Arc<crate::terminal::TerminalManager>>,
    recs: tauri::State<'_, Arc<crate::recording::RecordingStore>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<Value, String> {
    let store = &sessions.store;
    let session_id = mgr.session_of_tab(&tab_id);
    if !crate::mcp::terminal_usable(store, session_id.as_deref()).await {
        return Err(
            "MCP 未启用或该会话未放行「交互终端」权限（设置 → MCP / 会话编辑器 → MCP 权限）".into(),
        );
    }
    let title = format!("AI · {}", title.as_deref().unwrap_or(&tab_id));
    let (id, sid) = terms.share_tab(&mgr, &recs, &tab_id, &title)?;
    let _ = store
        .audit()
        .append(
            core_store::Actor::Gui,
            sid.as_deref(),
            "mcp_terminal_share",
            &json!({ "terminalId": id, "tabId": tab_id }),
        )
        .await;
    Ok(json!({ "terminalId": id }))
}

/// 用户收回标签的 AI 控制（共享或 agent 打开的标签）
#[tauri::command]
pub async fn mcp_unshare_tab(
    tab_id: String,
    terms: tauri::State<'_, Arc<McpTerminalState>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<bool, String> {
    let Some(sid) = terms.revoke_tab(&tab_id).await else {
        return Ok(false);
    };
    let _ = sessions
        .store
        .audit()
        .append(
            core_store::Actor::Gui,
            sid.as_deref(),
            "mcp_terminal_revoke",
            &json!({ "tabId": tab_id }),
        )
        .await;
    Ok(true)
}
/// 生成随机 terminal id（16 字节 hex，仿 mcp.rs gen_token）
fn gen_term_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn req_term_id<'a>(args: &'a Value, tool: &str) -> Result<&'a str, String> {
    args.get("terminalId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 terminalId"))
}

/// MCP 侧写操作审计（Actor::Mcp，照 mcp.rs mcp_audit）
async fn audit(store: &Arc<Store>, session_id: &str, action: &str, detail: &Value) {
    let _ = store
        .audit()
        .append(core_store::Actor::Mcp, Some(session_id), action, detail)
        .await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn term_buf_ring_drops_oldest() {
        let mut b = TermBuf::default();
        b.append(&vec![1u8; TERM_BUF_CAP]);
        assert_eq!(b.start(), 0);
        assert!(!b.truncated);
        // 再写 10 字节 → 丢最旧 10 字节，起点前移
        b.append(&[2u8; 10]);
        assert!(b.truncated);
        assert_eq!(b.start(), 10);
        assert_eq!(b.total, TERM_BUF_CAP as u64 + 10);
        assert_eq!(b.buf.len(), TERM_BUF_CAP);
        assert_eq!(b.buf[0], 1);
        assert_eq!(b.buf[TERM_BUF_CAP - 1], 2);
    }

    #[test]
    fn term_buf_oversized_chunk_keeps_tail() {
        let mut b = TermBuf::default();
        // 单块超容量：只保留尾部 CAP 字节
        let mut data = vec![0u8; TERM_BUF_CAP + 5];
        data[TERM_BUF_CAP + 4] = 7;
        b.append(&data);
        assert!(b.truncated);
        assert_eq!(b.start(), 5);
        assert_eq!(b.buf.len(), TERM_BUF_CAP);
        assert_eq!(b.buf[TERM_BUF_CAP - 1], 7);
    }

    #[test]
    fn user_input_blocks_agent_briefly() {
        use std::time::{Duration, Instant};
        let tap = crate::terminal::AgentTap::new();
        let t0 = Instant::now();
        assert!(!tap.user_active(t0), "无用户输入时 AI 可写");
        tap.mark_user_input_at(t0);
        assert!(tap.user_active(t0 + Duration::from_millis(500)));
        assert!(
            !tap.user_active(t0 + Duration::from_secs(2)),
            "超过优先窗后 AI 恢复写入"
        );
    }

    /// terminal_close：AI 自己开的标签请前端关闭（tab=closed）；用户共享的只收回控制（tab=kept）；
    /// 窗口不在线时如实报告未能关闭
    #[tokio::test]
    async fn close_agent_tab_closes_ui_tab_shared_tab_kept() {
        let dir = std::env::temp_dir().join(format!("myssh-term-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.unwrap());
        let terms = McpTerminalState::new();
        let ui = crate::mcp_ui::McpUiHub::new();
        let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
        let fs = frames.clone();
        ui.set_sink(Arc::new(move |v: &Value| {
            fs.lock().push(v.clone());
            true
        }));
        let mgr = Arc::new(crate::terminal::TerminalManager::default());
        let recs = crate::recording::RecordingStore::new(dir.join("rec"), None);
        let ctx = TermCtx {
            store: &store,
            terms: &terms,
            ui: &ui,
            mgr: &mgr,
            recs: &recs,
        };
        let close = |id: String| {
            let ctx = &ctx;
            async move {
                let out = terminal_close(ctx, &json!({ "terminalId": id }))
                    .await
                    .unwrap();
                serde_json::from_str::<Value>(&out).unwrap()
            }
        };

        let tap_a = Arc::new(crate::terminal::AgentTap::new());
        let a = terms.insert_ui("tabA", tap_a.clone(), None, UiKind::Agent, None);
        let v = close(a.clone()).await;
        assert_eq!(v["tab"], "closed", "{v}");
        assert!(tap_a.is_closed());
        assert!(terms.get(&a).is_err());
        let f = frames.lock().last().cloned().unwrap();
        assert_eq!(f["type"], "agent_terminal_closed");
        assert_eq!(f["tabId"], "tabA");
        assert_eq!(f["closeTab"], true);

        let s = terms.insert_ui(
            "tabS",
            Arc::new(crate::terminal::AgentTap::new()),
            None,
            UiKind::Shared,
            None,
        );
        let v = close(s).await;
        assert_eq!(v["tab"], "kept", "{v}");
        assert_eq!(frames.lock().last().unwrap()["closeTab"], false);

        // 窗口不在线：AI 标签关不掉，返回 kept 并说明
        ui.set_sink(Arc::new(|_: &Value| false));
        let b = terms.insert_ui(
            "tabB",
            Arc::new(crate::terminal::AgentTap::new()),
            None,
            UiKind::Agent,
            None,
        );
        let v = close(b).await;
        assert_eq!(v["tab"], "kept", "{v}");
        assert!(v["note"].as_str().unwrap().contains("不在线"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn revoke_only_affects_that_tab_and_keeps_others() {
        let st = McpTerminalState::new();
        let tap_a = Arc::new(crate::terminal::AgentTap::new());
        let tap_b = Arc::new(crate::terminal::AgentTap::new());
        let a = st.insert_ui(
            "tabA",
            tap_a.clone(),
            Some("s1".into()),
            UiKind::Shared,
            None,
        );
        let b = st.insert_ui("tabB", tap_b.clone(), None, UiKind::Agent, None);
        assert_eq!(st.revoke_tab("tabA").await, Some(Some("s1".into())));
        assert!(tap_a.is_closed(), "收回后旁路关闭，读循环不再 tee");
        assert!(st.get(&a).is_err(), "收回后 agent 无法再访问");
        assert!(!tap_b.is_closed());
        assert!(st.get(&b).is_ok());
        assert_eq!(st.revoke_tab("tabA").await, None, "重复收回无副作用");
        // MCP 停止：收回剩余 UI 标签并上报，供前端摘标记
        assert_eq!(st.close_all().await, vec!["tabB".to_string()]);
        assert!(tap_b.is_closed());
    }

    #[tokio::test]
    async fn read_waits_for_prompt_and_strips_ansi() {
        let st = McpTerminalState::new();
        let tap = Arc::new(crate::terminal::AgentTap::new());
        let id = st.insert_ui("tab", tap.clone(), None, UiKind::Agent, None);
        tap.out.lock().append(b"ls\r\n");
        let out = tap.out.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            out.lock().append(b"\x1b[32mfile\x1b[0m\r\nuser@h:~$ ");
        });
        let args = json!({ "terminalId": id, "offset": 0, "wait_seconds": 5, "wait_for": "\\$ $" });
        let v: Value = serde_json::from_str(&terminal_read(&st, &args).await.unwrap()).unwrap();
        assert_eq!(v["matched"], true, "{v}");
        assert_eq!(v["data"], "ls\nfile\nuser@h:~$ ");
        assert_eq!(v["more"], false);

        // 未匹配：等到期返回 matched=false，数据照常给出
        let next = v["nextOffset"].as_u64().unwrap();
        tap.out.lock().append(b"still running");
        let args =
            json!({ "terminalId": id, "offset": next, "wait_seconds": 1, "wait_for": "DONE" });
        let v: Value = serde_json::from_str(&terminal_read(&st, &args).await.unwrap()).unwrap();
        assert_eq!(v["matched"], false);
        assert_eq!(v["data"], "still running");

        let bad = json!({ "terminalId": id, "offset": 0, "wait_for": "(" });
        assert!(terminal_read(&st, &bad).await.is_err(), "非法正则应报错");
    }

    #[tokio::test]
    async fn read_idle_and_paging() {
        let st = McpTerminalState::new();
        let tap = Arc::new(crate::terminal::AgentTap::new());
        let id = st.insert_ui("tab", tap.clone(), None, UiKind::Agent, None);
        let out = tap.out.clone();
        tokio::spawn(async move {
            for i in 0..3 {
                out.lock().append(format!("line{i}\n").as_bytes());
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        let t0 = Instant::now();
        let args = json!({ "terminalId": id, "offset": 0, "wait_seconds": 10, "idle_ms": 1000 });
        let v: Value = serde_json::from_str(&terminal_read(&st, &args).await.unwrap()).unwrap();
        assert!(
            t0.elapsed() >= Duration::from_millis(1000),
            "静默期未到不应返回"
        );
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert_eq!(v["data"], "line0\nline1\nline2\n");

        let args = json!({ "terminalId": id, "offset": 0, "max_bytes": 6 });
        let v: Value = serde_json::from_str(&terminal_read(&st, &args).await.unwrap()).unwrap();
        assert_eq!(v["data"], "line0\n");
        assert_eq!(v["nextOffset"], 6);
        assert_eq!(v["more"], true);
    }
}
