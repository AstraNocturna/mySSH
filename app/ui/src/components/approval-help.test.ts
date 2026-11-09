import { describe, expect, it } from 'vitest';
import { commandHelpKey, summarizeParts } from './approval-help';
import { translate } from '../i18n';
import type { ApprovalPart } from '../state/app-store';

const zh = (key: Parameters<typeof translate>[1], vars?: Record<string, string | number>) =>
  translate('zh-CN', key, vars);
const en = (key: Parameters<typeof translate>[1], vars?: Record<string, string | number>) =>
  translate('en-US', key, vars);

const part = (p: Partial<ApprovalPart>): ApprovalPart => ({
  text: p.head ?? '',
  op: null,
  head: null,
  sub: null,
  wrapper: null,
  writes: null,
  append: false,
  heredocLines: 0,
  risk: 'readonly',
  ...p,
});

describe('approval-help', () => {
  it('优先匹配「命令 子命令」，退回命令名，未知命令为 null', () => {
    expect(commandHelpKey('systemctl', 'restart')).toBe('cmd.systemctl restart');
    expect(commandHelpKey('systemctl', 'nginx.service')).toBe('cmd.systemctl');
    expect(commandHelpKey('rm', '/tmp/x')).toBe('cmd.rm');
    expect(commandHelpKey('my-tool', null)).toBeNull();
    expect(commandHelpKey(null, null)).toBeNull();
    expect(commandHelpKey('constructor', null)).toBeNull();
  });

  it('heredoc 写文件 + ls && cat：合成一句总说明，原因只列写入的文件', () => {
    const file = '/root/99乘法口诀.txt';
    const s = summarizeParts(
      [
        part({ head: 'cat', writes: file, heredocLines: 9, risk: 'write', op: ';' }),
        part({ head: 'ls', sub: file, op: '&&' }),
        part({ head: 'cat', sub: file }),
      ],
      zh,
    );
    expect(s.text).toBe(`把内嵌的 9 行文本写入 ${file}，然后列出目录内容，然后显示文件内容`);
    expect(s.unknown).toBe('');
    expect(s.reasons).toBe(`写入文件 ${file}`);
  });

  it('未知命令不逐条解释，只列名字；sudo 前缀、后台、失败分支', () => {
    const s = summarizeParts(
      [
        part({ head: 'deploy.sh', risk: 'write', op: '&&' }),
        part({ head: 'systemctl', sub: 'restart', wrapper: 'sudo', risk: 'indirect', op: '||' }),
        part({ head: 'journalctl', op: '&' }),
      ],
      zh,
    );
    expect(s.text).toBe('以 root 身份重启服务（短暂中断），失败则查看 systemd 日志（后台运行）');
    expect(s.unknown).toBe('未内置说明的命令：deploy.sh');
    expect(s.reasons).toBe('间接执行 sudo；可能修改系统 deploy.sh');
  });

  it('英文文案：后续步骤首字母小写', () => {
    const s = summarizeParts(
      [part({ head: 'df', op: '&&' }), part({ head: 'rm', sub: '/tmp/x', risk: 'blacklist' })],
      en,
    );
    expect(s.text).toBe('Show disk space, then delete files or directories (cannot be undone)');
    expect(s.reasons).toBe('high-risk command rm');
  });

  it('同一说明连续出现只说一次', () => {
    const s = summarizeParts(
      [part({ head: 'cat', op: ';' }), part({ head: 'cat', op: ';' }), part({ head: 'df' })],
      zh,
    );
    expect(s.text).toBe('显示文件内容，然后查看磁盘空间');
  });
});
