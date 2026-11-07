//! MCP ↔ 前端交互枢纽：危险命令的应用内审批（弹窗 + pending）。
//!
//! 前端启动时经 `mcp_ui_subscribe` 挂一条事件通道；ssh_exec 判定为需人工确认时
//! 推 `approval_request` 帧，用户在弹窗里批准/拒绝（`mcp_approve`）。agent 侧同步
//! 等待一段时间，未决则返回 pending——同会话同命令再次调用即续等同一审批项，
//! 无需额外的 confirm id 往返。审批项一次性消费，超过 TTL 视为拒绝。
//! 通道不存在（窗口未就绪/无前端，如测试）时审批不可用，调用方 fail-closed。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

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

#[derive(Default)]
pub struct McpUiHub {
    sink: Mutex<Option<UiSink>>,
    approvals: Mutex<HashMap<String, Approval>>,
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
        let bytes: [u8; 8] = rand::random();
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
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

/// 前端订阅 MCP 交互事件（审批请求等）；窗口重载后再次调用替换旧通道
#[tauri::command]
pub async fn mcp_ui_subscribe(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    events: tauri::ipc::Channel<Value>,
) -> Result<(), String> {
    hub.set_sink(Arc::new(move |v: &Value| events.send(v.clone()).is_ok()));
    Ok(())
}

/// 用户对审批项的决定
#[tauri::command]
pub async fn mcp_approve(
    hub: tauri::State<'_, Arc<McpUiHub>>,
    id: String,
    allow: bool,
) -> Result<bool, String> {
    Ok(hub.decide(&id, allow))
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

    #[test]
    fn decide_unknown_is_false() {
        let (hub, _) = hub_with_sink();
        assert!(!hub.decide("nope", true));
    }
}
