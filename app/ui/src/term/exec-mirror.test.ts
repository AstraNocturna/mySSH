import { describe, expect, it } from 'vitest';
import { MirrorFeed, formatEnd, formatOutput, formatStart, type ExecEndFrame } from './exec-mirror';

const labels = {
  omitted: (n: number) => `omitted ${n}`,
  timeout: 'timeout',
  cancelled: 'stopped',
  error: 'error',
};
const end = (p: Partial<ExecEndFrame>): ExecEndFrame => ({
  type: 'exec_end',
  execId: 'e',
  sessionId: 's',
  status: 'ok',
  exitCode: 0,
  durationMs: 12,
  error: null,
  omittedBytes: 0,
  ...p,
});
// eslint-disable-next-line no-control-regex
const plain = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, '');

describe('exec-mirror', () => {
  it('start 行显示命令，多行命令续行', () => {
    const s = plain(
      formatStart(
        {
          type: 'exec_start',
          execId: 'e',
          sessionId: 's',
          sessionName: 'web',
          tool: 'ssh_exec',
          command: 'ls -la\necho hi',
        },
        new Date(2026, 0, 1, 9, 5, 7),
      ),
    );
    expect(s).toBe('[09:05:07] AI\r\n$ ls -la\r\n> echo hi\r\n');
  });

  it('输出 LF 转 CRLF，stderr 标红', () => {
    const f = { type: 'exec_output', execId: 'e', sessionId: 's' } as const;
    expect(formatOutput({ ...f, stream: 'stdout', data: 'a\nb\r\nc' })).toBe('a\r\nb\r\nc');
    expect(formatOutput({ ...f, stream: 'stderr', data: 'x' })).toContain('\x1b[31mx');
  });

  it('结束行：退出码、超时、错误、省略提示，必要时先补换行', () => {
    expect(plain(formatEnd(end({}), true, labels))).toBe('✓ exit 0 · 12 ms\r\n\r\n');
    expect(plain(formatEnd(end({ exitCode: 2 }), false, labels))).toBe(
      '\r\n✗ exit 2 · 12 ms\r\n\r\n',
    );
    expect(plain(formatEnd(end({ status: 'timeout', durationMs: 3000 }), true, labels))).toBe(
      '✗ timeout · 3.0 s\r\n\r\n',
    );
    expect(plain(formatEnd(end({ status: 'cancelled', exitCode: null }), true, labels))).toBe(
      '✗ stopped · 12 ms\r\n\r\n',
    );
    expect(
      plain(formatEnd(end({ status: 'error', exitCode: null, error: 'refused' }), true, labels)),
    ).toBe('✗ error：refused · 12 ms\r\n\r\n');
    expect(plain(formatEnd(end({ omittedBytes: 99 }), true, labels))).toContain('omitted 99\r\n');
  });

  it('视图挂载前缓冲，挂载后按序吐出并直写', () => {
    const feed = new MirrorFeed();
    feed.write('a');
    feed.write('b\n');
    expect(feed.atLineStart).toBe(true);
    const got: string[] = [];
    const detach = feed.attach((s) => got.push(s));
    feed.write('c');
    expect(got).toEqual(['a', 'b\n', 'c']);
    expect(feed.atLineStart).toBe(false);
    detach();
    feed.write('d');
    expect(got).toHaveLength(3);
    // stderr 以重置码收尾，仍按换行结尾判定
    feed.write('\x1b[31merr\n\x1b[0m');
    expect(feed.atLineStart).toBe(true);
  });
});
