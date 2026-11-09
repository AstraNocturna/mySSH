import { describe, expect, it } from 'vitest';
import { commandHelpKey, opKey } from './approval-help';
import { translate } from '../i18n';

describe('approval-help', () => {
  it('优先匹配「命令 子命令」，退回命令名，未知命令为 null', () => {
    expect(commandHelpKey('systemctl', 'restart')).toBe('cmd.systemctl restart');
    expect(commandHelpKey('systemctl', 'nginx.service')).toBe('cmd.systemctl');
    expect(commandHelpKey('rm', '/tmp/x')).toBe('cmd.rm');
    expect(commandHelpKey('my-tool', null)).toBeNull();
    expect(commandHelpKey(null, null)).toBeNull();
    // 原型链上的名字不能被当成命令说明
    expect(commandHelpKey('constructor', null)).toBeNull();
  });

  it('简介与连接符有中英文文案', () => {
    const k = commandHelpKey('rm', null);
    expect(k).not.toBeNull();
    if (!k) return;
    expect(translate('zh-CN', k)).toContain('删除');
    expect(translate('en-US', k)).toContain('Delete');
    expect(opKey('&&')).toBe('dialogs.approvalOpAnd');
    expect(opKey(null)).toBeNull();
  });
});
