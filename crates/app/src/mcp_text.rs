//! 模型上下文的文本瘦身：ANSI/控制序列剥离、回车覆盖行折叠。
//!
//! 按字节流增量处理：非末块时，末尾不完整的转义序列 / UTF-8 字符 / 孤立 \r 不消费，
//! 返回已消费字节数，调用方据此推进 offset，下次读取从断点续上，不会吐出半截 `[0m`。

use std::collections::VecDeque;

/// 剥离 ANSI 序列与控制字符。返回 (文本, 已消费输入字节数)。
/// `final_chunk=true` 表示输入已到流尾（进程退出/终端关闭），尾部残缺序列直接丢弃。
pub(crate) fn strip_ansi(input: &[u8], final_chunk: bool) -> (String, usize) {
    let n = if final_chunk {
        input.len()
    } else {
        utf8_safe_end(input)
    };
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut consumed = n;
    let mut i = 0;
    while i < n {
        let b = input[i];
        match b {
            0x1b => match escape_len(&input[i..n]) {
                Some(len) => {
                    i += len;
                    continue;
                }
                None if final_chunk => break,
                None => {
                    consumed = i;
                    break;
                }
            },
            b'\r' => {
                if i + 1 < n && input[i + 1] == b'\n' {
                    i += 1;
                    continue;
                }
                if i + 1 == n && !final_chunk {
                    // 可能是 \r\n 被切开：留到下次
                    consumed = i;
                    break;
                }
                // 孤立 \r（进度条刷新）：丢弃当前行已输出内容
                let ls = out.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
                out.truncate(ls);
            }
            0x08 => pop_char(&mut out),
            b'\n' | b'\t' => out.push(b),
            0x00..=0x1f | 0x7f => {}
            _ => out.push(b),
        }
        i += 1;
    }
    (String::from_utf8_lossy(&out).into_owned(), consumed)
}

/// 分页读取用的剥离：整段都是残缺序列（页过小）时按流尾处理，保证游标前进
pub(crate) fn strip_ansi_page(raw: &[u8], at_end: bool) -> (String, usize) {
    match strip_ansi(raw, at_end) {
        (_, 0) if !raw.is_empty() => strip_ansi(raw, true),
        r => r,
    }
}

/// 有界输出收集：超限时保留头尾各一半、丢中间（报错与汇总通常在末尾）
pub(crate) struct HeadTail {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    half: usize,
    omitted: u64,
}

impl HeadTail {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            half: cap / 2,
            omitted: 0,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        let room = self.half.saturating_sub(self.head.len()).min(data.len());
        self.head.extend_from_slice(&data[..room]);
        self.tail.extend(&data[room..]);
        if self.tail.len() > self.half {
            let drop = self.tail.len() - self.half;
            self.tail.drain(..drop);
            self.omitted += drop as u64;
        }
    }

    pub(crate) fn omitted(&self) -> u64 {
        self.omitted
    }

    pub(crate) fn render(&self, strip: bool) -> String {
        let conv = |b: &[u8]| {
            if strip {
                strip_ansi(b, true).0
            } else {
                String::from_utf8_lossy(b).into_owned()
            }
        };
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if self.omitted == 0 {
            let mut all = self.head.clone();
            all.extend_from_slice(&tail);
            return conv(&all);
        }
        // 切点可能落在多字节字符中间：头段去掉残缺尾字符，尾段跳过续字节
        let head = &self.head[..utf8_safe_end(&self.head)];
        let skip = tail.iter().take_while(|&&c| c & 0xC0 == 0x80).count();
        format!(
            "{}\n…[中间省略 {} 字节]…\n{}",
            conv(head),
            self.omitted,
            conv(&tail[skip..])
        )
    }
}
/// 退格：删掉上一个字符（不跨行；多字节 UTF-8 整体删除）
fn pop_char(out: &mut Vec<u8>) {
    if out.last().is_none_or(|&c| c == b'\n') {
        return;
    }
    while let Some(c) = out.pop() {
        if c & 0xC0 != 0x80 {
            break;
        }
    }
}

/// 输入末尾若是不完整的 UTF-8 多字节字符，返回其起点（否则返回 len）
fn utf8_safe_end(input: &[u8]) -> usize {
    let len = input.len();
    for back in 1..=3.min(len) {
        let c = input[len - back];
        if c & 0xC0 == 0x80 {
            continue;
        }
        let need = match c {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if need > back { len - back } else { len };
    }
    len
}

/// 以 ESC 开头的序列长度；不完整返回 None。超长未终结的序列只吞掉 ESC 本身，防止卡住流
fn escape_len(s: &[u8]) -> Option<usize> {
    const CSI_MAX: usize = 64;
    const STRING_MAX: usize = 4096;
    let second = *s.get(1)?;
    match second {
        b'[' => {
            for (j, &c) in s.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&c) {
                    return Some(j + 1);
                }
                if j > CSI_MAX {
                    return Some(2);
                }
            }
            None
        }
        // OSC / DCS / SOS / PM / APC：BEL 或 ST(ESC \) 结束
        b']' | b'P' | b'X' | b'^' | b'_' => {
            let mut j = 2;
            while j < s.len() {
                match s[j] {
                    0x07 => return Some(j + 1),
                    0x1b => {
                        return match s.get(j + 1) {
                            Some(b'\\') => Some(j + 2),
                            Some(_) => Some(j),
                            None => None,
                        }
                    }
                    _ if j > STRING_MAX => return Some(2),
                    _ => {}
                }
                j += 1;
            }
            None
        }
        // 字符集指定等三字节序列：ESC ( B
        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' | b'%' => {
            if s.len() < 3 {
                None
            } else {
                Some(3)
            }
        }
        _ => Some(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(input: &[u8]) -> String {
        strip_ansi(input, true).0
    }

    #[test]
    fn strips_colors_osc_and_charset() {
        assert_eq!(s(b"\x1b[1;31mERR\x1b[0m ok"), "ERR ok");
        assert_eq!(s(b"\x1b]0;title\x07$ ls\n"), "$ ls\n");
        assert_eq!(s(b"\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link");
        assert_eq!(s(b"\x1b(Bplain\x1b="), "plain");
        assert_eq!(s(b"a\x07b\x00c"), "abc");
    }

    #[test]
    fn folds_crlf_progress_and_backspace() {
        assert_eq!(s(b"l1\r\nl2\r\n"), "l1\nl2\n");
        assert_eq!(s(b"10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(s(b"abc\x08\x08x"), "ax");
        assert_eq!(s("中文\x08!".as_bytes()), "中!");
    }

    #[test]
    fn incremental_chunks_never_split_sequences() {
        let full = "a\x1b[31m红\x1b[0m\r\nb".as_bytes();
        // 任意切点：两段拼接 == 一次处理
        for cut in 0..=full.len() {
            let (t1, used) = strip_ansi(&full[..cut], false);
            assert!(used <= cut);
            let (t2, _) = strip_ansi(&full[used..], true);
            assert_eq!(format!("{t1}{t2}"), "a红\nb", "cut={cut}");
        }
    }

    #[test]
    fn head_tail_keeps_both_ends() {
        let mut h = HeadTail::new(8);
        h.push(b"abcdefgh");
        assert_eq!(h.omitted(), 0);
        assert_eq!(h.render(false), "abcdefgh");
        h.push(b"ijklmn");
        assert_eq!(h.omitted(), 6);
        let r = h.render(false);
        assert!(r.starts_with("abcd\n") && r.ends_with("\nklmn"), "{r}");
        // 尾段切在多字节字符中间不产生乱码
        let mut u = HeadTail::new(8);
        u.push("ab中文字".as_bytes());
        assert_eq!(u.render(false), "ab\n…[中间省略 3 字节]…\n字");
    }

    #[test]
    fn page_strip_always_advances() {
        let (t, used) = strip_ansi_page(b"\x1b[3", false);
        assert_eq!((t.as_str(), used), ("", 3));
        let (t, used) = strip_ansi_page(b"ok\x1b[3", false);
        assert_eq!((t.as_str(), used), ("ok", 2));
    }

    #[test]
    fn unterminated_sequences_do_not_stall() {
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 100));
        long.extend(b"x");
        let (t, used) = strip_ansi(&long, false);
        assert_eq!(used, long.len());
        assert!(t.ends_with('x'));
    }
}
