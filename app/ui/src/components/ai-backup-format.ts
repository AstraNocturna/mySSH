import type { MsgKey } from '../i18n';

type T = (key: MsgKey, vars?: Record<string, string | number>) => string;

/** 保留天数设置（后端 retention 模块读同样的键、默认值与范围） */
export const RETENTION = {
  aiBackups: { key: 'history.aiBackupDays', def: 7 },
  audit: { key: 'history.auditDays', def: 30 },
} as const;
export const RETENTION_MAX_DAYS = 365;

export function readRetentionDays(
  settings: Record<string, unknown>,
  which: keyof typeof RETENTION,
): number {
  const { key, def } = RETENTION[which];
  const v = settings[key];
  return typeof v === 'number' && Number.isInteger(v) && v >= 1 && v <= RETENTION_MAX_DAYS
    ? v
    : def;
}

/** 后端 mcp_files::ChangeSummary */
export type ChangeSummary =
  | { kind: 'created'; lines: number }
  | { kind: 'deleted'; lines: number }
  | { kind: 'modified'; line: number; added: number; removed: number }
  | { kind: 'unchanged' }
  | { kind: 'binary'; beforeBytes: number; afterBytes: number };

const TOOL_LABELS: Record<string, MsgKey> = {
  file_edit: 'dialogs.aiBackupToolEdit',
  sftp_write: 'dialogs.aiBackupToolWrite',
  sftp_delete: 'dialogs.aiBackupToolDelete',
  rollback: 'dialogs.aiBackupToolRollback',
};

/** 触发改动的工具名 → 用户看得懂的操作名；未知工具原样显示 */
export function toolLabel(tool: string, t: T): string {
  const key = TOOL_LABELS[tool];
  return key ? t(key) : tool;
}

/** 改动摘要 → 一句话；旧版本备份没有摘要时返回 null */
export function describeChange(change: ChangeSummary | null | undefined, t: T): string | null {
  if (!change) return null;
  switch (change.kind) {
    case 'created':
      return t('dialogs.aiChangeCreated', { lines: change.lines });
    case 'deleted':
      return t('dialogs.aiChangeDeleted', { lines: change.lines });
    case 'modified':
      return change.added === 1 && change.removed === 1
        ? t('dialogs.aiChangeModifiedOne', { line: change.line })
        : t('dialogs.aiChangeModified', {
            line: change.line,
            added: change.added,
            removed: change.removed,
          });
    case 'unchanged':
      return t('dialogs.aiChangeUnchanged');
    case 'binary':
      return t('dialogs.aiChangeBinary', {
        before: change.beforeBytes,
        after: change.afterBytes,
      });
  }
}
