//! MCP 后台命令（job_start / job_read / job_stop / job_list）：长时间运行的命令（构建、
//! 日志 tail、批量脚本）不占住一次 tools/call。启动即返回 jobId，输出进有界环形缓冲，
//! agent 按 offset 分段读取（默认剥离 ANSI），可随时停止。
//!
//! 命令门（分类器、审批弹窗、速率闸）与 ssh_exec 完全一致，在 mcp.rs 调用本模块之前执行。
//! 默认分配 PTY：关闭通道时远端进程组收到 SIGHUP，job_stop 才能可靠终止；
//! TERM=dumb 抑制大部分彩色/进度条输出。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde_json::{json, Value};

use core_store::{Actor, Store};

use crate::mcp_terminal::TermBuf;

/// 每会话同时运行的后台命令上限（Exec Transport 上 Monitor 2 + ssh_exec 4 + job 3
/// ≤ OpenSSH 默认 MaxSessions 10）
const PER_SESSION_RUNNING: usize = 3;
/// 注册表总量上限：超出时清理最早结束的任务
const MAX_JOBS: usize = 50;
const READ_DEFAULT_BYTES: usize = 16 * 1024;
const READ_MAX_BYTES: usize = 256 * 1024;
const READ_WAIT_MAX: u64 = 60;
const READ_POLL: Duration = Duration::from_millis(300);
const STOP_WAIT: Duration = Duration::from_secs(3);
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JobStatus {
    Running,
    Exited(Option<u32>),
    Stopped,
}

impl JobStatus {
    fn name(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited(_) => "exited",
            Self::Stopped => "stopped",
        }
    }
}

pub(crate) struct Job {
    session_id: String,
    command: String,
    started_at: u64,
    started: Instant,
    out: Mutex<TermBuf>,
    status: Mutex<(JobStatus, Option<Instant>)>,
    stop: tokio::sync::Notify,
}

impl Job {
    fn new(session_id: &str, command: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            command: command.to_string(),
            started_at: now_ms(),
            started: Instant::now(),
            out: Mutex::new(TermBuf::default()),
            status: Mutex::new((JobStatus::Running, None)),
            stop: tokio::sync::Notify::new(),
        }
    }

    fn status(&self) -> JobStatus {
        self.status.lock().0.clone()
    }

    fn running(&self) -> bool {
        self.status() == JobStatus::Running
    }

    /// 只从 Running 迁出一次（stop 与自然退出竞争时先到者生效）
    fn finish(&self, s: JobStatus) {
        let mut st = self.status.lock();
        if st.0 == JobStatus::Running {
            *st = (s, Some(Instant::now()));
        }
        self.out.lock().closed = true;
    }

    fn elapsed_ms(&self) -> u64 {
        let end = self.status.lock().1.unwrap_or_else(Instant::now);
        end.duration_since(self.started).as_millis() as u64
    }

    fn summary(&self, id: &str) -> Value {
        let status = self.status();
        let mut v = json!({
            "jobId": id,
            "sessionId": self.session_id,
            "command": self.command,
            "status": status.name(),
            "startedAt": self.started_at,
            "elapsedMs": self.elapsed_ms(),
            "totalBytes": self.out.lock().total,
        });
        if let JobStatus::Exited(code) = status {
            v["exitCode"] = json!(code);
        }
        v
    }
}

#[derive(Default)]
pub struct McpJobs {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl McpJobs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn session_of(&self, id: &str) -> Option<String> {
        self.jobs.lock().get(id).map(|j| j.session_id.clone())
    }

    fn get(&self, id: &str) -> Result<Arc<Job>, String> {
        self.jobs
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("后台命令 {id} 不存在（可能已被清理）"))
    }

    /// 占位登记（运行数检查与插入同锁，防并发超额）
    fn reserve(&self, job: Arc<Job>) -> Result<String, String> {
        let mut map = self.jobs.lock();
        let running = map
            .values()
            .filter(|j| j.session_id == job.session_id && j.running())
            .count();
        if running >= PER_SESSION_RUNNING {
            return Err(format!(
                "该会话已有 {running} 个后台命令在运行（上限 {PER_SESSION_RUNNING}），请先 job_stop 或等待结束"
            ));
        }
        if map.len() >= MAX_JOBS {
            let oldest = map
                .iter()
                .filter_map(|(id, j)| j.status.lock().1.map(|t| (t, id.clone())))
                .min();
            match oldest {
                Some((_, id)) => {
                    map.remove(&id);
                }
                None => return Err(format!("后台命令总数已达上限 {MAX_JOBS}")),
            }
        }
        let id = format!("{:016x}", rand::random::<u64>());
        map.insert(id.clone(), job);
        Ok(id)
    }

    /// MCP 停止：终止全部运行中的命令
    pub async fn stop_all(&self) {
        let all: Vec<Arc<Job>> = self.jobs.lock().drain().map(|(_, j)| j).collect();
        for j in all {
            if j.running() {
                j.stop.notify_one();
            }
        }
    }
}

/// 启动后台命令（命令门已在调用方通过）
pub(crate) async fn job_start(
    jobs: &Arc<McpJobs>,
    store: &Arc<Store>,
    ctx: Arc<crate::exec::ExecCtx>,
    session_id: &str,
    command: &str,
    pty: bool,
) -> Result<String, String> {
    let job = Arc::new(Job::new(session_id, command));
    let id = jobs.reserve(job.clone())?;
    let open = async {
        let ch = ctx
            .conn_handle()
            .open_session_channel()
            .await
            .map_err(|e| e.to_string())?;
        if pty {
            ch.request_pty(true, "dumb", 200, 50, 0, 0, &[])
                .await
                .map_err(|e| format!("PTY 请求失败: {e}"))?;
        }
        ch.exec(true, command)
            .await
            .map_err(|e| format!("exec 通道请求失败: {e}"))?;
        Ok::<_, String>(ch)
    };
    let ch = match tokio::time::timeout(OPEN_TIMEOUT, open).await {
        Ok(Ok(ch)) => ch,
        Ok(Err(e)) => {
            jobs.jobs.lock().remove(&id);
            return Err(e);
        }
        Err(_) => {
            jobs.jobs.lock().remove(&id);
            return Err("启动超时".into());
        }
    };
    tokio::spawn(run_job(job.clone(), ch, ctx, store.clone(), id.clone()));
    Ok(json!({
        "jobId": id,
        "status": "running",
        "hint": "用 job_read 分段读取输出（offset 从 0 开始，下次用返回的 nextOffset），job_stop 停止",
    })
    .to_string())
}

async fn run_job(
    job: Arc<Job>,
    mut ch: russh::Channel<russh::client::Msg>,
    ctx: Arc<crate::exec::ExecCtx>,
    store: Arc<Store>,
    id: String,
) {
    let mut exit: Option<u32> = None;
    let status = loop {
        tokio::select! {
            msg = ch.wait() => match msg {
                Some(russh::ChannelMsg::Data { data }) => job.out.lock().append(&data),
                Some(russh::ChannelMsg::ExtendedData { data, .. }) => job.out.lock().append(&data),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                Some(russh::ChannelMsg::ExitSignal { signal_name, .. }) => {
                    job.out
                        .lock()
                        .append(format!("\n[进程被信号终止: {signal_name:?}]\n").as_bytes());
                }
                Some(russh::ChannelMsg::Close) | None => break JobStatus::Exited(exit),
                // Eof 之后仍可能到 ExitStatus：等 Close
                Some(_) => {}
            },
            _ = job.stop.notified() => {
                let _ = ch.signal(russh::Sig::TERM).await;
                let _ = ch.eof().await;
                let _ = ch.close().await;
                break JobStatus::Stopped;
            }
        }
    };
    drop(ch);
    drop(ctx);
    job.finish(status);
    let summary = job.summary(&id);
    let _ = store
        .audit()
        .append(Actor::Mcp, Some(&job.session_id), "mcp_job_end", &summary)
        .await;
}

/// job_read：offset 游标分段读取；wait_seconds>0 时无新输出则服务端等待
pub(crate) async fn job_read(jobs: &Arc<McpJobs>, args: &Value) -> Result<String, String> {
    let id = req_job_id(args, "job_read")?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let max = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .map_or(READ_DEFAULT_BYTES, |v| v as usize)
        .clamp(1, READ_MAX_BYTES);
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(READ_WAIT_MAX);
    let strip = args
        .get("strip_ansi")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let job = jobs.get(id)?;
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        let ready = {
            let b = job.out.lock();
            offset < b.total || b.closed
        };
        if ready || Instant::now() >= deadline {
            let page = crate::mcp_terminal::read_page(&job.out, offset, max, strip);
            let mut v = job.summary(id);
            v["data"] = json!(page.data);
            v["nextOffset"] = json!(page.next);
            v["more"] = json!(page.more);
            v["truncated"] = json!(page.truncated);
            return Ok(v.to_string());
        }
        tokio::time::sleep(READ_POLL).await;
    }
}
pub(crate) async fn job_stop(jobs: &Arc<McpJobs>, args: &Value) -> Result<String, String> {
    let id = req_job_id(args, "job_stop")?;
    let job = jobs.get(id)?;
    if job.running() {
        job.stop.notify_one();
        let deadline = Instant::now() + STOP_WAIT;
        while job.running() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(job.summary(id).to_string())
}

impl McpJobs {
    /// 运行中的后台命令（旧→新），供 AI 活动面板
    pub(crate) fn running_snapshot(&self) -> Vec<Value> {
        let map = self.jobs.lock();
        let mut items: Vec<(u64, Value)> = map
            .iter()
            .filter(|(_, j)| j.running())
            .map(|(id, j)| (j.started_at, j.summary(id)))
            .collect();
        items.sort_by_key(|(t, _)| *t);
        items.into_iter().map(|(_, v)| v).collect()
    }
}

pub(crate) fn job_list(jobs: &Arc<McpJobs>, args: &Value) -> String {
    let sid = args.get("session_id").and_then(Value::as_str);
    let map = jobs.jobs.lock();
    let mut items: Vec<(u64, Value)> = map
        .iter()
        .filter(|(_, j)| sid.is_none_or(|s| s == j.session_id))
        .map(|(id, j)| (j.started_at, j.summary(id)))
        .collect();
    items.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    json!({ "jobs": items.into_iter().map(|(_, v)| v).collect::<Vec<_>>() }).to_string()
}

fn req_job_id<'a>(args: &'a Value, tool: &str) -> Result<&'a str, String> {
    args.get("job_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 job_id"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn insert(jobs: &McpJobs, sid: &str) -> (String, Arc<Job>) {
        let j = Arc::new(Job::new(sid, "sleep 1"));
        (jobs.reserve(j.clone()).unwrap(), j)
    }

    #[test]
    fn per_session_running_cap() {
        let jobs = McpJobs::default();
        let mut held = Vec::new();
        for _ in 0..PER_SESSION_RUNNING {
            held.push(insert(&jobs, "s1"));
        }
        let extra = Arc::new(Job::new("s1", "x"));
        assert!(jobs.reserve(extra).is_err(), "超过每会话上限");
        insert(&jobs, "s2"); // 其他会话不受影响
        held[0].1.finish(JobStatus::Exited(Some(0)));
        insert(&jobs, "s1"); // 结束一个后可再启动
    }

    #[test]
    fn finish_is_first_wins() {
        let j = Job::new("s", "c");
        j.finish(JobStatus::Stopped);
        j.finish(JobStatus::Exited(Some(0)));
        assert_eq!(j.status(), JobStatus::Stopped);
        assert!(j.out.lock().closed);
    }

    #[test]
    fn prunes_oldest_finished_when_full() {
        let jobs = McpJobs::default();
        let mut first = None;
        for i in 0..MAX_JOBS {
            let (id, j) = insert(&jobs, &format!("s{i}"));
            j.finish(JobStatus::Exited(Some(0)));
            first.get_or_insert(id);
            std::thread::sleep(Duration::from_millis(1));
        }
        insert(&jobs, "new");
        assert_eq!(jobs.jobs.lock().len(), MAX_JOBS);
        assert!(jobs.get(first.as_deref().unwrap()).is_err());
    }

    #[tokio::test]
    async fn read_pages_and_strips_ansi() {
        let jobs = McpJobs::new();
        let (id, j) = insert(&jobs, "s");
        j.out.lock().append(b"\x1b[32mok\x1b[0m line1\r\nline2\r\n");
        let v: Value = serde_json::from_str(
            &job_read(&jobs, &json!({ "job_id": id, "max_bytes": 12 }))
                .await
                .unwrap(),
        )
        .unwrap();
        // 前 12 字节含完整颜色序列与 "ok"，剥离后不残留转义
        assert_eq!(v["status"], "running");
        assert!(!v["data"].as_str().unwrap().contains('\x1b'));
        assert_eq!(v["more"], true);
        let mut off = v["nextOffset"].as_u64().unwrap();
        let mut text = v["data"].as_str().unwrap().to_string();
        j.finish(JobStatus::Exited(Some(3)));
        loop {
            let v: Value = serde_json::from_str(
                &job_read(
                    &jobs,
                    &json!({ "job_id": id, "offset": off, "max_bytes": 12 }),
                )
                .await
                .unwrap(),
            )
            .unwrap();
            text.push_str(v["data"].as_str().unwrap());
            off = v["nextOffset"].as_u64().unwrap();
            if v["more"] == false {
                assert_eq!(v["exitCode"], 3);
                break;
            }
        }
        assert_eq!(text, "ok line1\nline2\n");
    }

    #[tokio::test(start_paused = true)]
    async fn read_waits_for_output() {
        let jobs = McpJobs::new();
        let (id, j) = insert(&jobs, "s");
        let j2 = j.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            j2.out.lock().append(b"late\n");
        });
        let v: Value = serde_json::from_str(
            &job_read(&jobs, &json!({ "job_id": id, "wait_seconds": 10 }))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["data"], "late\n");
    }
}
