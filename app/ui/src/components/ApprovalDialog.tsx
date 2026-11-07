import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { Dialog } from './Dialog';
import { useT } from '../i18n';

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

  const remainingMs = pending ? pending.receivedAt + pending.expiresInMs - now : 0;
  useEffect(() => {
    if (pending && remainingMs <= 0) answer(pending.id, false);
  }, [pending, remainingMs, answer]);

  if (!pending) return null;
  const deny = () => answer(pending.id, false);
  const prod = pending.production === true;

  return (
    <Dialog
      // 换题即重挂载：焦点重新落到「拒绝」
      key={pending.id}
      title={t('dialogs.approvalTitle')}
      onClose={deny}
      closeOnBackdrop={false}
      backdropClass="z-20"
      panelClass={`w-[32rem] rounded-lg border p-4 text-sm shadow-xl ${
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
      <pre className="mb-2 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-black/50 p-2 font-mono text-xs">
        {pending.command}
      </pre>
      <p className="mb-3 text-xs text-neutral-400">
        {t('dialogs.approvalHint', { sec: Math.max(0, Math.ceil(remainingMs / 1000)) })}
        {queued > 0 && <> · {t('dialogs.approvalQueue', { count: queued })}</>}
      </p>
      <div className="flex justify-end gap-2">
        <button
          data-autofocus
          className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
          onClick={deny}
        >
          {t('dialogs.approvalDeny')}
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
    </Dialog>
  );
}
