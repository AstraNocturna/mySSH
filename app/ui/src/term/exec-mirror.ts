/**
 * AI 命令镜像（只读）：后端 ssh_exec / recipe_run 执行时推 exec_start / exec_output /
 * exec_end 帧（crates/app/src/mcp_ui.rs ExecMirror），前端按会话汇入「AI · 主机」只读标签。
 * 这里只做帧 → 终端文本的格式化，以及标签挂载前的输出缓冲。
 */

export interface ExecStartFrame {
  type: 'exec_start';
  execId: string;
  sessionId: string;
  sessionName: string;
  tool: string;
  command: string;
}
export interface ExecOutputFrame {
  type: 'exec_output';
  execId: string;
  sessionId: string;
  stream: 'stdout' | 'stderr';
  data: string;
}
export interface ExecEndFrame {
  type: 'exec_end';
  execId: string;
  sessionId: string;
  /** cancelled：用户在活动面板停止 */
  status: 'ok' | 'error' | 'timeout' | 'cancelled';
  exitCode: number | null;
  durationMs: number;
  error: string | null;
  omittedBytes: number;
}
export type ExecMirrorFrame = ExecStartFrame | ExecOutputFrame | ExecEndFrame;

const DIM = '\x1b[2m';
const RESET = '\x1b[0m';

/** exec 通道无 PTY：输出只有 \n，xterm 需要 \r\n 才回到行首 */
function crlf(s: string): string {
  return s.replace(/\r?\n/g, '\r\n');
}

function clock(now: Date): string {
  return [now.getHours(), now.getMinutes(), now.getSeconds()]
    .map((n) => String(n).padStart(2, '0'))
    .join(':');
}

export function formatStart(f: ExecStartFrame, now: Date): string {
  const tool = f.tool === 'ssh_exec' ? '' : ` ${f.tool}`;
  const cmd = f.command.split(/\r?\n/).join(`\r\n${DIM}>${RESET} `);
  return `${DIM}[${clock(now)}] AI${tool}${RESET}\r\n\x1b[1;36m$${RESET} \x1b[1m${cmd}${RESET}\r\n`;
}

export function formatOutput(f: ExecOutputFrame): string {
  const text = crlf(f.data);
  return f.stream === 'stderr' ? `\x1b[31m${text}${RESET}` : text;
}

/**
 * atLineStart：此前输出是否已以换行结尾（否则先补换行，结果行不黏在输出末尾）。
 * labels 由调用方按语言传入。
 */
export function formatEnd(
  f: ExecEndFrame,
  atLineStart: boolean,
  labels: { omitted: (n: number) => string; timeout: string; cancelled: string; error: string },
): string {
  let s = atLineStart ? '' : '\r\n';
  if (f.omittedBytes > 0) s += `${DIM}${labels.omitted(f.omittedBytes)}${RESET}\r\n`;
  const dur = `${DIM}· ${fmtDuration(f.durationMs)}${RESET}`;
  if (f.status === 'ok') {
    const ok = f.exitCode === 0;
    s += `${ok ? '\x1b[32m✓' : '\x1b[31m✗'} exit ${f.exitCode ?? '?'}${RESET} ${dur}`;
  } else if (f.status === 'timeout') {
    s += `\x1b[33m✗ ${labels.timeout}${RESET} ${dur}`;
  } else if (f.status === 'cancelled') {
    s += `\x1b[33m✗ ${labels.cancelled}${RESET} ${dur}`;
  } else {
    s += `\x1b[31m✗ ${labels.error}${f.error ? `：${crlf(f.error)}` : ''}${RESET} ${dur}`;
  }
  return `${s}\r\n\r\n`;
}

function fmtDuration(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/** 标签视图挂载前的缓冲上限（字符数）；超出丢最旧块 */
const BACKLOG_CAP = 1024 * 1024;

/** 单个镜像标签的输出流：视图挂载前先缓冲，挂载后直写 */
export class MirrorFeed {
  private backlog: string[] = [];
  private size = 0;
  private sink: ((s: string) => void) | null = null;
  /** 最近一次写出的输出是否以换行结尾（决定结束行前是否补换行） */
  atLineStart = true;

  write(s: string): void {
    if (!s) return;
    // eslint-disable-next-line no-control-regex
    this.atLineStart = s.replace(/(\x1b\[[0-9;]*m)+$/, '').endsWith('\n');
    if (this.sink) {
      this.sink(s);
      return;
    }
    this.backlog.push(s);
    this.size += s.length;
    while (this.size > BACKLOG_CAP && this.backlog.length > 1) {
      this.size -= this.backlog.shift()?.length ?? 0;
    }
  }

  /** 挂载视图：先吐出缓冲；返回卸载函数 */
  attach(sink: (s: string) => void): () => void {
    for (const s of this.backlog) sink(s);
    this.backlog = [];
    this.size = 0;
    this.sink = sink;
    return () => {
      if (this.sink === sink) this.sink = null;
    };
  }
}

/** 镜像标签 id → 输出流 */
export const mirrorFeeds = new Map<string, MirrorFeed>();

export function feedOf(tabId: string): MirrorFeed {
  let f = mirrorFeeds.get(tabId);
  if (!f) {
    f = new MirrorFeed();
    mirrorFeeds.set(tabId, f);
  }
  return f;
}
