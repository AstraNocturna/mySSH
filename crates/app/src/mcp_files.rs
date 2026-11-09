//! MCP 远程文件工具（file_view / file_edit / file_grep / file_glob）与 AI 改动备份回滚
//! （file_backups / file_rollback）。
//!
//! 经 MCP 的改写（file_edit、sftp_write、sftp_delete 文件、回滚本身）先把原内容落本地备份
//! （%LOCALAPPDATA%/myssh/ai-backups），并记下写后内容的 sha256。回滚时若远端内容与写后哈希
//! 不符（AI 改完后又被别人改过），默认拒绝，force=true 才覆盖——避免回滚吞掉他人改动。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use core_sftp::{EntryKind, SftpClient};
use core_store::{Actor, Store};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::sftp::{SftpCtx, SftpManagerState};

/// view/edit/备份的单文件上限
pub(crate) const FILE_MAX: u64 = 8 * 1024 * 1024;
const VIEW_DEFAULT_LINES: usize = 400;
const VIEW_MAX_LINES: usize = 2000;
/// 单行展示上限（压缩过的 JS/日志一行可达数 MB，整行塞给模型无意义）
const LINE_MAX_CHARS: usize = 2000;
const GREP_LINE_MAX_CHARS: usize = 300;
const GREP_FILE_MAX: u64 = 2 * 1024 * 1024;
const GREP_DEFAULT_RESULTS: usize = 100;
const GREP_MAX_RESULTS: usize = 500;
const GLOB_DEFAULT_RESULTS: usize = 200;
const GLOB_MAX_RESULTS: usize = 1000;
/// 遍历上限：条目数、深度、时长（超出即停并标 truncated，返回已得结果）
const WALK_MAX_ENTRIES: usize = 20_000;
const WALK_MAX_DEPTH: usize = 24;
const WALK_BUDGET: Duration = Duration::from_secs(20);
const GREP_PARALLEL: usize = 8;
const SKIP_DIRS: &[&str] = &[".git", ".svn", ".hg", "node_modules"];
/// 备份保留条数（超出删最旧）；按天数过期另见 retention 模块
const MAX_BACKUPS: usize = 300;
/// AI 自述改动标题的长度上限（字符）
const TITLE_MAX_CHARS: usize = 120;
static BACKUP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 乐观并发标记：sha256 前 16 位（file_view 返回，file_edit 可带 expected_etag 校验）
fn etag(data: &[u8]) -> String {
    let mut h = sha256_hex(data);
    h.truncate(16);
    h
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ———————————————————————————— 备份存储 ————————————————————————————

/// 一条 AI 改动备份
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackupMeta {
    pub id: String,
    pub session_id: String,
    pub path: String,
    /// 触发改动的工具（file_edit / sftp_write / sftp_delete / rollback）
    pub tool: String,
    /// 改动前文件是否存在（false = 回滚即删除该文件）
    pub existed: bool,
    pub bytes: u64,
    /// 改动后内容的 sha256；None = 改动后文件不存在（删除）
    pub after_sha256: Option<String>,
    /// unix 毫秒
    pub created_at: u64,
    /// AI 自述的改动标题（未经核实，仅供辨认）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 据改前/改后内容算出的改动摘要；旧版本写的备份没有此字段
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<ChangeSummary>,
}

/// 改动摘要（前端按语言渲染成一句话）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChangeSummary {
    Created {
        lines: usize,
    },
    Deleted {
        lines: usize,
    },
    /// line = 首个不同行（1 起，按改后内容）；added/removed = 中间差异段的行数
    Modified {
        line: usize,
        added: usize,
        removed: usize,
    },
    Unchanged,
    Binary {
        before_bytes: u64,
        after_bytes: u64,
    },
}

fn line_count(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

/// 公共前缀/后缀行之外即差异段：不求最小编辑距离，够用户辨认「改了哪儿、改了多少」
pub(crate) fn summarize_change(before: Option<&[u8]>, after: Option<&[u8]>) -> ChangeSummary {
    let text = |b: &[u8]| {
        if looks_binary(b) {
            None
        } else {
            std::str::from_utf8(b).ok().map(str::to_owned)
        }
    };
    match (before, after) {
        (None, None) => ChangeSummary::Unchanged,
        (None, Some(a)) => match text(a) {
            Some(t) => ChangeSummary::Created {
                lines: line_count(&t),
            },
            None => ChangeSummary::Binary {
                before_bytes: 0,
                after_bytes: a.len() as u64,
            },
        },
        (Some(b), None) => match text(b) {
            Some(t) => ChangeSummary::Deleted {
                lines: line_count(&t),
            },
            None => ChangeSummary::Binary {
                before_bytes: b.len() as u64,
                after_bytes: 0,
            },
        },
        (Some(b), Some(a)) if b == a => ChangeSummary::Unchanged,
        (Some(b), Some(a)) => match (text(b), text(a)) {
            (Some(tb), Some(ta)) => {
                let lb: Vec<&str> = tb.split_inclusive('\n').collect();
                let la: Vec<&str> = ta.split_inclusive('\n').collect();
                let prefix = lb.iter().zip(&la).take_while(|(x, y)| x == y).count();
                let suffix = lb[prefix..]
                    .iter()
                    .rev()
                    .zip(la[prefix..].iter().rev())
                    .take_while(|(x, y)| x == y)
                    .count();
                ChangeSummary::Modified {
                    line: prefix + 1,
                    added: la.len() - prefix - suffix,
                    removed: lb.len() - prefix - suffix,
                }
            }
            _ => ChangeSummary::Binary {
                before_bytes: b.len() as u64,
                after_bytes: a.len() as u64,
            },
        },
    }
}

/// AI 自述的改动标题：去空白，截断到 TITLE_MAX_CHARS
pub(crate) fn title_arg(args: &Value) -> Option<String> {
    let t = args.get("title").and_then(Value::as_str)?.trim();
    (!t.is_empty()).then(|| t.chars().take(TITLE_MAX_CHARS).collect())
}

/// 本地备份目录：<id>.json 元数据 + <id>.bin 原内容（改动前不存在则无 .bin）
pub struct BackupStore {
    root: PathBuf,
    lock: tokio::sync::Mutex<()>,
    /// 保留时长（毫秒）；0 = 不按时间过期，只按条数封顶
    retention_ms: std::sync::atomic::AtomicU64,
}

impl BackupStore {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            lock: tokio::sync::Mutex::new(()),
            retention_ms: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn set_retention_days(&self, days: u32) {
        self.retention_ms.store(
            u64::from(days) * 86_400_000,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn default_root() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("myssh")
            .join("ai-backups")
    }

    /// id 只含时间戳与 hex，拒绝其它字符防路径穿越
    fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn data_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.bin"))
    }

    pub async fn save(
        &self,
        session_id: &str,
        path: &str,
        tool: &str,
        title: Option<&str>,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
    ) -> Result<BackupMeta, String> {
        let _g = self.lock.lock().await;
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| format!("备份目录创建失败: {e}"))?;
        let created_at = now_ms();
        // 同毫秒内靠进程内序号保序（列表与清理按 id 字典序 = 时间序）
        let seq = BACKUP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xff_ffff;
        let id = format!("{created_at:013}-{seq:06x}{:04x}", rand::random::<u16>());
        let meta = BackupMeta {
            id: id.clone(),
            session_id: session_id.to_string(),
            path: path.to_string(),
            tool: tool.to_string(),
            existed: before.is_some(),
            bytes: before.map_or(0, |b| b.len() as u64),
            after_sha256: after.map(sha256_hex),
            created_at,
            title: title.map(str::to_owned),
            change: Some(summarize_change(before, after)),
        };
        if let Some(b) = before {
            tokio::fs::write(self.data_path(&id), b)
                .await
                .map_err(|e| format!("备份写入失败: {e}"))?;
        }
        let js = serde_json::to_vec(&meta).map_err(|e| e.to_string())?;
        tokio::fs::write(self.meta_path(&id), js)
            .await
            .map_err(|e| format!("备份写入失败: {e}"))?;
        self.prune_locked().await;
        Ok(meta)
    }

    /// 改动失败时撤掉对应备份（不留无效记录）
    pub async fn discard(&self, id: &str) {
        if !Self::valid_id(id) {
            return;
        }
        let _g = self.lock.lock().await;
        let _ = tokio::fs::remove_file(self.meta_path(id)).await;
        let _ = tokio::fs::remove_file(self.data_path(id)).await;
    }

    async fn ids_sorted(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let Ok(mut rd) = tokio::fs::read_dir(&self.root).await else {
            return ids;
        };
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                if Self::valid_id(id) {
                    ids.push(id.to_string());
                }
            }
        }
        // id 以 13 位毫秒时间戳开头：字典序即时间序
        ids.sort();
        ids
    }

    /// 删掉过期（早于保留天数）与超出条数上限的最旧备份，返回删除条数
    async fn prune_locked(&self) -> usize {
        let ids = self.ids_sorted().await;
        let retention = self.retention_ms.load(std::sync::atomic::Ordering::Relaxed);
        let cutoff = if retention == 0 {
            0
        } else {
            now_ms().saturating_sub(retention)
        };
        // id 前 13 位是创建时间（毫秒）；ids 升序，过期的都在前面
        let expired = ids
            .iter()
            .take_while(|id| {
                id.get(..13)
                    .and_then(|t| t.parse::<u64>().ok())
                    .is_some_and(|t| t < cutoff)
            })
            .count();
        let remove = expired.max(ids.len().saturating_sub(MAX_BACKUPS));
        for id in &ids[..remove] {
            let _ = tokio::fs::remove_file(self.meta_path(id)).await;
            let _ = tokio::fs::remove_file(self.data_path(id)).await;
        }
        remove
    }

    pub async fn prune(&self) -> usize {
        let _g = self.lock.lock().await;
        self.prune_locked().await
    }

    /// 新→旧；session_id 给定时只列该会话
    pub async fn list(&self, session_id: Option<&str>, limit: usize) -> Vec<BackupMeta> {
        let mut out = Vec::new();
        for id in self.ids_sorted().await.iter().rev() {
            if out.len() >= limit {
                break;
            }
            let Ok(raw) = tokio::fs::read(self.meta_path(id)).await else {
                continue;
            };
            let Ok(m) = serde_json::from_slice::<BackupMeta>(&raw) else {
                continue;
            };
            if session_id.is_none_or(|s| s == m.session_id) {
                out.push(m);
            }
        }
        out
    }

    pub async fn load(&self, id: &str) -> Result<(BackupMeta, Option<Vec<u8>>), String> {
        if !Self::valid_id(id) {
            return Err(format!("非法备份 id：{id}"));
        }
        let raw = tokio::fs::read(self.meta_path(id))
            .await
            .map_err(|_| format!("备份 {id} 不存在（可能已过保留期或超出条数上限被清理）"))?;
        let meta: BackupMeta = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let data = if meta.existed {
            Some(
                tokio::fs::read(self.data_path(id))
                    .await
                    .map_err(|e| format!("备份内容读取失败: {e}"))?,
            )
        } else {
            None
        };
        Ok((meta, data))
    }
}

// ———————————————————————————— 纯逻辑 ————————————————————————————

#[derive(Debug, PartialEq)]
pub(crate) struct EditOutcome {
    pub content: String,
    pub replacements: usize,
    /// 首个替换处所在行（1 起）
    pub line: usize,
}

/// 精确字符串替换：默认要求唯一匹配；CRLF 文件里给的 LF 片段自动按 CRLF 匹配
pub(crate) fn apply_edit(
    orig: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<EditOutcome, String> {
    if old.is_empty() {
        return Err("old_str 不能为空（新建文件请用 old_str=\"\" 且文件不存在）".into());
    }
    if old == new {
        return Err("old_str 与 new_str 相同，无需修改".into());
    }
    let (old_s, new_s) = if orig.contains(old) {
        (old.to_string(), new.to_string())
    } else {
        let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
        let o = crlf(old);
        if orig.contains("\r\n") && old.contains('\n') && orig.contains(&o) {
            (o, crlf(new))
        } else {
            return Err("未找到 old_str（需与文件内容逐字一致，含缩进与空白）".into());
        }
    };
    let count = orig.matches(&old_s).count();
    if count > 1 && !replace_all {
        return Err(format!(
            "old_str 匹配到 {count} 处；请加更多上下文使其唯一，或设 replace_all=true"
        ));
    }
    let pos = orig.find(&old_s).unwrap_or(0);
    let line = orig[..pos].matches('\n').count() + 1;
    let content = if replace_all {
        orig.replace(&old_s, &new_s)
    } else {
        orig.replacen(&old_s, &new_s, 1)
    };
    Ok(EditOutcome {
        content,
        replacements: if replace_all { count } else { 1 },
        line,
    })
}

fn clip_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…[行过长已截断]", &s[..i]),
        None => s.to_string(),
    }
}

/// cat -n 风格分页视图：首行为摘要，其后「行号<TAB>内容」
pub(crate) fn render_view(
    path: &str,
    text: &str,
    offset: usize,
    limit: usize,
    tag: &str,
) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.max(1);
    let end = (start - 1 + limit).min(total);
    let mut out = if total == 0 {
        format!("{path} · 空文件 · etag {tag}\n")
    } else if start > total {
        format!("{path} · 共 {total} 行，offset={start} 超出末尾 · etag {tag}\n")
    } else {
        format!("{path} · 第 {start}-{end} 行 / 共 {total} 行 · etag {tag}\n")
    };
    if start <= total {
        for (i, l) in lines[start - 1..end].iter().enumerate() {
            out.push_str(&format!(
                "{:>6}\t{}\n",
                start + i,
                clip_chars(l, LINE_MAX_CHARS)
            ));
        }
        if end < total {
            out.push_str(&format!(
                "…（还有 {} 行，用 offset={} 继续）\n",
                total - end,
                end + 1
            ));
        }
    }
    out
}

/// glob → 正则：`**/` 跨任意层目录，`*`/`?` 不跨 `/`，`{a,b}` 选择。
/// 不含 `/` 的模式只匹配文件名（任意深度），含 `/` 的匹配相对根的路径。
pub(crate) fn glob_to_regex(glob: &str) -> Result<Regex, String> {
    let mut re = String::from("^");
    let mut chars = glob.chars().peekable();
    let mut depth = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        re.push_str("(?:.*/)?");
                    } else {
                        re.push_str(".*");
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            ',' if depth > 0 => re.push('|'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    if depth > 0 {
        return Err(format!("glob 花括号未闭合：{glob}"));
    }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("glob 无效：{e}"))
}

fn glob_matches(re: &Regex, by_name: bool, rel: &str) -> bool {
    if by_name {
        re.is_match(rel.rsplit('/').next().unwrap_or(rel))
    } else {
        re.is_match(rel)
    }
}

fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn is_not_found(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("no such file") || l.contains("nosuchfile")
}

/// 逐行匹配；返回 (行号, 截断后的行)
fn grep_text(re: &Regex, text: &str, cap: usize) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| re.is_match(l))
        .take(cap)
        .map(|(i, l)| (i + 1, clip_chars(l.trim_end(), GREP_LINE_MAX_CHARS)))
        .collect()
}

fn looks_binary(data: &[u8]) -> bool {
    data.iter().take(8192).any(|b| *b == 0)
}

// ———————————————————————————— 远端读写 ————————————————————————————

/// 读整个远端文件；不存在 → Ok(None)；超过 cap / 是目录 → Err
pub(crate) async fn read_remote(
    ctx: &SftpCtx,
    path: &str,
    cap: u64,
) -> Result<Option<Vec<u8>>, String> {
    let client = ctx.meta_client().await?;
    let st = match client.lstat(path).await {
        Ok(e) => e,
        Err(e) => {
            let msg = e.to_string();
            return if is_not_found(&msg) {
                Ok(None)
            } else {
                Err(msg)
            };
        }
    };
    if st.kind == EntryKind::Dir {
        return Err(format!("{path} 是目录"));
    }
    if st.kind == EntryKind::File && st.size > cap {
        return Err(format!(
            "{path} 大小 {} 字节，超过上限 {cap}（大文件请用 sftp_download）",
            st.size
        ));
    }
    read_capped(&client, path, cap).await.map(Some)
}

async fn read_capped(client: &SftpClient, path: &str, cap: u64) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let f = client.open_read(path).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.take(cap + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > cap {
        return Err(format!("{path} 超过上限 {cap} 字节"));
    }
    Ok(buf)
}

/// 改动前备份的结果：saved = 已备份；skipped = 原因（如超限），改动照常进行
pub(crate) enum Backup {
    Saved(BackupMeta),
    Skipped(String),
}

impl Backup {
    pub(crate) fn id(&self) -> Option<&str> {
        match self {
            Backup::Saved(m) => Some(&m.id),
            Backup::Skipped(_) => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            Backup::Saved(m) => json!({ "backupId": m.id }),
            Backup::Skipped(r) => json!({ "backupId": null, "backupSkipped": r }),
        }
    }

    pub(crate) async fn discard(&self, backups: &BackupStore) {
        if let Backup::Saved(m) = self {
            backups.discard(&m.id).await;
        }
    }
}

/// sftp_write / sftp_delete 改动前备份现有内容（读不到或超限时跳过，不阻断改动）
pub(crate) async fn backup_existing(
    ctx: &SftpCtx,
    backups: &BackupStore,
    session_id: &str,
    path: &str,
    tool: &str,
    title: Option<&str>,
    after: Option<&[u8]>,
) -> Backup {
    match read_remote(ctx, path, FILE_MAX).await {
        Ok(before) => match backups
            .save(session_id, path, tool, title, before.as_deref(), after)
            .await
        {
            Ok(m) => Backup::Saved(m),
            Err(e) => Backup::Skipped(e),
        },
        Err(e) => Backup::Skipped(format!("原内容未备份：{e}")),
    }
}

/// 单文件 grep 结果：(遍历序号, 路径, [(行号, 行)])
type GrepHits = (usize, String, Vec<(usize, String)>);

/// 遍历结果中的文件
struct WalkFile {
    path: String,
    rel: String,
    size: u64,
}

/// BFS 遍历目录（不跟随目录软链接；跳过 .git/node_modules 等）。返回 (文件, 是否截断)
async fn walk(client: &SftpClient, root: &str, deadline: Instant) -> (Vec<WalkFile>, bool) {
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([(root.to_string(), String::new(), 0usize)]);
    let mut seen = 0usize;
    while let Some((dir, rel_dir, depth)) = queue.pop_front() {
        if Instant::now() > deadline || seen >= WALK_MAX_ENTRIES {
            return (files, true);
        }
        let Ok(entries) = client.list(&dir).await else {
            continue;
        };
        for e in entries {
            seen += 1;
            let rel = if rel_dir.is_empty() {
                e.name.clone()
            } else {
                format!("{rel_dir}/{}", e.name)
            };
            let path = join_remote(&dir, &e.name);
            match e.kind {
                EntryKind::Dir => {
                    if depth < WALK_MAX_DEPTH && !SKIP_DIRS.contains(&e.name.as_str()) {
                        queue.push_back((path, rel, depth + 1));
                    }
                }
                EntryKind::File | EntryKind::Symlink => files.push(WalkFile {
                    path,
                    rel,
                    size: e.size,
                }),
                EntryKind::Other => {}
            }
        }
    }
    (files, false)
}

// ———————————————————————————— 工具层 ————————————————————————————

pub(crate) struct FileCtx<'a> {
    pub store: &'a Arc<Store>,
    pub sftp: &'a Arc<SftpManagerState>,
    pub backups: &'a Arc<BackupStore>,
}

fn req_str<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 {key}"))
}

fn opt_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_u64).map(|v| v as usize)
}

async fn audit(store: &Store, actor: Actor, session_id: &str, action: &str, detail: &Value) {
    let _ = store
        .audit()
        .append(actor, Some(session_id), action, detail)
        .await;
}

pub(crate) async fn file_tool(
    ctx: &FileCtx<'_>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "file_backups" => {
            let sid = req_str(args, "session_id", name)?;
            let limit = opt_usize(args, "limit").unwrap_or(20).clamp(1, 100);
            let items = ctx.backups.list(Some(sid), limit).await;
            Ok(json!({ "backups": items }).to_string())
        }
        "file_rollback" => {
            let sid = req_str(args, "session_id", name)?;
            let id = req_str(args, "backup_id", name)?;
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            let (meta, _) = ctx.backups.load(id).await?;
            if meta.session_id != sid {
                return Err(format!("备份 {id} 不属于会话 {sid}"));
            }
            rollback(ctx.store, ctx.sftp, ctx.backups, id, force, Actor::Mcp)
                .await
                .map(|v| v.to_string())
        }
        _ => {
            let sid = req_str(args, "session_id", name)?;
            let sctx = crate::sftp::ensure_ctx(ctx.sftp, ctx.store, sid).await?;
            match name {
                "file_view" => file_view(&sctx, args).await,
                "file_edit" => file_edit(ctx, &sctx, sid, args).await,
                "file_grep" => file_grep(&sctx, args).await,
                "file_glob" => file_glob(&sctx, args).await,
                other => Err(format!("未知工具：{other}")),
            }
        }
    }
}

async fn file_view(sctx: &SftpCtx, args: &Value) -> Result<String, String> {
    let path = req_str(args, "path", "file_view")?;
    let offset = opt_usize(args, "offset").unwrap_or(1);
    let limit = opt_usize(args, "limit")
        .unwrap_or(VIEW_DEFAULT_LINES)
        .clamp(1, VIEW_MAX_LINES);
    let data = read_remote(sctx, path, FILE_MAX)
        .await?
        .ok_or_else(|| format!("文件不存在：{path}"))?;
    if looks_binary(&data) {
        return Err(format!(
            "{path} 疑似二进制文件（{} 字节），不展示",
            data.len()
        ));
    }
    let text = String::from_utf8_lossy(&data);
    Ok(render_view(path, &text, offset, limit, &etag(&data)))
}

async fn file_edit(
    ctx: &FileCtx<'_>,
    sctx: &SftpCtx,
    sid: &str,
    args: &Value,
) -> Result<String, String> {
    let tool = "file_edit";
    let path = req_str(args, "path", tool)?;
    let old = args.get("old_str").and_then(Value::as_str).unwrap_or("");
    let new = args
        .get("new_str")
        .and_then(Value::as_str)
        .ok_or_else(|| "file_edit 缺少参数 new_str".to_string())?;
    let replace_all = args
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let before = read_remote(sctx, path, FILE_MAX).await?;
    let (content, replacements, line) = match (&before, old.is_empty()) {
        (Some(_), true) => {
            return Err(format!(
                "{path} 已存在：修改请给 old_str；整体替换用 sftp_write"
            ))
        }
        (None, true) => (new.to_string(), 0, 1),
        (None, false) => return Err(format!("文件不存在：{path}")),
        (Some(b), false) => {
            if let Some(want) = args.get("expected_etag").and_then(Value::as_str) {
                let got = etag(b);
                if want != got {
                    return Err(format!(
                        "文件已变化（etag {got} ≠ {want}），请重新 file_view 后再改"
                    ));
                }
            }
            let text = std::str::from_utf8(b)
                .map_err(|_| format!("{path} 不是 UTF-8 文本，无法按字符串编辑"))?;
            let o = apply_edit(text, old, new, replace_all)?;
            (o.content, o.replacements, o.line)
        }
    };
    if content.len() as u64 > FILE_MAX {
        return Err(format!("编辑后超过上限 {FILE_MAX} 字节"));
    }
    let title = title_arg(args);
    let backup = ctx
        .backups
        .save(
            sid,
            path,
            tool,
            title.as_deref(),
            before.as_deref(),
            Some(content.as_bytes()),
        )
        .await?;
    let p = path.to_string();
    let bytes = content.clone().into_bytes();
    if let Err(e) =
        crate::sftp::meta(sctx, move |c| async move { c.overwrite(&p, &bytes).await }).await
    {
        ctx.backups.discard(&backup.id).await;
        return Err(e);
    }
    audit(
        ctx.store,
        Actor::Mcp,
        sid,
        "mcp_file_edit",
        &json!({
            "path": path,
            "created": before.is_none(),
            "replacements": replacements,
            "bytes": content.len(),
            "backupId": backup.id,
        }),
    )
    .await;
    Ok(json!({
        "path": path,
        "created": before.is_none(),
        "replacements": replacements,
        "line": line,
        "etag": etag(content.as_bytes()),
        "backupId": backup.id,
        "hint": "可用 file_rollback 撤销本次改动",
    })
    .to_string())
}

async fn file_grep(sctx: &SftpCtx, args: &Value) -> Result<String, String> {
    let tool = "file_grep";
    let root = req_str(args, "path", tool)?;
    let pattern = req_str(args, "pattern", tool)?;
    let ci = args
        .get("case_insensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let files_only = args
        .get("files_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max = opt_usize(args, "max_results")
        .unwrap_or(GREP_DEFAULT_RESULTS)
        .clamp(1, GREP_MAX_RESULTS);
    let re = RegexBuilder::new(pattern)
        .case_insensitive(ci)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("pattern 不是合法正则：{e}"))?;
    let filter = match args
        .get("glob")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(g) => Some((glob_to_regex(g)?, !g.contains('/'))),
        None => None,
    };
    let client = sctx.meta_client().await?;
    let deadline = Instant::now() + WALK_BUDGET;
    let st = client.stat(root).await.map_err(|e| e.to_string())?;
    let (mut files, mut truncated) = if st.kind == EntryKind::Dir {
        walk(&client, root, deadline).await
    } else {
        let name = root.rsplit('/').next().unwrap_or(root).to_string();
        (
            vec![WalkFile {
                path: root.to_string(),
                rel: name,
                size: st.size,
            }],
            false,
        )
    };
    files.retain(|f| {
        f.size <= GREP_FILE_MAX
            && filter
                .as_ref()
                .is_none_or(|(g, by_name)| glob_matches(g, *by_name, &f.rel))
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let scanned = files.len();
    let re = Arc::new(re);
    let mut lines: Vec<String> = Vec::new();
    let mut set = tokio::task::JoinSet::new();
    let mut iter = files.into_iter().enumerate();
    let mut results: Vec<GrepHits> = Vec::new();
    loop {
        while set.len() < GREP_PARALLEL {
            let Some((idx, f)) = iter.next() else { break };
            let client = client.clone();
            let re = re.clone();
            set.spawn(async move {
                let hits = match read_capped(&client, &f.path, GREP_FILE_MAX).await {
                    Ok(d) if !looks_binary(&d) => {
                        grep_text(&re, &String::from_utf8_lossy(&d), GREP_MAX_RESULTS)
                    }
                    _ => Vec::new(),
                };
                (idx, f.path, hits)
            });
        }
        let Some(done) = set.join_next().await else {
            break;
        };
        if let Ok(r) = done {
            if !r.2.is_empty() {
                results.push(r);
            }
        }
        if Instant::now() > deadline {
            truncated = true;
            set.abort_all();
            break;
        }
    }
    // 并行完成顺序不定：按路径序输出，结果稳定
    results.sort_by_key(|r| r.0);
    let mut total = 0usize;
    for (_, path, hits) in results {
        if files_only {
            total += 1;
            if lines.len() < max {
                lines.push(path);
            }
            continue;
        }
        for (n, l) in hits {
            total += 1;
            if lines.len() < max {
                lines.push(format!("{path}:{n}:{l}"));
            }
        }
    }
    let shown = lines.len();
    let mut out = format!(
        "匹配 {total} {}（显示 {shown}），扫描 {scanned} 个文件{}\n",
        if files_only { "个文件" } else { "行" },
        if truncated {
            "；遍历达到上限/超时，结果可能不全，可缩小 path 或加 glob"
        } else {
            ""
        }
    );
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}

async fn file_glob(sctx: &SftpCtx, args: &Value) -> Result<String, String> {
    let tool = "file_glob";
    let root = req_str(args, "path", tool)?;
    let pattern = req_str(args, "pattern", tool)?;
    let max = opt_usize(args, "max_results")
        .unwrap_or(GLOB_DEFAULT_RESULTS)
        .clamp(1, GLOB_MAX_RESULTS);
    let re = glob_to_regex(pattern)?;
    let by_name = !pattern.contains('/');
    let client = sctx.meta_client().await?;
    let (files, truncated) = walk(&client, root, Instant::now() + WALK_BUDGET).await;
    let mut hits: Vec<&WalkFile> = files
        .iter()
        .filter(|f| glob_matches(&re, by_name, &f.rel))
        .collect();
    hits.sort_by(|a, b| a.path.cmp(&b.path));
    let total = hits.len();
    let mut out = format!(
        "匹配 {total} 个文件（显示 {}）{}\n",
        total.min(max),
        if truncated {
            "；遍历达到上限/超时，结果可能不全"
        } else {
            ""
        }
    );
    for f in hits.into_iter().take(max) {
        out.push_str(&format!("{}\t{}\n", f.path, f.size));
    }
    Ok(out)
}

/// 回滚一条备份（MCP file_rollback 与 UI「回滚」共用）。回滚前先备份当前内容，可再撤销。
pub(crate) async fn rollback(
    store: &Arc<Store>,
    sftp: &Arc<SftpManagerState>,
    backups: &BackupStore,
    id: &str,
    force: bool,
    actor: Actor,
) -> Result<Value, String> {
    let (meta, data) = backups.load(id).await?;
    let sctx = crate::sftp::ensure_ctx(sftp, store, &meta.session_id).await?;
    let current = read_remote(&sctx, &meta.path, FILE_MAX).await?;
    let cur_sha = current.as_deref().map(sha256_hex);
    if !force && cur_sha != meta.after_sha256 {
        return Err(format!(
            "{} 在这次改动之后又被修改过（内容哈希不符）；回滚会覆盖后续改动，确认请用 force=true",
            meta.path
        ));
    }
    let rb = backups
        .save(
            &meta.session_id,
            &meta.path,
            "rollback",
            None,
            current.as_deref(),
            data.as_deref(),
        )
        .await?;
    let p = meta.path.clone();
    let res = match data {
        Some(bytes) => {
            crate::sftp::meta(&sctx, move |c| async move { c.overwrite(&p, &bytes).await }).await
        }
        None if current.is_some() => {
            crate::sftp::meta(&sctx, move |c| async move { c.remove_file(&p).await }).await
        }
        None => Ok(()),
    };
    if let Err(e) = res {
        backups.discard(&rb.id).await;
        return Err(e);
    }
    let restored = if meta.existed { "content" } else { "deleted" };
    audit(
        store,
        actor,
        &meta.session_id,
        "file_rollback",
        &json!({
            "backupId": id,
            "path": meta.path,
            "restored": restored,
            "force": force,
            "rollbackBackupId": rb.id,
        }),
    )
    .await;
    Ok(json!({
        "ok": true,
        "path": meta.path,
        "restored": restored,
        "backupId": rb.id,
        "hint": "回滚前的内容也已备份，可再次 file_rollback 该 backupId 撤销回滚",
    }))
}

// ———————————————————————————— UI 命令 ————————————————————————————

#[tauri::command]
pub async fn ai_backups_list(
    session_id: Option<String>,
    backups: tauri::State<'_, Arc<BackupStore>>,
) -> Result<Vec<BackupMeta>, String> {
    Ok(backups.list(session_id.as_deref(), MAX_BACKUPS).await)
}

#[tauri::command]
pub async fn ai_backup_restore(
    backup_id: String,
    force: bool,
    backups: tauri::State<'_, Arc<BackupStore>>,
    sftp: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<Value, String> {
    rollback(
        &sessions.store,
        sftp.inner(),
        &backups,
        &backup_id,
        force,
        Actor::Gui,
    )
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn edit_requires_unique_match() {
        let src = "a=1\nb=1\n";
        let e = apply_edit(src, "=1", "=2", false).unwrap_err();
        assert!(e.contains("2 处"), "{e}");
        let o = apply_edit(src, "=1", "=2", true).unwrap();
        assert_eq!(o.content, "a=2\nb=2\n");
        assert_eq!(o.replacements, 2);
        let o = apply_edit(src, "b=1", "b=3", false).unwrap();
        assert_eq!((o.content.as_str(), o.line), ("a=1\nb=3\n", 2));
        assert!(apply_edit(src, "zzz", "y", false).is_err());
        assert!(apply_edit(src, "a=1", "a=1", false).is_err());
    }

    #[test]
    fn edit_matches_lf_snippet_in_crlf_file() {
        let src = "[a]\r\nx=1\r\ny=2\r\n";
        let o = apply_edit(src, "x=1\ny=2", "x=9\ny=8", false).unwrap();
        assert_eq!(o.content, "[a]\r\nx=9\r\ny=8\r\n", "保留 CRLF 行尾");
    }

    #[test]
    fn view_paginates_with_line_numbers() {
        let text = "l1\nl2\nl3\nl4\n";
        let v = render_view("/f", text, 2, 2, "tag");
        assert!(
            v.starts_with("/f · 第 2-3 行 / 共 4 行 · etag tag\n"),
            "{v}"
        );
        assert!(v.contains("     2\tl2\n     3\tl3\n"));
        assert!(v.contains("offset=4"));
        assert!(!v.contains("l4"));
        assert!(render_view("/f", "", 1, 10, "t").contains("空文件"));
        let long = "x".repeat(LINE_MAX_CHARS + 10);
        assert!(render_view("/f", &long, 1, 1, "t").contains("截断"));
    }

    #[test]
    fn glob_semantics() {
        let g = glob_to_regex("**/*.conf").unwrap();
        assert!(g.is_match("a.conf"));
        assert!(g.is_match("nginx/sites/a.conf"));
        assert!(!g.is_match("a.conf.bak"));
        let g = glob_to_regex("*.{yml,yaml}").unwrap();
        assert!(
            glob_matches(&g, true, "deep/dir/x.yaml"),
            "无斜杠模式按文件名匹配"
        );
        assert!(!glob_matches(&g, true, "x.json"));
        let g = glob_to_regex("conf.d/*.conf").unwrap();
        assert!(glob_matches(&g, false, "conf.d/a.conf"));
        assert!(!glob_matches(&g, false, "conf.d/sub/a.conf"), "* 不跨目录");
        assert!(
            glob_to_regex("a.(b)").unwrap().is_match("a.(b)"),
            "正则元字符按字面"
        );
        assert!(glob_to_regex("{a,b").is_err());
    }

    #[test]
    fn grep_reports_line_numbers_and_caps() {
        let re = Regex::new("err").unwrap();
        let hits = grep_text(&re, "ok\nerr 1\nfine\nerr 2\n", 10);
        assert_eq!(hits, vec![(2, "err 1".into()), (4, "err 2".into())]);
        assert_eq!(grep_text(&re, "err\nerr\nerr\n", 2).len(), 2);
        assert!(looks_binary(b"ab\0c"));
        assert!(!looks_binary(b"plain"));
    }

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("myssh-bk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn backup_roundtrip_list_and_discard() {
        let store = BackupStore::new(temp_root("rt"));
        let a = store
            .save(
                "s1",
                "/etc/a",
                "file_edit",
                Some("改端口"),
                Some(b"old"),
                Some(b"new"),
            )
            .await
            .unwrap();
        let b = store
            .save("s2", "/etc/b", "file_edit", None, None, Some(b"x"))
            .await
            .unwrap();
        let (m, data) = store.load(&a.id).await.unwrap();
        assert_eq!(m, a);
        assert_eq!(m.title.as_deref(), Some("改端口"));
        assert_eq!(
            m.change,
            Some(ChangeSummary::Modified {
                line: 1,
                added: 1,
                removed: 1
            })
        );
        assert_eq!(data.as_deref(), Some(&b"old"[..]));
        assert_eq!(m.after_sha256.as_deref(), Some(sha256_hex(b"new").as_str()));
        let (mb, db) = store.load(&b.id).await.unwrap();
        assert!(!mb.existed);
        assert!(db.is_none(), "新建文件的备份无内容：回滚=删除");
        assert_eq!(store.list(Some("s1"), 10).await, vec![a.clone()]);
        assert_eq!(store.list(None, 10).await.len(), 2);
        store.discard(&a.id).await;
        assert!(store.load(&a.id).await.is_err());
        assert!(
            store.load("../../etc/passwd").await.is_err(),
            "拒绝路径穿越"
        );
    }

    #[tokio::test]
    async fn backup_prunes_oldest_beyond_cap() {
        let store = BackupStore::new(temp_root("prune"));
        let mut first = None;
        for i in 0..MAX_BACKUPS + 3 {
            let m = store
                .save("s", &format!("/f{i}"), "sftp_write", None, Some(b"x"), None)
                .await
                .unwrap();
            first.get_or_insert(m.id);
        }
        let all = store.list(None, MAX_BACKUPS + 10).await;
        assert_eq!(all.len(), MAX_BACKUPS);
        assert_eq!(all[0].path, format!("/f{}", MAX_BACKUPS + 2), "新→旧");
        assert!(
            store.load(first.as_deref().unwrap()).await.is_err(),
            "最旧的被清理"
        );
    }

    #[tokio::test]
    async fn backup_prunes_expired_by_retention_days() {
        let root = temp_root("expire");
        let store = BackupStore::new(root.clone());
        let old = store
            .save("s", "/old", "file_edit", None, Some(b"a"), Some(b"b"))
            .await
            .unwrap();
        // 把这条备份改成 8 天前创建：id 前 13 位即创建时间
        let aged = format!("{:013}{}", now_ms() - 8 * 86_400_000, &old.id[13..]);
        std::fs::rename(
            root.join(format!("{}.json", old.id)),
            root.join(format!("{aged}.json")),
        )
        .unwrap();
        std::fs::rename(
            root.join(format!("{}.bin", old.id)),
            root.join(format!("{aged}.bin")),
        )
        .unwrap();
        let fresh = store
            .save("s", "/new", "file_edit", None, Some(b"a"), Some(b"b"))
            .await
            .unwrap();
        assert_eq!(
            store.list(None, 10).await.len(),
            2,
            "未设保留天数：不按时间过期"
        );
        store.set_retention_days(7);
        assert_eq!(store.prune().await, 1);
        assert!(
            !root.join(format!("{aged}.bin")).exists(),
            "过期备份内容一并删除"
        );
        let left = store.list(None, 10).await;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, fresh.id);
    }

    #[test]
    fn change_summary_describes_edit() {
        let s = |b: Option<&str>, a: Option<&str>| {
            summarize_change(b.map(str::as_bytes), a.map(str::as_bytes))
        };
        assert_eq!(s(None, Some("a\nb\n")), ChangeSummary::Created { lines: 2 });
        assert_eq!(
            s(Some("a\nb\nc\n"), None),
            ChangeSummary::Deleted { lines: 3 }
        );
        assert_eq!(s(Some("x"), Some("x")), ChangeSummary::Unchanged);
        assert_eq!(
            s(Some("a\nb\nc\nd\n"), Some("a\nB\nB2\nc\nd\n")),
            ChangeSummary::Modified {
                line: 2,
                added: 2,
                removed: 1
            },
            "只算中间差异段，公共前后缀不计"
        );
        assert_eq!(
            s(Some("a\nb\n"), Some("a\n")),
            ChangeSummary::Modified {
                line: 2,
                added: 0,
                removed: 1
            }
        );
        assert_eq!(
            summarize_change(Some(b"\0\x01"), Some(b"\0\x02\x03")),
            ChangeSummary::Binary {
                before_bytes: 2,
                after_bytes: 3
            }
        );
        let v = serde_json::to_value(ChangeSummary::Binary {
            before_bytes: 1,
            after_bytes: 2,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "kind": "binary", "beforeBytes": 1, "afterBytes": 2 })
        );
    }

    #[test]
    fn title_is_trimmed_and_capped() {
        assert_eq!(
            title_arg(&json!({ "title": "  改 nginx 端口  " })).as_deref(),
            Some("改 nginx 端口")
        );
        assert_eq!(title_arg(&json!({ "title": "   " })), None);
        assert_eq!(title_arg(&json!({})), None);
        let long = "字".repeat(TITLE_MAX_CHARS + 5);
        assert_eq!(
            title_arg(&json!({ "title": long }))
                .unwrap()
                .chars()
                .count(),
            TITLE_MAX_CHARS
        );
    }

    #[test]
    fn legacy_meta_without_title_or_change_still_loads() {
        let raw = r#"{"id":"0000000000001-0000000000","sessionId":"s","path":"/a","tool":"file_edit","existed":true,"bytes":1,"afterSha256":null,"createdAt":1}"#;
        let m: BackupMeta = serde_json::from_str(raw).unwrap();
        assert!(m.title.is_none() && m.change.is_none());
    }
}
