import { describe, expect, it } from 'vitest';
import {
  LineTracker,
  expandTrigger,
  insertSnippet,
  matchTrigger,
  readSnippets,
  validateSnippet,
  type Snippet,
} from './snippets';

const snip = (p: Partial<Snippet>): Snippet => ({ id: 'x', name: 'n', command: 'c', ...p });

function sink() {
  const calls: string[] = [];
  return {
    calls,
    paste: (d: string) => calls.push(`paste:${d}`),
    input: (d: string) => calls.push(`input:${JSON.stringify(d)}`),
  };
}

describe('readSnippets', () => {
  it('丢弃结构不符的条目', () => {
    const ok = snip({ trigger: 'dps', run: true });
    expect(readSnippets([ok, { id: 1 }, null, 'x', { ...ok, run: 'yes' }])).toEqual([ok]);
    expect(readSnippets('nope')).toEqual([]);
  });
});

describe('validateSnippet', () => {
  const all = [snip({ id: 'a', trigger: 'dps' })];
  it('必填与触发词规则', () => {
    expect(validateSnippet(snip({ name: ' ' }), all)).toBe('nameRequired');
    expect(validateSnippet(snip({ command: '' }), all)).toBe('commandRequired');
    expect(validateSnippet(snip({ trigger: 'a b' }), all)).toBe('triggerInvalid');
    expect(validateSnippet(snip({ id: 'b', trigger: 'dps' }), all)).toBe('triggerTaken');
    // 编辑自身不算冲突
    expect(validateSnippet(snip({ id: 'a', trigger: 'dps' }), all)).toBeNull();
  });
});

describe('LineTracker', () => {
  it('打印字符累积、退格按字符删除、控制输入清空', () => {
    const t = new LineTracker();
    t.feed('l');
    t.feed('s 中');
    expect(t.text).toBe('ls 中');
    t.feed('\x7f');
    expect(t.text).toBe('ls ');
    t.feed('\x1b[A'); // 方向键：远端可能换了历史行
    expect(t.text).toBe('');
    t.feed('abc');
    t.feed('\r');
    expect(t.text).toBe('');
    t.feed('x');
    t.feed('\t');
    expect(t.text).toBe('');
  });
});

describe('matchTrigger', () => {
  const list = [snip({ id: '1', trigger: 'dps' }), snip({ id: '2', trigger: 'xdps' })];
  it('只匹配行尾完整 token', () => {
    expect(matchTrigger('dps', list)?.id).toBe('1');
    expect(matchTrigger('sudo dps', list)?.id).toBe('1');
    expect(matchTrigger('xdps', list)?.id).toBe('2');
    expect(matchTrigger('adps', list)).toBeNull();
    expect(matchTrigger('dps ', list)).toBeNull();
    expect(matchTrigger('', list)).toBeNull();
  });
});

describe('插入与展开', () => {
  it('去尾换行后粘贴，run 时补回车', () => {
    const s = sink();
    insertSnippet(s, snip({ command: 'echo 1\necho 2\n', run: true }));
    expect(s.calls).toEqual(['paste:echo 1\necho 2', 'input:"\\r"']);
  });

  it('展开先按字符数退格删掉触发词', () => {
    const s = sink();
    expandTrigger(s, snip({ trigger: '中d', command: 'docker ps' }));
    expect(s.calls).toEqual([`input:${JSON.stringify('\x7f\x7f')}`, 'paste:docker ps']);
  });
});
