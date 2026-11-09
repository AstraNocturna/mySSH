import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { Dialog } from './Dialog';
import { useT } from '../i18n';
import { summarizeParts } from './approval-help';

/**
 * MCP 危险命令审批弹窗（安全模型第 2 条的人工确认通道）。
 * 与 HostKeyDialog 同一防误触规则：默认焦点在「拒绝」、Esc = 拒绝、点遮罩不关闭；
 * 到期（后端 TTL）未决自动按拒绝出队。
 */
export function ApprovalDialog() {
  const t = useT();
  const pending = useAppStore((s) => s.pendingApprovals[0] ?? null);
  const queued = useAppStore((s) => s.pendingApprovals.length - 1);
  const answer = useAppStore((s) => s.answerApproval);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!pending) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [pending]);

  // now 在弹窗挂载时初始化，可能早于收到请求：不超过总时长
  const remainingMs = pending
    ? Math.min(pending.expiresInMs, pending.receivedAt + pending.expiresInMs - now)
    : 0;
  useEffect(() => {
    if (pending && remainingMs <= 0) answer(pending.id, false);
  }, [pending, remainingMs, answer]);

  if (!pending) return null;
  const deny = () => answer(pending.id, false);
  const prod = pending.production === true;
  const summary = pending.parts?.length ? summarizeParts(pending.parts, t) : null;

  return (
    <Dialog
      // 换题即重挂载：焦点重新落到「拒绝」
      key={pending.id}
      title={t('dialogs.approvalTitle')}
      onClose={deny}
      closeOnBackdrop={false}
      backdropClass="z-20"
      panelClass={`w-[36rem] rounded-lg border p-4 text-sm shadow-xl ${
        prod
          ? 'border-red-700 bg-red-950 text-red-100'
          : 'border-amber-700 bg-neutral-900 text-neutral-200'
      }`}
    >
      <h2 className="mb-2 flex items-center gap-2 text-base font-semibold">
        {t('dialogs.approvalTitle')}
        {prod && <span className="text-red-300">{t('dialogs.approvalProduction')}</span>}
      </h2>
      <p className="mb-1 text-xs text-neutral-400">
        {t('dialogs.approvalSession', {
          name: pending.sessionName ?? pending.sessionId,
          user: pending.user ?? '',
          host: pending.host ?? '',
        })}
      </p>
      {pending.level && (
        <p className="mb-2 text-xs text-neutral-400">
          {t('dialogs.approvalLevel', { level: pending.level })}
        </p>
      )}
      {pending.purpose && (
        <div className="mb-2 rounded border border-sky-800/60 bg-sky-950/40 p-2 text-xs">
          <div className="mb-0.5 text-sky-300">{t('dialogs.approvalPurpose')}</div>
          <div className="whitespace-pre-wrap break-words text-neutral-200">{pending.purpose}</div>
        </div>
      )}
      <pre className="mb-2 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-black/50 p-2 font-mono text-xs">
        {pending.command}
      </pre>
      {summary && (
        <div className="mb-2 rounded bg-black/30 p-2 text-xs">
          <div className="mb-0.5 text-neutral-400">{t('dialogs.approvalSummary')}</div>
          <div className="text-neutral-200">{summary.text || t('dialogs.approvalNothing')}</div>
          {summary.unknown && <div className="mt-0.5 text-neutral-500">{summary.unknown}</div>}
          {summary.reasons && (
            <div className="mt-1 text-amber-300">
              {t('dialogs.approvalReasons')}
              {summary.reasons}
            </div>
          )}
        </div>
      )}
      <p className="mb-3 text-xs text-neutral-400">
        {t('dialogs.approvalHint', { sec: Math.max(0, Math.ceil(remainingMs / 1000)) })}
        {queued > 0 && <> · {t('dialogs.approvalQueue', { count: queued })}</>}
      </p>
      <div className="flex items-center justify-end gap-2">
        <button
          data-autofocus
          className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
          onClick={deny}
        >
          {t('dialogs.approvalDeny')}
        </button>
        <button
          className="rounded border border-neutral-600 px-3 py-1 hover:bg-neutral-800"
          onClick={() => answer(pending.id, true, true)}
        >
          {t('dialogs.approvalAlways')}
        </button>
        <button
          className={`rounded px-3 py-1 text-white ${
            prod ? 'bg-red-600 hover:bg-red-500' : 'bg-amber-600 hover:bg-amber-500'
          }`}
          onClick={() => answer(pending.id, true)}
        >
          {t('dialogs.approvalAllow')}
        </button>
      </div>
      <p className="mt-2 text-right text-[11px] text-neutral-500">
        {t('dialogs.approvalAlwaysHint')}
      </p>
    </Dialog>
  );
}
