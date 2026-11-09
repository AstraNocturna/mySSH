import { describe, expect, it } from 'vitest';
import { describeChange, readRetentionDays, toolLabel } from './ai-backup-format';
import { translate } from '../i18n';

const zh = (key: Parameters<typeof translate>[1], vars?: Record<string, string | number>) =>
  translate('zh-CN', key, vars);
const en = (key: Parameters<typeof translate>[1], vars?: Record<string, string | number>) =>
  translate('en-US', key, vars);

describe('ai-backup-format', () => {
  it('把改动摘要说成一句话', () => {
    expect(describeChange({ kind: 'created', lines: 12 }, zh)).toBe('新建文件，共 12 行');
    expect(describeChange({ kind: 'deleted', lines: 3 }, zh)).toBe('删除文件，原有 3 行');
    expect(describeChange({ kind: 'modified', line: 8, added: 1, removed: 1 }, zh)).toBe(
      '修改了第 8 行',
    );
    expect(describeChange({ kind: 'modified', line: 8, added: 3, removed: 0 }, zh)).toBe(
      '从第 8 行起：新增 3 行，删除 0 行',
    );
    expect(describeChange({ kind: 'unchanged' }, zh)).toBe('内容没有变化');
    expect(describeChange({ kind: 'binary', beforeBytes: 10, afterBytes: 20 }, en)).toBe(
      'Binary content (10 → 20 bytes)',
    );
  });

  it('旧版本备份没有摘要时不编造', () => {
    expect(describeChange(undefined, zh)).toBeNull();
    expect(describeChange(null, zh)).toBeNull();
  });

  it('工具名转操作名，未知工具原样显示', () => {
    expect(toolLabel('file_edit', zh)).toBe('编辑');
    expect(toolLabel('sftp_write', zh)).toBe('覆盖写入');
    expect(toolLabel('rollback', en)).toBe('Rollback');
    expect(toolLabel('future_tool', zh)).toBe('future_tool');
  });

  it('保留天数：缺省或越界用默认值（备份 7 天、审计 30 天）', () => {
    expect(readRetentionDays({}, 'aiBackups')).toBe(7);
    expect(readRetentionDays({}, 'audit')).toBe(30);
    expect(readRetentionDays({ 'history.aiBackupDays': 14 }, 'aiBackups')).toBe(14);
    expect(readRetentionDays({ 'history.aiBackupDays': 0 }, 'aiBackups')).toBe(7);
    expect(readRetentionDays({ 'history.auditDays': 400 }, 'audit')).toBe(30);
    expect(readRetentionDays({ 'history.auditDays': '60' }, 'audit')).toBe(30);
  });
});
