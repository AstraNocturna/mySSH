//! MCP ↔ 前端交互枢纽：危险命令的应用内审批（弹窗 + pending）、agent 终端以 UI 标签打开。
//!
//! 前端启动时经 `mcp_ui_subscribe` 挂一条事件通道；ssh_exec 判定为需人工确认时
//! 推 `approval_request` 帧，用户在弹窗里批准/拒绝（`mcp_approve`）。agent 侧同步
//! 等待一段时间，未决则返回 pending——同会话同命令再次调用即续等同一审批项，
//! 无需额外的 confirm id 往返。审批项一次性消费，超过 TTL 视为拒绝。
//! 用户选「总是允许」时记下（会话, 完全相同的命令），之后同会话同命令免确认；
//! 规则只在内存，应用重启即失效。
//! 通道不存在（窗口未就绪/无前端，如测试）时审批不可用，调用方 fail-closed。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    decision: watch::Sender<Option<bool>>,
    deadline: Instant,
}

/// 前端开标签的结果：Ok(后端 tabId) / Err(连接失败原因)
type TabResult = Result<String, String>;

#[derive(Default)]
pub struct McpUiHub {
    sink: Mutex<Option<UiSink>>,
    approvals: Mutex<HashMap<String, Approval>>,
    /// 「总是允许」规则：(会话 id, 完全相同的命令)
    always: Mutex<HashSet<(String, String)>>,
    /// agent terminal_open 请前端开标签：requestId → 结果回传
    tab_waiters: Mutex<HashMap<String, oneshot::Sender<TabResult>>>,
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
        {
            let mut map = self.approvals.lock();
            map.retain(|_, a| a.deadline > now);
            if let Some((id, _)) = map
                .iter()
                .find(|(_, a)| a.session_id == session_id && a.command == command)
            {
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

    /// status: ok / error / timeout；error 为失败原因
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

    #[test]
    fn decide_unknown_is_false() {
        let (hub, _) = hub_with_sink();
        assert!(!hub.decide("nope", true));
    }
}
