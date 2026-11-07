/**
 * 命令片段（snippets）：用户保存的常用命令，经命令面板 / 片段管理框插入当前终端，
 * 或在提示符处键入触发词后按 Tab 展开。
 *
 * 插入走 xterm `paste()`：远端开启 bracketed paste（`CSI ? 2004 h`）时自动包裹
 * `ESC[200~ … ESC[201~`，多行片段整体进入编辑缓冲而不是逐行执行；未开启时换行即回车。
 * `run=true` 的片段在粘贴后补一个回车。
 */

export const SNIPPETS_KEY = 'terminal.snippets';

export interface Snippet {
  id: string;
  name: string;
  command: string;
  /** Tab 展开触发词（不含空白） */
  trigger?: string;
  /** 插入后自动回车执行 */
  run?: boolean;
}

/** 设置值 → 片段列表（脏数据容忍：结构不符的条目丢弃） */
export function readSnippets(v: unknown): Snippet[] {
  if (!Array.isArray(v)) return [];
  return v.filter(
    (x): x is Snippet =>
      !!x &&
      typeof x === 'object' &&
      typeof (x as Snippet).id === 'string' &&
      typeof (x as Snippet).name === 'string' &&
      typeof (x as Snippet).command === 'string' &&
      ((x as Snippet).trigger === undefined || typeof (x as Snippet).trigger === 'string') &&
      ((x as Snippet).run === undefined || typeof (x as Snippet).run === 'boolean'),
  );
}

export const TRIGGER_RE = /^\S{1,32}$/;

/** 编辑校验：返回错误键（i18n）或 null */
export function validateSnippet(
  s: Pick<Snippet, 'id' | 'name' | 'command' | 'trigger'>,
  all: Snippet[],
): 'nameRequired' | 'commandRequired' | 'triggerInvalid' | 'triggerTaken' | null {
  if (!s.name.trim()) return 'nameRequired';
  if (!s.command.trim()) return 'commandRequired';
  if (s.trigger) {
    if (!TRIGGER_RE.test(s.trigger)) return 'triggerInvalid';
    if (all.some((o) => o.id !== s.id && o.trigger === s.trigger)) return 'triggerTaken';
  }
  return null;
}

/**
 * 跟踪用户在当前行键入的文本，用于 Tab 展开判定。只认可打印字符与退格；
 * 回车、方向键、Ctrl 组合、Tab 等任何控制输入都可能让本地视图与远端行缓冲失配，
 * 一律清空——宁可不展开，也不删错字符。
 */
export class LineTracker {
  text = '';

  feed(data: string): void {
    if (data === '\x7f' || data === '\b') {
      this.text = Array.from(this.text).slice(0, -1).join('');
      return;
    }
    // eslint-disable-next-line no-control-regex
    if (data.length > 0 && !/[\x00-\x1f\x7f]/.test(data)) {
      this.text = (this.text + data).slice(-256);
      return;
    }
    this.text = '';
  }

  reset(): void {
    this.text = '';
  }
}

/** 行尾完整 token 等于某触发词 → 该片段（多个命中取最长触发词） */
export function matchTrigger(line: string, snippets: Snippet[]): Snippet | null {
  let best: Snippet | null = null;
  for (const s of snippets) {
    const t = s.trigger;
    if (!t || !line.endsWith(t)) continue;
    const before = line.slice(0, line.length - t.length);
    if (before !== '' && !/\s$/.test(before)) continue;
    if (!best || t.length > (best.trigger?.length ?? 0)) best = s;
  }
  return best;
}

/** 片段插入所需的终端能力（xterm Terminal 的子集，便于测试） */
export interface SnippetSink {
  paste(data: string): void;
  input(data: string, wasUserInput?: boolean): void;
}

export function insertSnippet(term: SnippetSink, s: Snippet): void {
  // 尾换行去掉：否则 run=false 也会被当回车执行，run=true 会执行两次
  term.paste(s.command.replace(/[\r\n]+$/, ''));
  if (s.run) term.input('\r');
}

/** Tab 展开：退格删掉已键入的触发词，再插入片段 */
export function expandTrigger(term: SnippetSink, s: Snippet): void {
  const n = Array.from(s.trigger ?? '').length;
  if (n > 0) term.input('\x7f'.repeat(n));
  insertSnippet(term, s);
}

export function newSnippetId(): string {
  return `sn-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}
