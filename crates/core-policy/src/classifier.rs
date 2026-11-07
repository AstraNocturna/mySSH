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
            let Some(head) = command_head(sub) else {
                return Verdict::RequireConfirmation;
            };
            if blacklist.contains(&head) {
                return Verdict::RequireConfirmation;
            }
            if INDIRECT_SHELLS.contains(&head) {
                return Verdict::RequireConfirmation;
            }
            if !whitelist.contains(&head) {
                has_unproven = true;
            }
            // 重定向到文件 = 写操作（> / >>）；输入重定向不单独判写
            if has_output_redirect(sub) {
                has_unproven = true;
            }
            // 白名单只认命令名不够：find -delete、systemctl stop、env rm 等
            // 靠参数写系统或执行任意命令，按参数再判一次
            let args = command_args(sub);
            match arg_rule(head, &args) {
                ArgRule::ReadOnly => {}
                ArgRule::Unproven => has_unproven = true,
                ArgRule::Indirect => return Verdict::RequireConfirmation,
            }
        }

        if production && (has_unproven || !all_whitelisted(&subs, &whitelist)) {
            // production：写操作（即任何无法证明只读的操作）强制人工确认，不可绕过
            return Verdict::RequireConfirmation;
        }

        if has_unproven {
            return match level {
                AccessLevel::Readonly => Verdict::Deny,
                AccessLevel::Standard | AccessLevel::Full => Verdict::RequireConfirmation,
            };
        }
        Verdict::Allow
    }
}

fn all_whitelisted(subs: &[String], whitelist: &[&str]) -> bool {
    subs.iter()
        .all(|s| command_head(s).is_some_and(|h| whitelist.contains(&h)))
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
}
