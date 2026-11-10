//! MCP ↔ 前端交互枢纽：危险命令的应用内审批（弹窗 + pending）、agent 终端以 UI 标签打开。
//!
//! 前端启动时经 `mcp_ui_subscribe` 挂一条事件通道；ssh_exec 判定为需人工确认时
//! 推 `approval_request` 帧，用户在弹窗里批准/拒绝（`mcp_approve`）。agent 侧同步
//! 等待一段时间，未决则返回 pending——同会话同命令再次调用即续等同一审批项，
//! 无需额外的 confirm id 往返。审批项一次性消费，超过 TTL 视为拒绝。
//! 用户选「总是允许」时记下（会话, 完全相同的命令），之后同会话同命令免确认；
//! 规则只在内存，应用重启即失效。
//! 通道不存在（窗口未就绪/无前端，如测试）时审批不可用，调用方 fail-closed。
//!
//! AI 活动登记：进行中的 ssh_exec / recipe_run 在此登记（不依赖镜像开关），活动有变化时推
//! `ai_activity` 提示帧，前端再经 `mcp_activity` 取快照（含后台命令与 AI 终端），
//! 用户可在活动面板经 `mcp_activity_stop` 停止。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};

/// 审批项存活上限：超时未决即拒绝，前端弹窗按同一时长自动关闭
pub(crate) const APPROVAL_TTL: Duration = Duration::from_secs(300);

/// 事件出口；返回 false 表示通道已失效
pub(crate) type UiSink = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApprovalWait {
    Approved,
    Denied,
    /// 等待窗口内用户未决；携带审批项 id
    Pending(String),
}

struct Approval {
    session_id: String,
    command: String,
    /// 发起请求的 MCP 客户端（客户端断开时据此拒绝并关弹窗）
    owner: Option<String>,
    decision: watch::Sender<Option<bool>>,
    deadline: Instant,
}

/// 前端开标签的结果：Ok(后端 tabId) / Err(连接失败原因)
type TabResult = Result<String, String>;

/// 一条进行中的 AI 一次性命令（ssh_exec / recipe_run）
struct ExecActivity {
    session_id: String,
    tool: String,
    command: String,
    /// unix 毫秒
    started_at: u64,
    cancel: Arc<tokio::sync::Notify>,
    owner: Option<String>,
}

#[derive(Default)]
pub struct McpUiHub {
    sink: Mutex<Option<UiSink>>,
    approvals: Mutex<HashMap<String, Approval>>,
    /// 「总是允许」规则：(会话 id, 完全相同的命令)
    always: Mutex<HashSet<(String, String)>>,
    /// agent terminal_open 请前端开标签：requestId → 结果回传
    tab_waiters: Mutex<HashMap<String, oneshot::Sender<TabResult>>>,
    /// 进行中的 AI 一次性命令：execId → 登记
    execs: Mutex<HashMap<String, ExecActivity>>,
}

/// 一次 AI 命令的活动登记凭据；drop 即注销（正常结束、超时、取消、任务被中止都覆盖）
pub(crate) struct ExecTicket {
    hub: Arc<McpUiHub>,
    id: String,
    cancel: Arc<tokio::sync::Notify>,
}

impl ExecTicket {
    /// 用户在活动面板停止该命令时返回
    pub(crate) async fn cancelled(&self) {
        self.cancel.notified().await;
    }
}

impl Drop for ExecTicket {
    fn drop(&mut self) {
        self.hub.execs.lock().remove(&self.id);
        self.hub.activity_changed();
    }
}

fn random_id() -> String {
    let bytes: [u8; 8] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl McpUiHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 前端（重新）订阅：新通道替换旧通道（窗口重载后旧通道已失效）
    pub(crate) fn set_sink(&self, sink: UiSink) {
        *self.sink.lock() = Some(sink);
    }

    pub(crate) fn has_ui(&self) -> bool {
        self.sink.lock().is_some()
    }

    /// 推一帧到前端；通道失效则摘除（仅当仍是同一通道，避免误删新订阅）
    pub(crate) fn emit(&self, v: &Value) -> bool {
        let Some(sink) = self.sink.lock().clone() else {
            return false;
        };
        if sink(v) {
            return true;
        }
        let mut cur = self.sink.lock();
        if cur.as_ref().is_some_and(|s| Arc::ptr_eq(s, &sink)) {
            *cur = None;
        }
        false
    }

    /// 发起（或复用）审批项。同会话同命令已有未过期项时复用其 id——agent 收到
    /// pending 后用同样参数重试即续等；`event` 为弹窗展示用的附加字段。
    pub(crate) fn request(
        &self,
        session_id: &str,
        command: &str,
        event: Value,
    ) -> Result<String, String> {
        let now = Instant::now();
        let owner = crate::mcp_clients::current();
        {
            let mut map = self.approvals.lock();
            map.retain(|_, a| a.deadline > now);
            // 只复用同一客户端的：别的客户端断开时不连带拒绝
            if let Some((id, _)) = map.iter().find(|(_, a)| {
                a.session_id == session_id && a.command == command && a.owner == owner
            }) {
                return Ok(id.clone());
            }
        }
        if !self.has_ui() {
            return Err(
                "需人工确认，但 mySSH 窗口审批通道不可用（请保持 mySSH 主窗口打开）".into(),
            );
        }
        let id = random_id();
        let (tx, _rx) = watch::channel(None);
        self.approvals.lock().insert(
            id.clone(),
            Approval {
                session_id: session_id.to_string(),
                command: command.to_string(),
                owner,
                decision: tx,
                deadline: now + APPROVAL_TTL,
            },
        );
        let mut frame = json!({
            "type": "approval_request",
            "id": id,
            "sessionId": session_id,
            "command": command,
            "expiresInMs": APPROVAL_TTL.as_millis() as u64,
        });
        if let (Some(f), Value::Object(extra)) = (frame.as_object_mut(), event) {
            for (k, v) in extra {
                f.entry(k).or_insert(v);
            }
        }
        if !self.emit(&frame) {
            self.approvals.lock().remove(&id);
            return Err(
                "需人工确认，但 mySSH 窗口审批通道不可用（请保持 mySSH 主窗口打开）".into(),
            );
        }
        Ok(id)
    }

    /// 等用户决定，最长 `max`（且不超过审批项 TTL）。决定后审批项即被消费。
    pub(crate) async fn wait(&self, id: &str, max: Duration) -> ApprovalWait {
        let (mut rx, deadline) = {
            let map = self.approvals.lock();
            match map.get(id) {
                Some(a) => (a.decision.subscribe(), a.deadline),
                None => return ApprovalWait::Denied,
            }
        };
        let until = (Instant::now() + max).min(deadline);
        loop {
            let decided = *rx.borrow_and_update();
            if let Some(allow) = decided {
                self.approvals.lock().remove(id);
                return if allow {
                    ApprovalWait::Approved
                } else {
                    ApprovalWait::Denied
                };
            }
            match tokio::time::timeout_at(until.into(), rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return ApprovalWait::Denied,
                Err(_) => break,
            }
        }
        if Instant::now() >= deadline {
            self.approvals.lock().remove(id);
            return ApprovalWait::Denied;
        }
        ApprovalWait::Pending(id.to_string())
    }

    /// 请前端为 agent 开一个普通终端标签；连上后前端经 mcp_agent_tab_ready 回传 tabId
    pub(crate) fn request_tab(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> Result<(String, oneshot::Receiver<TabResult>), String> {
        let id = random_id();
        let (tx, rx) = oneshot::channel();
        self.tab_waiters.lock().insert(id.clone(), tx);
        let sent = self.emit(&json!({
            "type": "agent_terminal_open",
            "requestId": id,
            "sessionId": session_id,
            "sessionName": session_name,
        }));
        if !sent {
            self.tab_waiters.lock().remove(&id);
            return Err("mySSH 窗口不可用，无法以标签打开终端".into());
        }
        Ok((id, rx))
    }

    pub(crate) fn cancel_tab(&self, id: &str) {
        self.tab_waiters.lock().remove(id);
    }

    fn resolve_tab(&self, id: &str, result: TabResult) -> bool {
        match self.tab_waiters.lock().remove(id) {
            Some(tx) => tx.send(result).is_ok(),
            None => false,
        }
    }

    /// 提示前端 AI 活动有变化（前端随后经 mcp_activity 取快照）；窗口不在线时无操作
    pub(crate) fn activity_changed(&self) {
        self.emit(&json!({ "type": "ai_activity" }));
    }

    pub(crate) fn track_exec(
        self: &Arc<Self>,
        session_id: &str,
        tool: &str,
        command: &str,
    ) -> ExecTicket {
        let id = random_id();
        let cancel = Arc::new(tokio::sync::Notify::new());
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.execs.lock().insert(
            id.clone(),
            ExecActivity {
                session_id: session_id.to_string(),
                tool: tool.to_string(),
                command: command.to_string(),
                started_at,
                cancel: cancel.clone(),
                owner: crate::mcp_clients::current(),
            },
        );
        self.activity_changed();
        ExecTicket {
            hub: self.clone(),
            id,
            cancel,
        }
    }

    /// 用户停止一条进行中的命令；返回 (是否找到, 所属会话)
    pub(crate) fn cancel_exec(&self, id: &str) -> Option<String> {
        let map = self.execs.lock();
        let a = map.get(id)?;
        // notify_one 在无人等待时也会留一个许可：select 稍后进入等待也能立刻收到
        a.cancel.notify_one();
        Some(a.session_id.clone())
    }

    /// 某客户端进行中的命令数
    pub(crate) fn owned_execs(&self, owner: &str) -> usize {
        self.execs
            .lock()
            .values()
            .filter(|a| a.owner.as_deref() == Some(owner))
            .count()
    }

    /// 客户端断开：停止它进行中的命令；返回停止条数
    pub(crate) fn cancel_owned_execs(&self, owner: &str) -> usize {
        let map = self.execs.lock();
        let mine: Vec<_> = map
            .values()
            .filter(|a| a.owner.as_deref() == Some(owner))
            .collect();
        for a in &mine {
            a.cancel.notify_one();
        }
        mine.len()
    }

    /// 某客户端未过期的待确认项数
    pub(crate) fn owned_approvals(&self, owner: &str) -> usize {
        let now = Instant::now();
        self.approvals
            .lock()
            .values()
            .filter(|a| a.deadline > now && a.owner.as_deref() == Some(owner))
            .count()
    }

    /// 客户端断开：拒绝并移除它的待确认项，通知前端关掉对应弹窗；返回条数
    pub(crate) fn drop_owned_approvals(&self, owner: &str) -> usize {
        let removed: Vec<String> = {
            let mut map = self.approvals.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, a)| a.owner.as_deref() == Some(owner))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                if let Some(a) = map.remove(id) {
                    a.decision.send_replace(Some(false));
                }
            }
            ids
        };
        for id in &removed {
            self.emit(&json!({ "type": "approval_closed", "id": id }));
        }
        removed.len()
    }

    /// 进行中的 AI 命令（旧→新）
    pub(crate) fn exec_snapshot(&self) -> Vec<Value> {
        let map = self.execs.lock();
        let mut items: Vec<(u64, Value)> = map
            .iter()
            .map(|(id, a)| {
                (
                    a.started_at,
                    json!({
                        "execId": id,
                        "sessionId": a.session_id,
                        "tool": a.tool,
                        "command": a.command,
                        "startedAt": a.started_at,
                    }),
                )
            })
            .collect();
        items.sort_by_key(|(t, _)| *t);
        items.into_iter().map(|(_, v)| v).collect()
    }

    /// 批准并记下「总是允许」规则；未知/已过期审批项返回 false 且不记规则
    pub(crate) fn allow_always(&self, id: &str) -> bool {
        let key = {
            let map = self.approvals.lock();
            match map.get(id) {
                Some(a) if a.deadline > Instant::now() => (a.session_id.clone(), a.command.clone()),
                _ => return false,
            }
        };
        self.always.lock().insert(key);
        self.decide(id, true)
    }

    /// 同会话同命令是否已被用户「总是允许」
    pub(crate) fn is_always_allowed(&self, session_id: &str, command: &str) -> bool {
        self.always
            .lock()
            .contains(&(session_id.to_string(), command.to_string()))
    }

    /// 用户决定；未知/已过期审批项返回 false
    pub(crate) fn decide(&self, id: &str, allow: bool) -> bool {
        let map = self.approvals.lock();
        match map.get(id) {
            Some(a) if a.deadline > Instant::now() => {
                a.decision.send_replace(Some(allow));
                true
            }
            _ => false,
        }
    }
}

/// 单次命令镜像推送上限：超出后只计数不再推送（镜像只为旁观，完整结果仍回给 agent）
const MIRROR_CAP: usize = 512 * 1024;

#[derive(Default)]
struct MirrorBuf {
    out: Vec<u8>,
    err: Vec<u8>,
    sent: usize,
    omitted: usize,
}

/// ssh_exec / recipe_run 的只读镜像：开始、流式输出、结束三类帧推给前端，
/// 前端按会话汇入「AI · 主机」只读标签。窗口不在线时不创建。
pub(crate) struct ExecMirror {
    hub: Arc<McpUiHub>,
    id: String,
    session_id: String,
    buf: Mutex<MirrorBuf>,
}

impl ExecMirror {
    pub(crate) fn begin(
        hub: &Arc<McpUiHub>,
        session_id: &str,
        session_name: &str,
        tool: &str,
        command: &str,
    ) -> Option<Self> {
        let id = random_id();
        let sent = hub.emit(&json!({
            "type": "exec_start",
            "execId": id,
            "sessionId": session_id,
            "sessionName": session_name,
            "tool": tool,
            "command": command,
        }));
        sent.then(|| Self {
            hub: hub.clone(),
            id,
            session_id: session_id.to_string(),
            buf: Mutex::new(MirrorBuf::default()),
        })
    }

    pub(crate) fn push(&self, stderr: bool, data: &[u8]) {
        let text = {
            let mut b = self.buf.lock();
            if b.sent >= MIRROR_CAP {
                b.omitted += data.len();
                return;
            }
            let room = MIRROR_CAP - b.sent;
            let take = data.len().min(room);
            b.sent += take;
            b.omitted += data.len() - take;
            let carry = if stderr { &mut b.err } else { &mut b.out };
            carry.extend_from_slice(&data[..take]);
            crate::recording::take_utf8(carry, false)
        };
        self.emit_output(stderr, text);
    }

    fn emit_output(&self, stderr: bool, data: String) {
        if data.is_empty() {
            return;
        }
        self.hub.emit(&json!({
            "type": "exec_output",
            "execId": self.id,
            "sessionId": self.session_id,
            "stream": if stderr { "stderr" } else { "stdout" },
            "data": data,
        }));
    }

    /// status: ok / error / timeout / cancelled；error 为失败原因
    pub(crate) fn finish(
        self,
        status: &str,
        exit_code: Option<u64>,
        duration_ms: u64,
        error: Option<&str>,
    ) {
        let (out, err, omitted) = {
            let mut b = self.buf.lock();
            let out = crate::recording::take_utf8(&mut b.out, true);
            let err = crate::recording::take_utf8(&mut b.err, true);
            (out, err, b.omitted)
        };
        self.emit_output(false, out);
        self.emit_output(true, err);
        self.hub.emit(&json!({
            "type": "exec_end",
            "execId": self.id,
            "sessionId": self.session_id,
            "status": status,
            "exitCode": exit_code,
            "durationMs": duration_ms,
            "error": error,
            "omittedBytes": omitted,
        }));
    }
}

/// 前端订阅 MCP 交互事件（审批请求等）；窗口重载后再次调用替换旧通道
#[tauri::command]
pub async fn mcp_ui_subscribe(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    events: tauri::ipc::Channel<Value>,
) -> Result<(), String> {
    hub.set_sink(Arc::new(move |v: &Value| events.send(v.clone()).is_ok()));
    Ok(())
}

/// 用户对审批项的决定；always = 批准且本会话此命令以后免确认（至应用重启）
#[tauri::command]
pub async fn mcp_approve(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    id: String,
    allow: bool,
    always: Option<bool>,
) -> Result<bool, String> {
    if allow && always == Some(true) {
        return Ok(hub.allow_always(&id));
    }
    Ok(hub.decide(&id, allow))
}

/// AI 活动快照：进行中的一次性命令、运行中的后台命令、未关闭的 AI 终端
#[tauri::command]
pub async fn mcp_activity(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    jobs: tauri::State<'_, Arc<crate::mcp_jobs::McpJobs>>,
    terms: tauri::State<'_, Arc<crate::mcp_terminal::McpTerminalState>>,
) -> Result<Value, String> {
    Ok(json!({
        "execs": hub.exec_snapshot(),
        "jobs": jobs.running_snapshot(),
        "terminals": terms.live_snapshot(),
    }))
}

/// 用户在活动面板停止一项 AI 活动：kind = exec / job / terminal
#[tauri::command]
pub async fn mcp_activity_stop(
    kind: String,
    id: String,
    hub: tauri::State<'_, Arc<McpUiHub>>,
    jobs: tauri::State<'_, Arc<crate::mcp_jobs::McpJobs>>,
    terms: tauri::State<'_, Arc<crate::mcp_terminal::McpTerminalState>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<(), String> {
    let store = &sessions.store;
    let gone = || format!("该项已结束：{id}");
    match kind.as_str() {
        "exec" => {
            let sid = hub.cancel_exec(&id).ok_or_else(gone)?;
            let _ = store
                .audit()
                .append(
                    core_store::Actor::Gui,
                    Some(&sid),
                    "mcp_exec_cancel",
                    &json!({ "execId": id }),
                )
                .await;
        }
        "job" => {
            let sid = jobs.session_of(&id).ok_or_else(gone)?;
            let body = crate::mcp_jobs::job_stop(&jobs, &json!({ "job_id": id })).await?;
            let detail: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let _ = store
                .audit()
                .append(
                    core_store::Actor::Gui,
                    Some(&sid),
                    "mcp_job_stop",
                    &json!({ "detail": detail }),
                )
                .await;
        }
        "terminal" => {
            crate::mcp_terminal::close_terminal(&terms, &hub, store, &id, core_store::Actor::Gui)
                .await?;
        }
        other => return Err(format!("未知活动类型：{other}")),
    }
    hub.activity_changed();
    Ok(())
}

/// 前端为 agent 开的标签已连上（tabId）或失败（error）；返回 false = 请求已超时/取消
#[tauri::command]
pub async fn mcp_agent_tab_ready(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    request_id: String,
    tab_id: Option<String>,
    error: Option<String>,
) -> Result<bool, String> {
    let result = match tab_id {
        Some(t) => Ok(t),
        None => Err(error.unwrap_or_else(|| "未知错误".into())),
    };
    Ok(hub.resolve_tab(&request_id, result))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn hub_with_sink() -> (Arc<McpUiHub>, Arc<Mutex<Vec<Value>>>) {
        let hub = McpUiHub::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        hub.set_sink(Arc::new(move |v: &Value| {
            s.lock().push(v.clone());
            true
        }));
        (hub, seen)
    }

    #[test]
    fn no_ui_fails_closed() {
        let hub = McpUiHub::new();
        assert!(hub.request("s", "rm -rf /x", json!({})).is_err());
    }

    #[test]
    fn dead_sink_is_dropped_and_request_fails() {
        let hub = McpUiHub::new();
        hub.set_sink(Arc::new(|_: &Value| false));
        assert!(hub.request("s", "rm x", json!({})).is_err());
        assert!(!hub.has_ui());
    }

    #[tokio::test]
    async fn approve_then_consumed_once() {
        let (hub, seen) = hub_with_sink();
        let id = hub
            .request(
                "s",
                "systemctl restart nginx",
                json!({ "production": true }),
            )
            .unwrap();
        let frame = seen.lock()[0].clone();
        assert_eq!(frame["type"], "approval_request");
        assert_eq!(frame["production"], true);
        assert_eq!(frame["command"], "systemctl restart nginx");
        let h = hub.clone();
        let id2 = id.clone();
        let waiter = tokio::spawn(async move { h.wait(&id2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(hub.decide(&id, true));
        assert_eq!(waiter.await.unwrap(), ApprovalWait::Approved);
        // 已消费：再次 wait 视为拒绝，再次 request 产生新审批项（重新弹窗）
        assert_eq!(
            hub.wait(&id, Duration::from_millis(10)).await,
            ApprovalWait::Denied
        );
        let again = hub
            .request("s", "systemctl restart nginx", json!({}))
            .unwrap();
        assert_ne!(again, id);
        assert_eq!(seen.lock().len(), 2);
    }

    #[tokio::test]
    async fn undecided_returns_pending_and_retry_reuses_item() {
        let (hub, seen) = hub_with_sink();
        let id = hub.request("s", "rm -rf /tmp/x", json!({})).unwrap();
        assert_eq!(
            hub.wait(&id, Duration::from_millis(20)).await,
            ApprovalWait::Pending(id.clone())
        );
        // 同参数重试复用同一项，不重复弹窗
        assert_eq!(hub.request("s", "rm -rf /tmp/x", json!({})).unwrap(), id);
        assert_eq!(seen.lock().len(), 1);
        // 不同会话同命令是独立审批项
        assert_ne!(
            hub.request("other", "rm -rf /tmp/x", json!({})).unwrap(),
            id
        );
        assert!(hub.decide(&id, false));
        assert_eq!(
            hub.wait(&id, Duration::from_secs(1)).await,
            ApprovalWait::Denied
        );
    }

    #[tokio::test]
    async fn tab_request_roundtrip() {
        let (hub, seen) = hub_with_sink();
        let (id, rx) = hub.request_tab("s", "web").unwrap();
        let frame = seen.lock()[0].clone();
        assert_eq!(frame["type"], "agent_terminal_open");
        assert_eq!(frame["requestId"], id.as_str());
        assert!(hub.resolve_tab(&id, Ok("t9".into())));
        assert_eq!(rx.await.unwrap(), Ok("t9".to_string()));
        // 已消费/取消的请求不能再回传
        assert!(!hub.resolve_tab(&id, Ok("t10".into())));
        let (id2, _rx2) = hub.request_tab("s", "web").unwrap();
        hub.cancel_tab(&id2);
        assert!(!hub.resolve_tab(&id2, Err("x".into())));
        assert!(
            McpUiHub::new().request_tab("s", "web").is_err(),
            "无前端订阅不能开标签"
        );
    }

    #[test]
    fn exec_mirror_streams_and_caps() {
        assert!(
            ExecMirror::begin(&McpUiHub::new(), "s", "web", "ssh_exec", "ls").is_none(),
            "无窗口不镜像"
        );
        let (hub, seen) = hub_with_sink();
        let m = ExecMirror::begin(&hub, "s", "web", "ssh_exec", "ls").unwrap();
        // "中" = E4 B8 AD：拆在两块之间不能出乱码
        m.push(false, b"a\xe4\xb8");
        m.push(false, b"\xad\n");
        m.push(true, b"oops");
        m.push(false, &vec![b'x'; MIRROR_CAP]);
        m.push(false, b"tail");
        m.finish("ok", Some(0), 12, None);
        let frames = seen.lock().clone();
        assert_eq!(frames[0]["type"], "exec_start");
        assert_eq!(frames[0]["command"], "ls");
        let out: String = frames
            .iter()
            .filter(|f| f["type"] == "exec_output" && f["stream"] == "stdout")
            .map(|f| f["data"].as_str().unwrap().to_string())
            .collect();
        assert!(out.starts_with("a中\n"));
        assert!(!out.contains('\u{FFFD}'));
        assert!(frames
            .iter()
            .any(|f| f["stream"] == "stderr" && f["data"] == "oops"));
        let end = frames.last().unwrap();
        assert_eq!(end["type"], "exec_end");
        assert_eq!(end["exitCode"], 0);
        // 先收 3+2+4=9 字节，x 块只收余量 MIRROR_CAP-9，其余 9 字节与 "tail" 4 字节省略
        assert_eq!(end["omittedBytes"], 13);
    }

    #[tokio::test]
    async fn allow_always_records_exact_command_per_session() {
        let (hub, _) = hub_with_sink();
        let id = hub
            .request("s", "systemctl restart nginx", json!({}))
            .unwrap();
        assert!(!hub.is_always_allowed("s", "systemctl restart nginx"));
        assert!(hub.allow_always(&id));
        assert_eq!(
            hub.wait(&id, Duration::from_millis(10)).await,
            ApprovalWait::Approved
        );
        assert!(hub.is_always_allowed("s", "systemctl restart nginx"));
        // 只认同会话 + 完全相同的命令
        assert!(!hub.is_always_allowed("other", "systemctl restart nginx"));
        assert!(!hub.is_always_allowed("s", "systemctl restart nginx "));
        assert!(!hub.is_always_allowed("s", "systemctl restart mysql"));
        // 已消费/未知审批项不能借机记规则
        assert!(!hub.allow_always(&id));
        assert!(!hub.allow_always("nope"));
    }

    #[tokio::test]
    async fn exec_ticket_registers_cancels_and_unregisters_on_drop() {
        let (hub, seen) = hub_with_sink();
        let t = hub.track_exec("s1", "ssh_exec", "sleep 100");
        let snap = hub.exec_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["sessionId"], "s1");
        assert_eq!(snap[0]["command"], "sleep 100");
        let id = snap[0]["execId"].as_str().unwrap().to_string();
        // 先取消、后等待也能收到（命令可能还在排队拿配额）
        assert_eq!(hub.cancel_exec(&id).as_deref(), Some("s1"));
        tokio::time::timeout(Duration::from_secs(1), t.cancelled())
            .await
            .expect("取消信号应立刻送达");
        drop(t);
        assert!(hub.exec_snapshot().is_empty(), "drop 即注销");
        assert!(hub.cancel_exec(&id).is_none(), "已结束的命令不能再停止");
        let hints = seen
            .lock()
            .iter()
            .filter(|f| f["type"] == "ai_activity")
            .count();
        assert_eq!(hints, 2, "登记与注销各推一次提示帧");
    }

    #[test]
    fn decide_unknown_is_false() {
        let (hub, _) = hub_with_sink();
        assert!(!hub.decide("nope", true));
    }
}
