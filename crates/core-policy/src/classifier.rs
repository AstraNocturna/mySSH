//! 命令分类器。解析与判定分离：`split_subcommands` 产出子命令序列，
//! `classify` 按矩阵合成 Verdict。

/// 访问档位（按会话独立配置，默认 Readonly）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    Readonly,
    Standard,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// 需要人工确认（Elicitation 弹窗，展示完整命令与风险说明）
    RequireConfirmation,
    Deny,
}

/// 默认只读白名单（M0；运行时由 core-store 的用户编辑版覆盖）
const READONLY_WHITELIST: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "grep",
    "egrep",
    "fgrep",
    "find",
    "pwd",
    "df",
    "du",
    "ps",
    "top",
    "htop",
    "free",
    "uptime",
    "uname",
    "hostname",
    "whoami",
    "id",
    "date",
    "ip",
    "ss",
    "netstat",
    "lsof",
    "dmesg",
    "journalctl",
    "systemctl",
    "mount",
    "env",
    "printenv",
    "wc",
    "sort",
    "uniq",
    "diff",
    "file",
    "stat",
    "readlink",
    "echo",
    "printf",
    "true",
    "false",
];

/// 全局黑名单：任何档位都强制确认（规格书安全模型第 2 条）
const BLACKLIST: &[&str] = &[
    "rm",
    "mkfs",
    "dd",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "iptables",
    "useradd",
    "userdel",
    "usermod",
    "passwd",
    "chpasswd",
    "visudo",
    "chmod",
    "chown",
    "chattr",
    "yum",
    "apt",
    "apt-get",
    "dnf",
    "zypper",
    "pacman",
    "snap",
    "systemctl-danger",
];

/// 间接执行器：出现即无法静态判定，fail-closed
const INDIRECT_SHELLS: &[&str] = &[
    "bash", "sh", "dash", "zsh", "ksh", "fish", "eval", "exec", "xargs", "nohup", "sudo", "su",
    "doas", "python", "python3", "perl", "ruby", "node", "awk", "sed",
];

#[derive(Default)]
pub struct PolicyEngine {
    whitelist: Vec<String>,
    blacklist: Vec<String>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入用户编辑后的白/黑名单（core-store 加载；M6 接线）
    pub fn with_lists(whitelist: Vec<String>, blacklist: Vec<String>) -> Self {
        Self {
            whitelist,
            blacklist,
        }
    }

    fn whitelist(&self) -> Vec<&str> {
        if self.whitelist.is_empty() {
            READONLY_WHITELIST.to_vec()
        } else {
            self.whitelist.iter().map(String::as_str).collect()
        }
    }

    fn blacklist(&self) -> Vec<&str> {
        if self.blacklist.is_empty() {
            BLACKLIST.to_vec()
        } else {
            self.blacklist.iter().map(String::as_str).collect()
        }
    }

    pub fn classify(&self, cmd: &str, level: AccessLevel, production: bool) -> Verdict {
        let parsed = split_subcommands(cmd);
        let subs = match parsed {
            Ok(s) if !s.is_empty() => s,
            // 空命令不执行任何事；解析失败 → fail-closed
            Ok(_) => return Verdict::Deny,
            Err(_) => return Verdict::RequireConfirmation,
        };

        let whitelist = self.whitelist();
        let blacklist = self.blacklist();

        let mut has_unproven = false;
        for sub in &subs {
            match sub_risk(sub, &whitelist, &blacklist) {
                PartRisk::Unparsed | PartRisk::Blacklist | PartRisk::Indirect => {
                    return Verdict::RequireConfirmation
                }
                PartRisk::Write => has_unproven = true,
                PartRisk::ReadOnly => {}
            }
        }

        if production && has_unproven {
            // production：写操作（即任何无法证明只读的操作）强制人工确认，不可绕过
            return Verdict::RequireConfirmation;
        }

        if has_unproven {
            return match level {
                AccessLevel::Readonly => Verdict::Deny,
                AccessLevel::Standard => Verdict::RequireConfirmation,
                // full：普通写操作放行；黑名单/间接执行/解析失败/生产会话已在上面强制确认
                AccessLevel::Full => Verdict::Allow,
            };
        }
        Verdict::Allow
    }

    /// 审批展示用：按顶层 `| || && ; &` 与换行把复合命令切成原文片段，逐段给出
    /// 主命令名、写入的文件与风险（与 `classify` 同一单段判定）。heredoc 正文是数据，
    /// 不切成命令。结构无法解析时整条作为一段 Unparsed。
    pub fn explain(&self, cmd: &str) -> Vec<CommandPart> {
        let whitelist = self.whitelist();
        let blacklist = self.blacklist();
        let Ok(raw) = split_top_level(cmd) else {
            return vec![CommandPart {
                text: cmd.trim().to_string(),
                risk: PartRisk::Unparsed,
                ..CommandPart::default()
            }];
        };
        raw.into_iter()
            .map(|p| {
                let mut risk = match split_subcommands(&p.scan) {
                    Ok(subs) => subs
                        .iter()
                        .map(|s| sub_risk(s, &whitelist, &blacklist))
                        .max()
                        .unwrap_or(PartRisk::ReadOnly),
                    Err(()) => PartRisk::Unparsed,
                };
                // 不加引号的 heredoc 会展开 $( ) / 反引号：正文里可能藏命令
                if p.heredoc_expands {
                    risk = risk.max(PartRisk::Unparsed);
                }
                let words = analyze_words(&shell_words(&p.text));
                CommandPart {
                    text: p.text,
                    op: p.op,
                    head: words.head,
                    sub: words.sub,
                    wrapper: words.wrapper,
                    writes: words.writes,
                    append: words.append,
                    heredoc_lines: p.heredoc_lines,
                    risk,
                }
            })
            .collect()
    }
}

/// 复合命令中的一段（审批弹窗据此生成总说明）
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandPart {
    /// 原文（保留引号与替换，不含 heredoc 正文），已去首尾空白
    pub text: String,
    /// 与下一段之间的连接符：`|` `||` `&&` `;` `&`；末段为 None
    pub op: Option<String>,
    /// 主命令名（剥掉环境变量赋值、路径前缀与 sudo 等前缀）
    pub head: Option<String>,
    /// 主命令名后的第一个非选项参数（如 systemctl restart 的 restart）
    pub sub: Option<String>,
    /// 包在主命令外的前缀命令：sudo / nohup / nice / time / timeout / stdbuf
    pub wrapper: Option<String>,
    /// 输出重定向写入的文件（第一个，忽略 /dev/null 与 `>&` 复制描述符）
    pub writes: Option<String>,
    /// `>>` 追加写
    pub append: bool,
    /// heredoc 正文行数（无 heredoc 为 0）
    pub heredoc_lines: usize,
    pub risk: PartRisk,
}

/// 单段风险，按严重程度升序（取 max 即该段最高风险）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PartRisk {
    /// 可证明只读
    #[default]
    ReadOnly,
    /// 无法证明只读：白名单外命令、输出重定向、写系统的参数
    Write,
    /// 结构无法静态判定（子 shell、不配对的引号等）
    Unparsed,
    /// 间接执行：bash -c / sudo / xargs / find -exec 等
    Indirect,
    /// 命中黑名单
    Blacklist,
}

impl PartRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            PartRisk::ReadOnly => "readonly",
            PartRisk::Write => "write",
            PartRisk::Unparsed => "unparsed",
            PartRisk::Indirect => "indirect",
            PartRisk::Blacklist => "blacklist",
        }
    }
}

fn sub_risk(sub: &str, whitelist: &[&str], blacklist: &[&str]) -> PartRisk {
    let Some(head) = command_head(sub) else {
        return PartRisk::Unparsed;
    };
    if blacklist.contains(&head) {
        return PartRisk::Blacklist;
    }
    if INDIRECT_SHELLS.contains(&head) {
        return PartRisk::Indirect;
    }
    // 白名单只认命令名不够：find -delete、systemctl stop、env rm 等
    // 靠参数写系统或执行任意命令，按参数再判一次
    let rule = arg_rule(head, &command_args(sub));
    if rule == ArgRule::Indirect {
        return PartRisk::Indirect;
    }
    // 重定向到文件 = 写操作（> / >>）；输入重定向不单独判写
    if !whitelist.contains(&head) || has_output_redirect(sub) || rule == ArgRule::Unproven {
        PartRisk::Write
    } else {
        PartRisk::ReadOnly
    }
}

#[derive(Default)]
struct RawPart {
    /// 展示原文
    text: String,
    /// 判风险用的文本：去掉 heredoc 操作符与定界符（它们不是命令）
    scan: String,
    op: Option<String>,
    heredoc_lines: usize,
    heredoc_expands: bool,
}

struct Heredoc {
    delim: String,
    strip_tabs: bool,
    quoted: bool,
    part: usize,
}

fn push_part(cur: &mut RawPart, op: Option<String>, out: &mut Vec<RawPart>) {
    let mut part = std::mem::take(cur);
    part.text = part.text.trim().to_string();
    if !part.text.is_empty() {
        part.op = op;
        out.push(part);
    } else if let (Some(op), Some(last)) = (op, out.last_mut()) {
        // 空段（如 `a ;; b`）：连接符并给上一段
        last.op = Some(op);
    }
}

/// `<<` 之后：可选 `-`、空白、定界符（可带引号/反斜杠 = 正文不展开）。
/// 原文照录进 text，不进 scan。定界符为空 → Err。
fn read_heredoc_op(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    cur: &mut RawPart,
    part: usize,
) -> Result<Heredoc, ()> {
    cur.text.push_str("<<");
    let strip_tabs = chars.peek() == Some(&'-');
    if strip_tabs {
        chars.next();
        cur.text.push('-');
    }
    while let Some(&c) = chars.peek() {
        if c != ' ' && c != '\t' {
            break;
        }
        cur.text.push(c);
        chars.next();
    }
    let (mut delim, mut quoted) = (String::new(), false);
    while let Some(&c) = chars.peek() {
        match c {
            '\'' | '"' => {
                chars.next();
                cur.text.push(c);
                quoted = true;
                for q in chars.by_ref() {
                    cur.text.push(q);
                    if q == c {
                        break;
                    }
                    delim.push(q);
                }
            }
            '\\' => {
                chars.next();
                cur.text.push(c);
                quoted = true;
                if let Some(n) = chars.next() {
                    cur.text.push(n);
                    delim.push(n);
                }
            }
            c if c.is_whitespace() || ";|&<>()".contains(c) => break,
            _ => {
                chars.next();
                cur.text.push(c);
                delim.push(c);
            }
        }
    }
    if delim.is_empty() {
        return Err(());
    }
    Ok(Heredoc {
        delim,
        strip_tabs,
        quoted,
        part,
    })
}

/// 换行后读掉 heredoc 正文（直到定界符行或输入结束），行数与是否会展开记到所属段
fn read_heredoc_body(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    h: &Heredoc,
    out: &mut [RawPart],
) {
    let (mut lines, mut expands) = (0usize, false);
    loop {
        if chars.peek().is_none() {
            break;
        }
        let mut line = String::new();
        for c in chars.by_ref() {
            if c == '\n' {
                break;
            }
            line.push(c);
        }
        let cmp = if h.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line.as_str()
        };
        if cmp.trim_end_matches('\r') == h.delim {
            break;
        }
        lines += 1;
        if !h.quoted && (line.contains("$(") || line.contains('`')) {
            expands = true;
        }
    }
    if let Some(p) = out.get_mut(h.part) {
        p.heredoc_lines += lines;
        p.heredoc_expands |= expands;
    }
}

/// 只按顶层连接符切分并保留原文：引号、`$( )`、反引号内不切；
/// `2>&1`、`&>` 里的 `&` 属于重定向；heredoc 正文整体跳过。引号/替换不闭合 → Err。
fn split_top_level(cmd: &str) -> Result<Vec<RawPart>, ()> {
    let mut out: Vec<RawPart> = Vec::new();
    let mut cur = RawPart::default();
    let mut chars = cmd.chars().peekable();
    let (mut in_single, mut in_double, mut in_tick) = (false, false, false);
    let mut depth = 0usize;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    while let Some(c) = chars.next() {
        let quoted = in_single || in_double || in_tick || depth > 0;
        match c {
            '\\' if !in_single => {
                let n = chars.next().ok_or(())?;
                for x in [c, n] {
                    cur.text.push(x);
                    cur.scan.push(x);
                }
                continue;
            }
            '\'' if !in_double && !in_tick && depth == 0 => in_single = !in_single,
            '"' if !in_single && !in_tick && depth == 0 => in_double = !in_double,
            '`' if !in_single => in_tick = !in_tick,
            '$' if !in_single && chars.peek() == Some(&'(') => {
                chars.next();
                cur.text.push_str("$(");
                cur.scan.push_str("$(");
                depth += 1;
                continue;
            }
            '(' if depth > 0 && !in_single => depth += 1,
            ')' if depth > 0 && !in_single => depth -= 1,
            '<' if !quoted && chars.peek() == Some(&'<') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    // here-string `<<<`：后面是普通单词
                    chars.next();
                    cur.text.push_str("<<<");
                    cur.scan.push_str("<<<");
                    continue;
                }
                heredocs.push(read_heredoc_op(&mut chars, &mut cur, out.len())?);
                continue;
            }
            '|' | '&' | ';' | '\n' if !quoted => {
                let prev = cur.text.chars().last();
                if c == '&' && (prev == Some('>') || chars.peek() == Some(&'>')) {
                    cur.text.push(c);
                    cur.scan.push(c);
                    continue;
                }
                let op = match (c, chars.peek()) {
                    ('|', Some('|')) | ('&', Some('&')) => {
                        chars.next();
                        format!("{c}{c}")
                    }
                    ('|', Some('&')) => {
                        chars.next();
                        "|".to_string()
                    }
                    ('\n', _) => ";".to_string(),
                    _ => c.to_string(),
                };
                push_part(&mut cur, Some(op), &mut out);
                if c == '\n' {
                    for h in heredocs.drain(..) {
                        read_heredoc_body(&mut chars, &h, &mut out);
                    }
                }
                continue;
            }
            _ => {}
        }
        cur.text.push(c);
        cur.scan.push(c);
    }
    if in_single || in_double || in_tick || depth > 0 {
        return Err(());
    }
    push_part(&mut cur, None, &mut out);
    // 末尾的 `;` 不再连接下一段（`&` 表示后台运行，保留）
    if let Some(last) = out.last_mut() {
        if last.op.as_deref() == Some(";") {
            last.op = None;
        }
    }
    Ok(out)
}

/// 按 shell 规则切单词并去掉引号；bool = 单词含引号/转义（不能再当操作符解读）
fn shell_words(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let (mut word, mut quoted, mut started) = (String::new(), false, false);
    let (mut in_single, mut in_double) = (false, false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                quoted = true;
                started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                quoted = true;
                started = true;
            }
            '\\' if !in_single => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
                quoted = true;
                started = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if started {
                    out.push((std::mem::take(&mut word), quoted));
                }
                quoted = false;
                started = false;
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push((word, quoted));
    }
    out
}

/// 包在真正命令外面的前缀命令
const WRAPPERS: &[&str] = &["sudo", "nohup", "nice", "time", "timeout", "stdbuf"];
/// 前缀命令里带取值的选项（取值是下一个单词）
const WRAPPER_VALUE_OPTS: &[&str] = &["-u", "-g", "-C", "-h", "-p", "-n", "-s", "-k"];

#[derive(Default)]
struct Words {
    head: Option<String>,
    sub: Option<String>,
    wrapper: Option<String>,
    writes: Option<String>,
    append: bool,
}

/// 单词是重定向时返回 (是否输出, 是否追加, 内联目标, 目标是否在下一个单词)
fn redirect_of(word: &str) -> Option<(bool, bool, Option<String>, bool)> {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    if let Some(r) = rest.strip_prefix("<<") {
        // heredoc / here-string：`<< EOF` 时定界符在下一个单词
        let r = r.trim_start_matches(['<', '-']);
        return Some((false, false, None, r.is_empty()));
    }
    if let Some(r) = rest.strip_prefix('<') {
        return Some((false, false, None, r.is_empty()));
    }
    let r = rest.strip_prefix('>')?;
    let append = r.starts_with('>');
    let r = r.trim_start_matches(['>', '|']);
    if r.starts_with('&') {
        // >&2：复制描述符，不写文件
        return Some((false, false, None, false));
    }
    if r.is_empty() {
        Some((true, append, None, true))
    } else {
        Some((true, append, Some(r.to_string()), false))
    }
}

fn analyze_words(words: &[(String, bool)]) -> Words {
    let mut w = Words::default();
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let (word, quoted) = (&words[i].0, words[i].1);
        let redirect = if quoted { None } else { redirect_of(word) };
        let Some((output, append, inline, needs_next)) = redirect else {
            positional.push(word);
            i += 1;
            continue;
        };
        let target = inline.or_else(|| {
            needs_next
                .then(|| words.get(i + 1).map(|x| x.0.clone()))
                .flatten()
        });
        if let (true, Some(t), None) = (output, target, &w.writes) {
            if t != "/dev/null" {
                w.writes = Some(t);
                w.append = append;
            }
        }
        i += if needs_next { 2 } else { 1 };
    }
    let mut rest = positional.into_iter().peekable();
    // 前导环境变量赋值 A=1 B=2 cmd
    while rest.peek().is_some_and(|t| {
        t.split_once('=')
            .is_some_and(|(k, _)| is_ident(k) && !t.starts_with('/'))
    }) {
        rest.next();
    }
    let base = |t: &str| t.rsplit('/').next().unwrap_or(t).to_string();
    let mut head = rest.next().map(base);
    if let Some(h) = head.clone().filter(|h| WRAPPERS.contains(&h.as_str())) {
        while let Some(t) = rest.peek() {
            if !t.starts_with('-') {
                break;
            }
            let takes_value = WRAPPER_VALUE_OPTS.contains(t);
            rest.next();
            if takes_value {
                rest.next();
            }
        }
        if h == "timeout" {
            rest.next(); // 时长
        }
        if let Some(inner) = rest.next() {
            w.wrapper = Some(h);
            head = Some(base(inner));
        }
    }
    w.head = head.filter(|h| !h.is_empty());
    w.sub = rest.find(|t| !t.starts_with('-')).map(str::to_string);
    w
}

/// 提取子命令的「主命令名」：剥掉前导环境变量赋值（A=1 B=2 cmd …）与路径前缀。
/// 返回 None 表示结构无法判定（调用方按危险处理）。
fn command_head(sub: &str) -> Option<&str> {
    split_command(sub).map(|(head, _)| head)
}

/// 主命令名之后的参数（不含重定向占位符 `>`）
fn command_args(sub: &str) -> Vec<&str> {
    split_command(sub).map(|(_, args)| args).unwrap_or_default()
}

fn split_command(sub: &str) -> Option<(&str, Vec<&str>)> {
    let mut tokens = sub.split_whitespace();
    let mut head = tokens.next()?;
    while head.contains('=')
        && !head.starts_with('/')
        && head.split('=').next().is_some_and(is_ident)
    {
        head = tokens.next()?;
    }
    // /bin/rm → rm；/usr/bin/systemctl → systemctl
    let name = head.rsplit('/').next()?;
    if name.is_empty() {
        return None;
    }
    Some((name, tokens.filter(|t| *t != ">").collect()))
}

/// 参数级判定结果
#[derive(Debug, PartialEq, Eq)]
enum ArgRule {
    ReadOnly,
    /// 可能写系统：按「无法证明只读」处理
    Unproven,
    /// 把参数当命令执行：与间接执行器同级
    Indirect,
}

/// 白名单命令中靠参数即可写系统或执行任意命令的逐个收紧；规则按命令名生效，
/// 与白名单来源（内置/用户编辑）无关。宁严勿漏：认不出的写法按 Unproven。
fn arg_rule(head: &str, args: &[&str]) -> ArgRule {
    let has = |flags: &[&str]| {
        args.iter().any(|a| {
            flags.iter().any(|f| {
                a == f
                    // --opt=value 与短选项粘连值（-ofile）
                    || (f.starts_with("--") && a.starts_with(&format!("{f}=")))
                    || (f.len() == 2 && !f.starts_with("--") && a.starts_with(f) && !a.starts_with("--"))
            })
        })
    };
    // 短选项合并写法：sort -nro out 里的 o、ss -tnK 里的 K
    let in_cluster = |c: char| {
        args.iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains(c))
    };
    let positionals = || args.iter().filter(|a| !a.starts_with('-')).count();
    let unproven_if = |cond: bool| {
        if cond {
            ArgRule::Unproven
        } else {
            ArgRule::ReadOnly
        }
    };
    match head {
        // env CMD / env -S "CMD" 执行任意命令；只打印环境时参数只有选项与 A=1 赋值
        "env" => {
            let runs_cmd = args.iter().any(|a| !a.starts_with('-') && !a.contains('='));
            if runs_cmd || has(&["-S", "--split-string"]) {
                ArgRule::Indirect
            } else {
                ArgRule::ReadOnly
            }
        }
        "find" => {
            if has(&["-exec", "-execdir", "-ok", "-okdir"]) {
                ArgRule::Indirect
            } else {
                unproven_if(has(&["-delete", "-fprint", "-fprint0", "-fprintf", "-fls"]))
            }
        }
        // 只放行查询类子命令；start/stop/restart/enable/mask/kill 等一律不可证明只读
        "systemctl" => {
            const READ: &[&str] = &[
                "status",
                "show",
                "cat",
                "help",
                "list-units",
                "list-unit-files",
                "list-timers",
                "list-sockets",
                "list-dependencies",
                "list-jobs",
                "list-machines",
                "is-active",
                "is-enabled",
                "is-failed",
                "is-system-running",
                "show-environment",
            ];
            match args.iter().find(|a| !a.starts_with('-')) {
                None => ArgRule::ReadOnly,
                Some(sub) => unproven_if(!READ.contains(sub)),
            }
        }
        "journalctl" => unproven_if(has(&[
            "--vacuum-size",
            "--vacuum-time",
            "--vacuum-files",
            "--rotate",
            "--flush",
            "--sync",
            "--relinquish-var",
            "--smart-relinquish-var",
            "--setup-keys",
            "--update-catalog",
        ])),
        "dmesg" => unproven_if(has(&[
            "-C",
            "--clear",
            "-c",
            "--read-clear",
            "-n",
            "--console-level",
            "-D",
            "--console-off",
            "-E",
            "--console-on",
        ])),
        // date -s 与 date MMDDhhmm 都会改系统时间；只放行 +FORMAT 与 -d/-r 的取值
        "date" => {
            let mut sets = false;
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if matches!(*a, "-d" | "--date" | "-r" | "--reference") {
                    it.next();
                } else if !a.starts_with('-') && !a.starts_with('+') {
                    sets = true;
                }
            }
            unproven_if(sets || has(&["-s", "--set"]))
        }
        // hostname NAME / -F FILE / -b 会改主机名
        "hostname" => unproven_if(positionals() > 0 || has(&["-F", "--file", "-b", "--boot"])),
        "sort" => {
            if has(&["--compress-program"]) {
                ArgRule::Indirect
            } else {
                unproven_if(in_cluster('o') || has(&["--output"]))
            }
        }
        // uniq INPUT OUTPUT：第二个位置参数是输出文件
        "uniq" => unproven_if(positionals() > 1),
        "file" => unproven_if(has(&["-C", "--compile"])),
        "ss" => unproven_if(in_cluster('K') || has(&["--kill"])),
        // 只放行裸 mount 或 -l 列举；带位置参数即挂载
        "mount" => unproven_if(args.iter().any(|a| *a != "-l" && *a != "--show-labels")),
        "ip" => ip_rule(args),
        _ => ArgRule::ReadOnly,
    }
}

/// ip [选项] OBJECT [COMMAND …]：COMMAND 缺省为 show；只认完整的查询词。
/// iproute2 允许前缀缩写（ip l s = link set），缩写一律按 Unproven。
fn ip_rule(args: &[&str]) -> ArgRule {
    const READ: &[&str] = &["show", "list", "lst", "get", "help"];
    let mut rest = args.iter().skip_while(|a| a.starts_with('-'));
    let object = rest.next();
    if object.is_some_and(|o| *o == "netns" || o.starts_with("netn")) && args.contains(&"exec") {
        return ArgRule::Indirect;
    }
    match rest.next() {
        None => ArgRule::ReadOnly,
        Some(cmd) if READ.contains(cmd) => ArgRule::ReadOnly,
        Some(_) => ArgRule::Unproven,
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn has_output_redirect(sub: &str) -> bool {
    // M0 近似：> 或 >> 且不紧跟描述符合并（>&2 之类不算写文件）
    let bytes = sub.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'>' {
            let next = bytes.get(i + 1).copied();
            if next == Some(b'&') {
                continue;
            }
            return true;
        }
    }
    false
}

/// 把复合命令拆成子命令序列：按 | ; && || 与换行切分，尊重单/双引号；
/// $(…) 与反引号内层命令递归并入序列（黑名单检查必须穿透命令替换）。
/// 引号/括号不配对 → Err（fail-closed）。
/// M0 近似：$() 内层扫描不处理嵌套引号——结构异常一律 Err，宁严勿漏。
fn split_subcommands(cmd: &str) -> Result<Vec<String>, ()> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                let next = chars.next().ok_or(())?;
                cur.push(next);
            }
            '$' if !in_single && chars.peek() == Some(&'(') => {
                chars.next(); // 消费 '('
                let mut inner = String::new();
                let mut depth = 1;
                let mut closed = false;
                for c2 in chars.by_ref() {
                    match c2 {
                        '(' => {
                            depth += 1;
                            inner.push(c2);
                        }
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                closed = true;
                                break;
                            }
                            inner.push(c2);
                        }
                        _ => inner.push(c2),
                    }
                }
                if !closed {
                    return Err(());
                }
                // 内层命令递归并入判定序列
                out.extend(split_subcommands(&inner)?);
                cur.push('x'); // 占位：保持外层命令结构（如 echo $(…) → echo x）
            }
            '`' if !in_single => {
                let mut inner = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '`' {
                        closed = true;
                        break;
                    }
                    inner.push(c2);
                }
                if !closed {
                    return Err(());
                }
                out.extend(split_subcommands(&inner)?);
            }
            '(' | ')' if !in_single && !in_double => {
                return Err(()); // 子shell/括号结构，M0 不静态判定
            }
            '|' | ';' | '&' | '\n' | '<' | '>' if !in_single && !in_double => {
                // 双字符操作符吞掉第二个字符
                if (c == '|' || c == '&' || c == '>') && chars.peek() == Some(&c) {
                    chars.next();
                }
                if c == '>' {
                    cur.push('>'); // 保留给 has_output_redirect 判定
                }
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            _ => cur.push(c),
        }
    }
    if in_single || in_double {
        return Err(());
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn splits_pipes_and_semicolons() {
        let subs = split_subcommands("ls -la | grep foo; df -h").unwrap();
        assert_eq!(subs.len(), 3);
        assert_eq!(command_head(&subs[0]), Some("ls"));
        assert_eq!(command_head(&subs[2]), Some("df"));
    }

    #[test]
    fn command_substitution_is_traversed() {
        let subs = split_subcommands("echo $(rm -rf /)").unwrap();
        assert!(subs.iter().any(|s| command_head(s) == Some("rm")));
    }

    #[test]
    fn unbalanced_quotes_fail_closed() {
        assert!(split_subcommands("echo 'oops").is_err());
    }

    #[test]
    fn env_prefix_is_stripped() {
        assert_eq!(command_head("A=1 B=2 rm -rf /"), Some("rm"));
        assert_eq!(command_head("/bin/rm -rf /"), Some("rm"));
    }

    fn parts(cmd: &str) -> Vec<(String, Option<String>, PartRisk)> {
        PolicyEngine::new()
            .explain(cmd)
            .into_iter()
            .map(|p| (p.text, p.op, p.risk))
            .collect()
    }

    #[test]
    fn explain_splits_compound_command_keeping_raw_text() {
        let got = PolicyEngine::new()
            .explain("cd /app && git pull | tee 'a b.log'; systemctl restart nginx 2>&1 &");
        let texts: Vec<_> = got.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "cd /app",
                "git pull",
                "tee 'a b.log'",
                "systemctl restart nginx 2>&1"
            ]
        );
        let ops: Vec<_> = got.iter().map(|p| p.op.as_deref()).collect();
        assert_eq!(ops, [Some("&&"), Some("|"), Some(";"), Some("&")]);
        assert_eq!(got[3].head.as_deref(), Some("systemctl"));
        assert_eq!(got[3].sub.as_deref(), Some("restart"));
        assert!(got.iter().all(|p| p.risk == PartRisk::Write));
    }

    #[test]
    fn explain_risk_per_part() {
        assert_eq!(
            parts("ls -la | grep foo || df -h"),
            [
                ("ls -la".into(), Some("|".into()), PartRisk::ReadOnly),
                ("grep foo".into(), Some("||".into()), PartRisk::ReadOnly),
                ("df -h".into(), None, PartRisk::ReadOnly),
            ]
        );
        // 命令替换不切开，但内层黑名单命令计入该段风险
        assert_eq!(
            parts("echo \"$(rm -rf /)\"; uptime"),
            [
                (
                    "echo \"$(rm -rf /)\"".into(),
                    Some(";".into()),
                    PartRisk::Blacklist
                ),
                ("uptime".into(), None, PartRisk::ReadOnly),
            ]
        );
        assert_eq!(parts("sudo ls")[0].2, PartRisk::Indirect);
        assert_eq!(
            parts("find / -name x -exec cat {} \\;")[0].2,
            PartRisk::Indirect
        );
        assert_eq!(parts("cat a > b")[0].2, PartRisk::Write);
        // 引号中的连接符不切
        assert_eq!(parts("grep 'a|b;c' f").len(), 1);
        // 结构无法解析：整条一段
        assert_eq!(
            parts("echo 'oops && rm x"),
            [("echo 'oops && rm x".into(), None, PartRisk::Unparsed)]
        );
    }

    /// 截图里的真实用例：heredoc 写文件后 ls && cat。正文是数据，不能被切成命令
    #[test]
    fn explain_heredoc_body_is_data() {
        let cmd = "cat > '/root/99乘法口诀.txt' <<'EOF'\n1x1=1\n1x2=2 2x2=4\n1x3=3 2x3=6 3x3=9\nEOF\nls -l '/root/99乘法口诀.txt' && cat '/root/99乘法口诀.txt'";
        let got = PolicyEngine::new().explain(cmd);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].text, "cat > '/root/99乘法口诀.txt' <<'EOF'");
        assert_eq!(got[0].head.as_deref(), Some("cat"));
        assert_eq!(got[0].writes.as_deref(), Some("/root/99乘法口诀.txt"));
        assert!(!got[0].append);
        assert_eq!(got[0].heredoc_lines, 3);
        assert_eq!(got[0].risk, PartRisk::Write);
        assert_eq!(got[0].op.as_deref(), Some(";"));
        assert_eq!(got[1].text, "ls -l '/root/99乘法口诀.txt'");
        assert_eq!(got[1].risk, PartRisk::ReadOnly);
        assert_eq!(got[1].op.as_deref(), Some("&&"));
        assert_eq!(got[2].head.as_deref(), Some("cat"));
        assert_eq!(got[2].writes, None);
        assert_eq!(got[2].risk, PartRisk::ReadOnly);
    }

    #[test]
    fn explain_heredoc_variants() {
        // `<< EOF` 带空格、`<<-` 去前导 tab；只读输出不算写
        let got = PolicyEngine::new().explain("cat << EOF\nrm -rf /\nEOF\nuptime");
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].heredoc_lines, 1);
        assert_eq!(got[0].writes, None);
        assert_eq!(got[1].head.as_deref(), Some("uptime"));
        let got = PolicyEngine::new().explain("cat <<-END > /tmp/a\n\tx\n\tEND\ndf -h");
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].writes.as_deref(), Some("/tmp/a"));
        // 不加引号的 heredoc 会展开命令替换：按无法静态判定提示
        let got = PolicyEngine::new().explain("cat > /tmp/b <<EOF\n$(rm -rf /)\nEOF");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].risk, PartRisk::Unparsed);
        // 定界符缺失 → 整条无法解析
        assert_eq!(parts("cat <<\nx")[0].2, PartRisk::Unparsed);
    }

    #[test]
    fn explain_writes_and_wrappers() {
        let e = PolicyEngine::new();
        let p = &e.explain("echo hi >> /var/log/x.log 2>/dev/null")[0];
        assert_eq!(p.writes.as_deref(), Some("/var/log/x.log"));
        assert!(p.append);
        let p = &e.explain("ls 2>/dev/null >/tmp/out")[0];
        assert_eq!(p.writes.as_deref(), Some("/tmp/out"));
        let p = &e.explain("grep x f 2>&1")[0];
        assert_eq!(p.writes, None);
        // 引号里的 > 不是重定向
        let p = &e.explain("echo '>' x")[0];
        assert_eq!(p.writes, None);
        let p = &e.explain("sudo -u www systemctl restart nginx")[0];
        assert_eq!(p.wrapper.as_deref(), Some("sudo"));
        assert_eq!(p.head.as_deref(), Some("systemctl"));
        assert_eq!(p.sub.as_deref(), Some("restart"));
        assert_eq!(p.risk, PartRisk::Indirect);
        let p = &e.explain("LANG=C timeout 5 /usr/bin/ping -c 1 host")[0];
        assert_eq!(p.wrapper.as_deref(), Some("timeout"));
        assert_eq!(p.head.as_deref(), Some("ping"));
    }
    /// 逐段风险与整条判定一致：standard 档放行 ⇔ 每段都只读；确认 ⇔ 存在非只读段
    #[test]
    fn explain_agrees_with_classify() {
        let engine = PolicyEngine::new();
        for cmd in [
            "ls | wc -l",
            "uptime && df -h; free -m",
            "systemctl status nginx | head",
            "systemctl restart nginx && systemctl status nginx",
            "rm -rf /tmp/x; ls",
            "echo $(whoami) > /tmp/me",
            "bash -c 'ls'",
            "journalctl -u x --vacuum-time=1d",
            "ip addr show && ip link set eth0 down",
        ] {
            let all_ro = engine
                .explain(cmd)
                .iter()
                .all(|p| p.risk == PartRisk::ReadOnly);
            let verdict = engine.classify(cmd, AccessLevel::Standard, false);
            assert_eq!(
                verdict == Verdict::Allow,
                all_ro,
                "{cmd}: {verdict:?} vs {:?}",
                engine.explain(cmd)
            );
        }
    }
}
