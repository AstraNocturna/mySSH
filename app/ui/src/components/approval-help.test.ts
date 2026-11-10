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
    expect(s.reasons).toBe('sudo 能执行任意命令或改写文件，无法确认只读；可能修改系统 deploy.sh');
  });

  it('英文文案：后续步骤首字母小写', () => {
    const s = summarizeParts(
      [part({ head: 'df', op: '&&' }), part({ head: 'rm', sub: '/tmp/x', risk: 'blacklist' })],
      en,
    );
    expect(s.text).toBe('Show disk space, then delete files or directories (cannot be undone)');
    expect(s.reasons).toBe('high-risk command rm');
  });

  it('多行脚本只给整体说明；原因指向真正执行的命令，不把关键字当命令', () => {
    const awk = { head: 'awk', sub: '/^write_bytes/{print $2; exit}', risk: 'indirect' } as const;
    const ro = (head: string, sub: string | null = null) =>
      ({ head, sub, risk: 'readonly' }) as const;
    const command = [
      '# 两次采样',
      'declare -A a b',
      'for p in /proc/[0-9]*; do v=$(awk ... 2>/dev/null); [ -n "$v" ] && a[x]=$v; done',
      'sleep 5',
      'for k in "${!b[@]}"; do if [ "$d" -gt 1 ]; then printf x; fi; done | sort -rn | head -12',
    ].join('\n');
    const s = summarizeParts(
      [
        part({ head: 'declare', cmds: [ro('declare')], op: ';' }),
        part({ keyword: 'for', head: null, op: ';' }),
        part({ keyword: null, head: null, cmds: [awk], risk: 'indirect', op: ';' }),
        part({ head: '[', cmds: [ro('[')], op: '&&' }),
        part({ head: null, op: ';' }),
        part({ head: 'sleep', cmds: [ro('sleep', '5')], op: ';' }),
        part({ keyword: 'for', head: null, op: ';' }),
        part({ keyword: 'if', head: '[', cmds: [ro('[')], op: ';' }),
        part({ head: 'printf', cmds: [ro('printf')], op: '|' }),
        part({ head: 'sort', cmds: [ro('sort')], op: '|' }),
        part({ head: 'head', sub: '-12', cmds: [ro('head', '-12')] }),
      ],
      zh,
      command,
    );
    expect(s.text).toBe(
      '一段 4 行的脚本（含循环、条件判断），用到 declare（声明变量）、awk（处理文本）、' +
        '[（判断条件）、sleep（等待一段时间）、printf（按格式输出文本）、sort（对文本排序）等 7 个命令。未发现写文件操作',
    );
    expect(s.unknown).toBe('');
    expect(s.reasons).toBe('awk 能执行任意命令或改写文件，无法确认只读');
    expect(s.reasons).not.toMatch(/\bdo\b|for|\/dev\/null|declare/);
  });

  it('改关键环境变量单列原因；脚本写文件时说明写到哪里', () => {
    const s = summarizeParts(
      [
        part({ head: null, cmds: [{ head: 'PATH=', sub: null, risk: 'indirect' }], op: ';' }),
        part({
          keyword: 'while',
          head: 'read',
          cmds: [{ head: 'read', sub: null, risk: 'readonly' }],
          op: ';',
        }),
        part({
          head: 'echo',
          writes: '/tmp/out',
          risk: 'write',
          cmds: [{ head: 'echo', sub: null, risk: 'readonly' }],
        }),
      ],
      zh,
      'PATH=/x\nwhile read l; do echo $l > /tmp/out; done',
    );
    expect(s.text).toBe(
      '一段 2 行的脚本（含循环），用到 read（读入一行到变量）、echo（输出文本）。会写入 /tmp/out',
    );
    expect(s.reasons).toBe('修改关键环境变量 PATH；写入文件 /tmp/out');
  });

  it('同一说明连续出现只说一次', () => {
    const s = summarizeParts(
      [part({ head: 'cat', op: ';' }), part({ head: 'cat', op: ';' }), part({ head: 'df' })],
      zh,
    );
    expect(s.text).toBe('显示文件内容，然后查看磁盘空间');
  });
});
