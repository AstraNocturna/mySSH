//! 终端会话录像：asciicast v2（asciinema 兼容）落 %LOCALAPPDATA%/myssh/recordings。
//!
//! 读循环把解码后的输出 tee 给 Recorder（与 AI 旁路同一 tee 点，不经信用闸）；写盘在专用
//! 线程，读循环只做一次拷贝 + channel send。<id>.cast 为录像本体，<id>.json 为元数据，
//! 结束时写入文件 sha256，并把摘要落审计：元数据文件可被随手改掉，审计哈希链里的摘要才是
//! 校验录像未被篡改的锚点。只录输出、resize 与 AI 写入的输入；用户键盘输入不录（可能含密码）。

use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 单个录像文件上限：超出后停止写入并标 truncated
const REC_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// 播放器整读上限（再大走导出文本）
const LOAD_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// 保留条数（新录像开始时删最旧的已结束录像）
const MAX_RECORDINGS: usize = 500;
/// 审计动作名：录像结束时落 sha256
pub(crate) const AUDIT_ACTION: &str = "recording_finished";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecSource {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingMeta {
    pub id: String,
    pub title: String,
    pub source: RecSource,
    pub session_id: Option<String>,
    pub cols: u32,
    pub rows: u32,
    /// unix 毫秒
    pub started_at: u64,
    /// None = 录制中（或应用异常退出未收尾）
    pub ended_at: Option<u64>,
    pub duration_ms: u64,
    pub bytes: u64,
    pub sha256: Option<String>,
    /// 超出单录像上限后停止写入
    pub truncated: bool,
}

enum Event {
    Out(f64, Vec<u8>),
    In(f64, Vec<u8>),
    Resize(f64, u32, u32),
}

/// 终端标签的录像槽：TermSession 与 SuperviseCtx 共享，跨自动重连保留
pub(crate) type RecSlot = Arc<Mutex<Option<Arc<Recorder>>>>;

pub struct Recorder {
    pub(crate) id: String,
    t0: Instant,
    tx: Mutex<Option<mpsc::Sender<Event>>>,
    join: Mutex<Option<JoinHandle<RecordingMeta>>>,
    /// AI 自动开始且未被用户接管：AI 退出控制时随之停止
    agent_owned: AtomicBool,
}

impl Recorder {
    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn send(&self, ev: Event) {
        if let Some(tx) = self.tx.lock().as_ref() {
            let _ = tx.send(ev);
        }
    }

    pub(crate) fn output(&self, data: &[u8]) {
        if !data.is_empty() {
            self.send(Event::Out(self.now(), data.to_vec()));
        }
    }

    pub(crate) fn input(&self, data: &[u8]) {
        if !data.is_empty() {
            self.send(Event::In(self.now(), data.to_vec()));
        }
    }

    pub(crate) fn resize(&self, cols: u32, rows: u32) {
        self.send(Event::Resize(self.now(), cols, rows));
    }

    pub(crate) fn is_agent_owned(&self) -> bool {
        self.agent_owned.load(Ordering::Acquire)
    }

    /// 用户手动开始录制时接管 AI 的自动录像：AI 退出后继续录
    pub(crate) fn claim_for_user(&self) {
        self.agent_owned.store(false, Ordering::Release);
    }

    /// 停止接收（非阻塞）：写线程随后收尾 sha256 / 元数据 / 审计
    pub(crate) fn stop(&self) {
        self.tx.lock().take();
    }

    /// 停止并等写线程收尾（阻塞，async 上下文经 spawn_blocking 调用）
    pub(crate) fn finish(&self) -> Option<RecordingMeta> {
        self.stop();
        let handle = self.join.lock().take()?;
        handle.join().ok()
    }
}

/// 读循环 tee 点：仅在录制中时拷贝
pub(crate) fn tee(slot: &RecSlot, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    if let Some(r) = slot.lock().as_ref() {
        r.output(data);
    }
}

/// 标签终结：停止录像
pub(crate) fn stop_slot(slot: &RecSlot) {
    if let Some(r) = slot.lock().take() {
        r.stop();
    }
}

/// AI 退出控制：只停 AI 自动开始、且未被用户接管的那一份录像
pub(crate) fn release_agent(slot: &RecSlot, rec: &Arc<Recorder>) {
    let mut g = slot.lock();
    if g.as_ref()
        .is_some_and(|r| Arc::ptr_eq(r, rec) && r.is_agent_owned())
    {
        if let Some(r) = g.take() {
            r.stop();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 取出 buf 中全部完整的 UTF-8 内容（非法序列替换为 U+FFFD），末尾不完整的多字节序列
/// 留在 buf 等下一块；flush=true 时连残留一并替换输出
pub(crate) fn take_utf8(buf: &mut Vec<u8>, flush: bool) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < buf.len() {
        match std::str::from_utf8(&buf[i..]) {
            Ok(s) => {
                out.push_str(s);
                i = buf.len();
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(&String::from_utf8_lossy(&buf[i..i + valid]));
                i += valid;
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        i += n;
                    }
                    None => {
                        if flush {
                            out.push('\u{FFFD}');
                            i = buf.len();
                        }
                        break;
                    }
                }
            }
        }
    }
    buf.drain(..i);
    out
}

fn event_line(t: f64, code: &str, data: &str) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let t = (t * 1e6).round() / 1e6;
    Some(format!("{}\n", json!([t, code, data])))
}

/// 边写边算 sha256，免得收尾时再整读一遍文件
struct HashingWriter {
    inner: BufWriter<File>,
    hasher: Sha256,
    bytes: u64,
}

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_meta(path: &PathBuf, meta: &RecordingMeta) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?,
    )?;
    fs::rename(&tmp, path)
}

/// 写一行事件；超单录像上限则标 truncated 不再写。返回 false = 磁盘写失败
fn emit(w: &mut HashingWriter, meta: &mut RecordingMeta, line: Option<String>) -> bool {
    let Some(line) = line else { return true };
    if w.bytes + line.len() as u64 > REC_MAX_BYTES {
        meta.truncated = true;
        return true;
    }
    w.write_all(line.as_bytes()).is_ok()
}

fn write_loop(
    rx: mpsc::Receiver<Event>,
    file: File,
    mut meta: RecordingMeta,
    meta_path: PathBuf,
    audit: Option<Arc<core_store::Store>>,
) -> RecordingMeta {
    let mut w = HashingWriter {
        inner: BufWriter::new(file),
        hasher: Sha256::new(),
        bytes: 0,
    };
    let header = json!({
        "version": 2,
        "width": meta.cols,
        "height": meta.rows,
        "timestamp": meta.started_at / 1000,
        "title": meta.title,
        "env": { "TERM": "xterm-256color" },
    });
    let mut ok = writeln!(w, "{header}").is_ok();
    let mut out_carry = Vec::new();
    let mut in_carry = Vec::new();
    let mut last = 0.0f64;
    for ev in rx {
        if !ok || meta.truncated {
            continue;
        }
        let line = match ev {
            Event::Out(t, d) => {
                last = t;
                out_carry.extend_from_slice(&d);
                event_line(t, "o", &take_utf8(&mut out_carry, false))
            }
            Event::In(t, d) => {
                last = t;
                in_carry.extend_from_slice(&d);
                event_line(t, "i", &take_utf8(&mut in_carry, false))
            }
            Event::Resize(t, c, r) => {
                last = t;
                event_line(t, "r", &format!("{c}x{r}"))
            }
        };
        ok = emit(&mut w, &mut meta, line);
    }
    if ok && !meta.truncated {
        let tail = event_line(last, "o", &take_utf8(&mut out_carry, true));
        ok = emit(&mut w, &mut meta, tail);
    }
    ok = ok && w.flush().is_ok();
    meta.ended_at = Some(now_ms());
    meta.duration_ms = (last * 1000.0) as u64;
    meta.bytes = w.bytes;
    meta.sha256 = ok.then(|| hex(&w.hasher.finalize()));
    if let Err(e) = write_meta(&meta_path, &meta) {
        tracing::warn!(id = %meta.id, error = %e, "录像元数据写入失败");
    }
    if let Some(store) = audit {
        let actor = match meta.source {
            RecSource::User => core_store::Actor::Gui,
            RecSource::Agent => core_store::Actor::Mcp,
        };
        let detail = json!({
            "id": meta.id,
            "title": meta.title,
            "bytes": meta.bytes,
            "sha256": meta.sha256,
            "truncated": meta.truncated,
        });
        let sid = meta.session_id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = store
                .audit()
                .append(actor, sid.as_deref(), AUDIT_ACTION, &detail)
                .await;
        });
    }
    meta
}

/// 录像目录：<id>.cast + <id>.json
pub struct RecordingStore {
    root: PathBuf,
    /// 收尾时落审计（测试为 None）
    audit: Option<Arc<core_store::Store>>,
}

impl RecordingStore {
    pub fn new(root: PathBuf, audit: Option<Arc<core_store::Store>>) -> Arc<Self> {
        Arc::new(Self { root, audit })
    }

    pub fn default_root() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("myssh")
            .join("recordings")
    }

    /// id 只含 hex 与 '-'，拒绝其它字符防路径穿越
    fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    }

    fn cast_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.cast"))
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn checked(&self, id: &str) -> Result<(), String> {
        if Self::valid_id(id) {
            Ok(())
        } else {
            Err(format!("非法录像 id：{id}"))
        }
    }

    pub(crate) fn start(
        &self,
        title: &str,
        source: RecSource,
        session_id: Option<String>,
        cols: u32,
        rows: u32,
    ) -> Result<Arc<Recorder>, String> {
        fs::create_dir_all(&self.root).map_err(|e| format!("创建录像目录失败: {e}"))?;
        self.prune();
        let started_at = now_ms();
        let id = format!("{started_at:x}-{:08x}", rand::random::<u32>());
        let file =
            File::create(self.cast_path(&id)).map_err(|e| format!("创建录像文件失败: {e}"))?;
        let meta = RecordingMeta {
            id: id.clone(),
            title: title.to_string(),
            source,
            session_id,
            cols: cols.max(1),
            rows: rows.max(1),
            started_at,
            ended_at: None,
            duration_ms: 0,
            bytes: 0,
            sha256: None,
            truncated: false,
        };
        let meta_path = self.meta_path(&id);
        write_meta(&meta_path, &meta).map_err(|e| format!("写录像元数据失败: {e}"))?;
        let (tx, rx) = mpsc::channel();
        let audit = self.audit.clone();
        let join = std::thread::Builder::new()
            .name("myssh-recorder".into())
            .spawn(move || write_loop(rx, file, meta, meta_path, audit))
            .map_err(|e| format!("启动录像线程失败: {e}"))?;
        Ok(Arc::new(Recorder {
            id,
            t0: Instant::now(),
            tx: Mutex::new(Some(tx)),
            join: Mutex::new(Some(join)),
            agent_owned: AtomicBool::new(source == RecSource::Agent),
        }))
    }

    /// 全部录像元数据，新的在前
    pub(crate) fn list(&self) -> Vec<RecordingMeta> {
        let Ok(rd) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut v: Vec<RecordingMeta> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.started_at));
        v
    }

    pub(crate) fn meta(&self, id: &str) -> Result<RecordingMeta, String> {
        self.checked(id)?;
        let b = fs::read(self.meta_path(id)).map_err(|e| format!("录像 {id} 不存在: {e}"))?;
        serde_json::from_slice(&b).map_err(|e| format!("录像元数据损坏: {e}"))
    }

    /// 播放用：整读 .cast 文本
    pub(crate) fn load(&self, id: &str) -> Result<String, String> {
        self.checked(id)?;
        let path = self.cast_path(id);
        let len = fs::metadata(&path)
            .map_err(|e| format!("录像 {id} 不存在: {e}"))?
            .len();
        if len > LOAD_MAX_BYTES {
            return Err(format!(
                "录像过大（{} MB），无法在应用内播放；可导出为文本或用 asciinema 播放",
                len / 1024 / 1024
            ));
        }
        fs::read_to_string(&path).map_err(|e| format!("读取录像失败: {e}"))
    }

    /// 纯文本导出：拼接全部输出事件后剥离 ANSI 控制序列
    pub(crate) fn export_text(&self, id: &str) -> Result<String, String> {
        self.checked(id)?;
        let f = File::open(self.cast_path(id)).map_err(|e| format!("录像 {id} 不存在: {e}"))?;
        let mut raw = String::new();
        for line in BufReader::new(f).lines().skip(1) {
            let line = line.map_err(|e| format!("读取录像失败: {e}"))?;
            if let Ok(Value::Array(ev)) = serde_json::from_str::<Value>(&line) {
                if ev.get(1).and_then(Value::as_str) == Some("o") {
                    if let Some(s) = ev.get(2).and_then(Value::as_str) {
                        raw.push_str(s);
                    }
                }
            }
        }
        Ok(crate::mcp_text::strip_ansi(raw.as_bytes(), true).0)
    }

    /// 重新计算录像文件 sha256
    pub(crate) fn file_sha256(&self, id: &str) -> Result<String, String> {
        self.checked(id)?;
        let mut f = File::open(self.cast_path(id)).map_err(|e| format!("录像 {id} 不存在: {e}"))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf).map_err(|e| format!("读取录像失败: {e}"))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex(&h.finalize()))
    }

    pub(crate) fn delete(&self, id: &str) -> Result<(), String> {
        self.checked(id)?;
        fs::remove_file(self.cast_path(id)).map_err(|e| format!("删除录像失败: {e}"))?;
        let _ = fs::remove_file(self.meta_path(id));
        Ok(())
    }

    /// 超出保留条数时删最旧的已结束录像
    fn prune(&self) {
        let done: Vec<RecordingMeta> = self
            .list()
            .into_iter()
            .filter(|m| m.ended_at.is_some())
            .collect();
        for m in done.iter().skip(MAX_RECORDINGS.saturating_sub(1)) {
            let _ = self.delete(&m.id);
        }
    }
}

/// 校验录像完整性：文件 sha256 与元数据、审计记录三方比对
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub file_sha256: String,
    pub meta_sha256: Option<String>,
    pub audit_sha256: Option<String>,
    /// 审计哈希链本身是否完整
    pub audit_chain_ok: bool,
    pub ok: bool,
    pub reason: String,
}

fn verify_report(
    file: String,
    meta: Option<String>,
    audit: Option<String>,
    chain_ok: bool,
) -> VerifyReport {
    let reason = if !chain_ok {
        "审计记录哈希链断裂：审计日志可能被改动".to_string()
    } else if audit.is_none() {
        "没有找到该录像的审计记录（录制未正常结束，或审计已按保留策略清理）".to_string()
    } else if audit.as_deref() != Some(file.as_str()) {
        "录像文件摘要与审计记录不一致：录像可能被改动".to_string()
    } else if meta.as_deref() != Some(file.as_str()) {
        "录像与审计一致，但元数据文件摘要不符（元数据可能被改动）".to_string()
    } else {
        "录像与审计记录一致，未被改动".to_string()
    };
    let ok = chain_ok && audit.as_deref() == Some(file.as_str());
    VerifyReport {
        file_sha256: file,
        meta_sha256: meta,
        audit_sha256: audit,
        audit_chain_ok: chain_ok,
        ok,
        reason,
    }
}

#[tauri::command]
pub async fn recordings_list(
    recs: tauri::State<'_, Arc<RecordingStore>>,
) -> Result<Vec<RecordingMeta>, String> {
    let recs = recs.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recs.list())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn recording_load(
    id: String,
    recs: tauri::State<'_, Arc<RecordingStore>>,
) -> Result<String, String> {
    let recs = recs.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recs.load(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn recording_export_text(
    id: String,
    dest: String,
    recs: tauri::State<'_, Arc<RecordingStore>>,
) -> Result<(), String> {
    let recs = recs.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let text = recs.export_text(&id)?;
        fs::write(&dest, text).map_err(|e| format!("写入 {dest} 失败: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn recording_delete(
    id: String,
    recs: tauri::State<'_, Arc<RecordingStore>>,
) -> Result<(), String> {
    let recs = recs.inner().clone();
    tauri::async_runtime::spawn_blocking(move || recs.delete(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn recording_verify(
    id: String,
    recs: tauri::State<'_, Arc<RecordingStore>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<VerifyReport, String> {
    let r = recs.inner().clone();
    let id2 = id.clone();
    let (file, meta) = tauri::async_runtime::spawn_blocking(move || {
        let meta = r.meta(&id2)?;
        Ok::<_, String>((r.file_sha256(&id2)?, meta.sha256))
    })
    .await
    .map_err(|e| e.to_string())??;
    let audit = sessions.store.audit();
    let rec = audit
        .find_by_detail_id(AUDIT_ACTION, &id)
        .await
        .map_err(|e| e.to_string())?;
    let audit_sha = rec
        .as_ref()
        .and_then(|r| r.detail.get("sha256"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let chain_ok = audit.verify_chain().await.map_err(|e| e.to_string())?.ok;
    Ok(verify_report(file, meta, audit_sha, chain_ok))
}

/// 审计日志哈希链整体校验
#[tauri::command]
pub async fn audit_verify_chain(
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<core_store::ChainReport, String> {
    sessions
        .store
        .audit()
        .verify_chain()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "myssh-rec-{tag}-{}-{:x}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn utf8_carry_keeps_split_chars_and_replaces_invalid() {
        let s = "中文".as_bytes();
        let mut buf = s[..4].to_vec(); // 「中」完整 +「文」的首字节
        assert_eq!(take_utf8(&mut buf, false), "中");
        assert_eq!(buf.len(), 1, "不完整的多字节序列留待下一块");
        buf.extend_from_slice(&s[4..]);
        assert_eq!(take_utf8(&mut buf, false), "文");
        assert!(buf.is_empty());
        let mut bad = vec![b'a', 0xff, b'b'];
        assert_eq!(take_utf8(&mut bad, false), "a\u{FFFD}b");
        let mut tail = vec![b'x', 0xe4];
        assert_eq!(
            take_utf8(&mut tail, true),
            "x\u{FFFD}",
            "收尾时残留也要输出"
        );
    }

    #[test]
    fn records_asciicast_v2_and_hashes_file() {
        let store = RecordingStore::new(temp_root("rt"), None);
        let rec = store
            .start("web-01", RecSource::User, Some("s1".into()), 100, 30)
            .unwrap();
        let s = "日志".as_bytes();
        rec.output(b"\x1b[32mok\x1b[0m\r\n");
        rec.output(&s[..2]); // 跨块拆开的多字节字符
        rec.output(&s[2..]);
        rec.input(b"ls\r");
        rec.resize(120, 40);
        let meta = rec.finish().unwrap();
        assert!(meta.ended_at.is_some());
        assert_eq!(
            meta.sha256.as_deref(),
            Some(store.file_sha256(&meta.id).unwrap().as_str())
        );
        assert_eq!(store.meta(&meta.id).unwrap(), meta, "元数据落盘与返回一致");

        let cast = store.load(&meta.id).unwrap();
        let mut lines = cast.lines();
        let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 100);
        assert_eq!(header["height"], 30);
        let events: Vec<Value> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        let kinds: Vec<&str> = events.iter().map(|e| e[1].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["o", "o", "i", "r"],
            "拆开的半个字符不单独成事件"
        );
        assert_eq!(events[1][2], "日志");
        assert_eq!(events[3][2], "120x40");
        assert_eq!(store.export_text(&meta.id).unwrap(), "ok\n日志");
        assert_eq!(store.list().len(), 1);
        store.delete(&meta.id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn stop_without_join_still_finalizes() {
        let store = RecordingStore::new(temp_root("stop"), None);
        let rec = store.start("t", RecSource::Agent, None, 80, 24).unwrap();
        rec.output(b"hello");
        rec.stop();
        rec.output(b"late");
        let meta = rec.finish().unwrap();
        assert!(meta.sha256.is_some());
        assert_eq!(
            store.export_text(&meta.id).unwrap(),
            "hello",
            "停止后的输出不再写入"
        );
    }

    #[test]
    fn release_agent_only_stops_unclaimed_agent_recording() {
        let store = RecordingStore::new(temp_root("own"), None);
        let slot = RecSlot::default();
        let rec = store.start("t", RecSource::Agent, None, 80, 24).unwrap();
        *slot.lock() = Some(rec.clone());
        rec.claim_for_user();
        release_agent(&slot, &rec);
        assert!(slot.lock().is_some(), "用户接管后 AI 退出不停录");
        stop_slot(&slot);
        assert!(slot.lock().is_none());

        let rec2 = store.start("t2", RecSource::Agent, None, 80, 24).unwrap();
        *slot.lock() = Some(rec2.clone());
        release_agent(&slot, &rec2);
        assert!(slot.lock().is_none(), "AI 自动录像随 AI 退出停止");
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let store = RecordingStore::new(temp_root("id"), None);
        assert!(store.load("../secret").is_err());
        assert!(store.delete("a/b").is_err());
    }

    #[test]
    fn verify_report_reasons() {
        let f = "aa".to_string();
        assert!(verify_report(f.clone(), Some(f.clone()), Some(f.clone()), true).ok);
        let r = verify_report(f.clone(), Some(f.clone()), Some("bb".into()), true);
        assert!(!r.ok && r.reason.contains("不一致"));
        assert!(!verify_report(f.clone(), Some(f.clone()), None, true).ok);
        assert!(!verify_report(f.clone(), Some(f.clone()), Some(f.clone()), false).ok);
        let r = verify_report(f.clone(), Some("cc".into()), Some(f), true);
        assert!(r.ok, "元数据被改不影响录像本身可信");
    }
}
