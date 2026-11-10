import { useMemo } from 'react';
import { useAppStore } from '../state/app-store';
import { aiStatusBySession, useAiActivity, type SessionAiStatus } from '../state/ai-activity';
import { useT } from '../i18n';

/** 会话 id → AI 状态（执行中 / 待确认）；标签栏与侧边栏共用 */
export function useAiStatusMap(): Map<string, SessionAiStatus> {
  const execs = useAiActivity((s) => s.execs);
  const jobs = useAiActivity((s) => s.jobs);
  const terminals = useAiActivity((s) => s.terminals);
  const approvals = useAppStore((s) => s.pendingApprovals);
  return useMemo(
    () => aiStatusBySession({ execs, jobs, terminals }, approvals),
    [execs, jobs, terminals, approvals],
  );
}

/** 「待确认 n」（点击展开该会话的授权项）与「AI 执行中 n」两枚徽标 */
export function AiStatusBadge({
  sessionId,
  status,
}: {
  sessionId: string;
  status: SessionAiStatus | undefined;
}) {
  const t = useT();
  if (!status) return null;
  const openPending = (e: React.MouseEvent) => {
    e.stopPropagation();
    const first = useAppStore.getState().pendingApprovals.find((a) => a.sessionId === sessionId);
    if (first) useAiActivity.getState().focusApproval(first.id);
  };
  return (
    <>
      {status.pending > 0 && (
        <button
          type="button"
          className="shrink-0 rounded bg-amber-600 px-1 text-[10px] leading-4 text-white hover:bg-amber-500"
          title={t('chrome.aiPendingBadgeTitle')}
          onClick={openPending}
        >
          {t('chrome.aiPendingBadge', { count: status.pending })}
        </button>
      )}
      {status.running > 0 && (
        <span
          className="shrink-0 rounded bg-purple-700/80 px-1 text-[10px] leading-4 text-white"
          title={t('chrome.aiRunningBadgeTitle')}
        >
          {t('chrome.aiRunningBadge', { count: status.running })}
        </span>
      )}
    </>
  );
}
