import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import {
  activeSessionIds,
  fmtElapsed,
  useAiActivity,
  type AiStopKind,
  type AiTerminal,
} from '../state/ai-activity';
import { useT, type MsgKey } from '../i18n';

const TERM_LABEL: Record<AiTerminal['kind'], MsgKey> = {
  headless: 'chrome.aiTermHeadless',
  agent: 'chrome.aiTermAgent',
  shared: 'chrome.aiTermShared',
};

function firstLine(cmd: string): string {
  return cmd.split(/\r?\n/, 1)[0];
}

/**
 * AI 活动右侧浮动面板（工具栏「AI」开关）：按会话列出待确认、执行中命令、后台命令、
 * AI 终端，每项可停止。非模态，不挡终端操作。
 */
export function AiActivityPanel() {
  const t = useT();
  const open = useAiActivity((s) => s.panelOpen);
  const toggle = useAiActivity((s) => s.togglePanel);
  const execs = useAiActivity((s) => s.execs);
  const jobs = useAiActivity((s) => s.jobs);
  const terminals = useAiActivity((s) => s.terminals);
  const stop = useAiActivity((s) => s.stop);
  const focusApproval = useAiActivity((s) => s.focusApproval);
  const approvals = useAppStore((s) => s.pendingApprovals);
  const sessions = useAppStore((s) => s.sessions);
  const [now, setNow] = useState(() => Date.now());
  const ticking = open && execs.length + jobs.length > 0;

  useEffect(() => {
    if (!open) return;
    useAiActivity.getState().refresh();
  }, [open]);

  useEffect(() => {
    if (!ticking) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [ticking]);

  if (!open) return null;

  const activity = { execs, jobs, terminals };
  const ids = activeSessionIds(activity, approvals);
  const orphanTerms = terminals.filter((x) => x.sessionId === null);

  const doStop = (kind: AiStopKind, id: string) => {
    stop(kind, id).catch((e: unknown) =>
      useAppStore.getState().notify(t('chrome.aiStopFailed', { error: String(e) }), 'error'),
    );
  };

  const stopBtn = (kind: AiStopKind, id: string, label: MsgKey) => (
    <button
      type="button"
      className="shrink-0 rounded border border-red-800 px-1.5 text-[11px] text-red-300 hover:bg-red-900/60"
      onClick={() => doStop(kind, id)}
    >
      {t(label)}
    </button>
  );

  const termRow = (x: AiTerminal) => (
    <li key={x.terminalId} className="flex items-center gap-1.5">
      <span className="min-w-0 flex-1 truncate text-neutral-300">{t(TERM_LABEL[x.kind])}</span>
      {stopBtn(
        'terminal',
        x.terminalId,
        x.kind === 'shared' ? 'chrome.aiRevoke' : 'chrome.aiCloseTerm',
      )}
    </li>
  );

  return (
    <aside
      className="fixed right-2 top-[4.5rem] z-40 flex max-h-[calc(100vh-6rem)] w-80 flex-col rounded-lg border border-neutral-700 bg-neutral-900/95 text-xs text-neutral-200 shadow-xl"
      aria-label={t('chrome.aiActivityLabel')}
    >
      <header className="flex items-center justify-between border-b border-neutral-800 px-3 py-1.5">
        <span className="font-semibold">{t('chrome.aiActivityLabel')}</span>
        <button
          type="button"
          className="rounded px-1 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
          aria-label={t('chrome.aiPanelClose')}
          title={t('chrome.aiPanelClose')}
          onClick={toggle}
        >
          ×
        </button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {ids.length === 0 && orphanTerms.length === 0 && (
          <p className="px-1 py-4 text-center text-neutral-500">{t('chrome.aiPanelEmpty')}</p>
        )}
        {ids.map((sid) => {
          const rec = sessions.find((r) => r.id === sid);
          const pend = approvals.filter((a) => a.sessionId === sid);
          const ex = execs.filter((e) => e.sessionId === sid);
          const jb = jobs.filter((j) => j.sessionId === sid);
          const tm = terminals.filter((x) => x.sessionId === sid);
          const name = rec?.name ?? pend[0]?.sessionName ?? sid;
          return (
            <section key={sid} className="mb-2 rounded border border-neutral-800 bg-black/20 p-2">
              <div className="mb-1 flex items-center gap-1.5">
                <span
                  className="h-2 w-2 shrink-0 rounded-full"
                  style={{ backgroundColor: rec?.color ?? '#737373' }}
                />
                <span className="min-w-0 flex-1 truncate">
                  <span className="font-semibold">{name}</span>
                  {rec && (
                    <span className="ml-1 text-neutral-500">
                      {rec.username}@{rec.host}
                    </span>
                  )}
                </span>
                <button
                  type="button"
                  className="shrink-0 rounded border border-neutral-600 px-1.5 text-[11px] hover:bg-neutral-800 disabled:opacity-40"
                  title={t('chrome.aiJumpTitle')}
                  disabled={!rec}
                  onClick={() => rec && useAppStore.getState().connectOrActivate(rec.id, rec.name)}
                >
                  {t('chrome.aiJump')}
                </button>
              </div>
              <ul className="space-y-1">
                {pend.map((a) => (
                  <li key={a.id}>
                    <button
                      type="button"
                      title={a.command}
                      className="flex w-full items-center gap-1.5 rounded px-1 text-left hover:bg-neutral-800"
                      onClick={() => focusApproval(a.id)}
                    >
                      <span className="shrink-0 rounded bg-amber-600 px-1 text-[10px] text-white">
                        {t('chrome.aiPanelPending')}
                      </span>
                      <span className="min-w-0 flex-1 truncate font-mono">
                        {firstLine(a.command)}
                      </span>
                    </button>
                  </li>
                ))}
                {ex.map((e) => (
                  <li key={e.execId} className="flex items-center gap-1.5" title={e.command}>
                    <span className="shrink-0 rounded bg-purple-700/80 px-1 text-[10px] text-white">
                      {t('chrome.aiPanelExec')}
                    </span>
                    <span className="min-w-0 flex-1 truncate font-mono">
                      {firstLine(e.command)}
                    </span>
                    <span className="shrink-0 text-neutral-500">
                      {fmtElapsed(now - e.startedAt)}
                    </span>
                    {stopBtn('exec', e.execId, 'chrome.aiStop')}
                  </li>
                ))}
                {jb.map((j) => (
                  <li key={j.jobId} className="flex items-center gap-1.5" title={j.command}>
                    <span className="shrink-0 rounded bg-sky-800 px-1 text-[10px] text-white">
                      {t('chrome.aiPanelJob')}
                    </span>
                    <span className="min-w-0 flex-1 truncate font-mono">
                      {firstLine(j.command)}
                    </span>
                    <span className="shrink-0 text-neutral-500">
                      {fmtElapsed(now - j.startedAt)}
                    </span>
                    {stopBtn('job', j.jobId, 'chrome.aiStop')}
                  </li>
                ))}
                {tm.map(termRow)}
              </ul>
            </section>
          );
        })}
        {orphanTerms.length > 0 && (
          <section className="mb-2 rounded border border-neutral-800 bg-black/20 p-2">
            <div className="mb-1 font-semibold">{t('chrome.aiTempConn')}</div>
            <ul className="space-y-1">{orphanTerms.map(termRow)}</ul>
          </section>
        )}
      </div>
    </aside>
  );
}
