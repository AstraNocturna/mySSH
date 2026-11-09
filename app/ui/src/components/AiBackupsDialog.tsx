import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { Dialog } from './Dialog';
import { useT } from '../i18n';
import {
  describeChange,
  readRetentionDays,
  toolLabel,
  type ChangeSummary,
} from './ai-backup-format';

/** 后端 mcp_files::BackupMeta */
interface BackupMeta {
  id: string;
  sessionId: string;
  path: string;
  tool: string;
  existed: boolean;
  bytes: number;
  afterSha256: string | null;
  createdAt: number;
  title?: string;
  change?: ChangeSummary;
}

/**
 * AI 文件改动记录：列出该会话经 MCP 改动文件前的自动备份，一键回滚。
 * 远端文件在那次改动后又被修改过时后端拒绝，需用户二次确认「仍要覆盖」（force）。
 */
export function AiBackupsDialog({
  session,
  onClose,
}: {
  session: { id: string; name: string };
  onClose: () => void;
}) {
  const t = useT();
  const notify = useAppStore((s) => s.notify);
  const keepDays = useAppStore((s) => readRetentionDays(s.settings, 'aiBackups'));
  const [items, setItems] = useState<BackupMeta[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  /** 回滚被拒（远端已变化）的备份及原因，等待确认覆盖 */
  const [conflict, setConflict] = useState<{ id: string; error: string } | null>(null);

  const reload = () =>
    invoke<BackupMeta[]>('ai_backups_list', { sessionId: session.id })
      .then(setItems)
      .catch((e: unknown) => {
        setItems([]);
        notify(String(e), 'error');
      });

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.id]);

  const restore = async (id: string, force: boolean) => {
    setBusy(id);
    try {
      await invoke('ai_backup_restore', { backupId: id, force });
      setConflict(null);
      notify(t('dialogs.aiBackupRestored'), 'success');
      await reload();
    } catch (e) {
      const msg = String(e);
      if (!force && msg.includes('force=true')) setConflict({ id, error: msg });
      else notify(t('dialogs.aiBackupRestoreFailed', { error: msg }), 'error');
    } finally {
      setBusy(null);
    }
  };

  return (
    <Dialog
      title={t('dialogs.aiBackupsTitle', { name: session.name })}
      onClose={onClose}
      panelClass="flex max-h-[80vh] w-[44rem] flex-col rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-200 shadow-xl"
    >
      <h2 className="mb-1 text-sm font-semibold text-neutral-100">
        {t('dialogs.aiBackupsTitle', { name: session.name })}
      </h2>
      <p className="mb-3 text-neutral-500">{t('dialogs.aiBackupsHint', { days: keepDays })}</p>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {items === null ? (
          <p className="text-neutral-500">{t('dialogs.loading')}</p>
        ) : items.length === 0 ? (
          <p className="text-neutral-500">{t('dialogs.aiBackupsEmpty')}</p>
        ) : (
          <table className="w-full table-fixed">
            <thead className="text-left text-neutral-500">
              <tr>
                <th className="w-36 pb-1 font-normal">{t('dialogs.aiBackupTime')}</th>
                <th className="w-24 pb-1 font-normal">{t('dialogs.aiBackupTool')}</th>
                <th className="pb-1 font-normal">{t('dialogs.aiBackupChange')}</th>
                <th className="w-24 pb-1" />
              </tr>
            </thead>
            <tbody>
              {items.map((b) => {
                const summary = describeChange(b.change, t);
                return (
                  <tr key={b.id} className="border-t border-neutral-800 align-top">
                    <td className="py-1 text-neutral-400">
                      {new Date(b.createdAt).toLocaleString()}
                    </td>
                    <td className="py-1">{toolLabel(b.tool, t)}</td>
                    <td className="py-1">
                      {b.title && (
                        <div
                          className="flex items-baseline gap-1.5"
                          title={t('dialogs.aiBackupAiTitleHint')}
                        >
                          <span className="shrink-0 rounded bg-neutral-800 px-1 text-[10px] text-neutral-400">
                            {t('dialogs.aiBackupAiTitle')}
                          </span>
                          <span className="min-w-0 break-words text-neutral-100">{b.title}</span>
                        </div>
                      )}
                      {summary && (
                        <div className={b.title ? 'text-neutral-400' : 'text-neutral-100'}>
                          {summary}
                        </div>
                      )}
                      <div className="truncate font-mono text-neutral-400" title={b.path}>
                        {b.path}
                      </div>
                      <div className="text-neutral-500">
                        {b.existed
                          ? t('dialogs.aiBackupBefore', { bytes: b.bytes })
                          : t('dialogs.aiBackupWasAbsent')}
                      </div>
                      {conflict?.id === b.id && (
                        <div className="mt-1 text-amber-300">{conflict.error}</div>
                      )}
                    </td>
                    <td className="py-1 text-right">
                      {conflict?.id === b.id ? (
                        <button
                          className="rounded bg-red-700 px-2 py-0.5 text-white hover:bg-red-600 disabled:opacity-50"
                          disabled={busy !== null}
                          onClick={() => void restore(b.id, true)}
                        >
                          {t('dialogs.aiBackupForce')}
                        </button>
                      ) : (
                        <button
                          className="rounded bg-neutral-700 px-2 py-0.5 hover:bg-neutral-600 disabled:opacity-50"
                          disabled={busy !== null}
                          onClick={() => void restore(b.id, false)}
                        >
                          {t('dialogs.aiBackupRestore')}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
      <div className="mt-3 flex justify-end">
        <button
          className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
          onClick={onClose}
        >
          {t('dialogs.close')}
        </button>
      </div>
    </Dialog>
  );
}
