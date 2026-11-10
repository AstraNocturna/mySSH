import { useEffect, useState } from 'react';
import { useAppStore, type McpApprovalRequest } from '../state/app-store';
import { groupApprovals, useAiActivity } from '../state/ai-activity';
import { Dialog } from './Dialog';
import { useT } from '../i18n';
import { summarizeParts } from './approval-help';

/** 剩余毫秒；now 可能早于收到请求（计时器在首个请求到达前停着）：不超过总时长 */
function remainingMs(a: McpApprovalRequest, now: number): number {
  return Math.min(a.expiresInMs, a.receivedAt + a.expiresInMs - now);
}

function firstLine(cmd: string): string {
  return cmd.split(/\r?\n/, 1)[0];
}

/**
 * MCP 危险命令审批弹窗（安全模型第 2 条的人工确认通道）。
 * 与 HostKeyDialog 同一防误触规则：默认焦点在「拒绝」、Esc = 拒绝、点遮罩不关闭；
 * 到期（后端 TTL，从请求时刻起算）未决自动按拒绝出队。
 * 多会话并发时左栏按会话分组列出全部待确认项；批量操作只有「拒绝本会话全部」，允许必须逐条。
 * 「自动执行」勾选框：勾上 3 秒后自动批准，并对本会话开启自动执行——后续弹框预勾选并倒计时
 *（高危命令除外，AI 客户端断开失效）；倒计时中取消勾选即关掉。
 */
export function ApprovalDialog() {
  const t = useT();
  const approvals = useAppStore((s) => s.pendingApprovals);
  const sessions = useAppStore((s) => s.sessions);
  const answer = useAppStore((s) => s.answerApproval);
  const focus = useAiActivity((s) => s.approvalFocus);
  const focusApproval = useAiActivity((s) => s.focusApproval);
  const hidden = useAiActivity((s) => s.approvalHidden);
  const setHidden = useAiActivity((s) => s.setApprovalHidden);
  const setAuto = useAppStore((s) => s.setApprovalAuto);
  const [now, setNow] = useState(() => Date.now());
  const any = approvals.length > 0;

  useEffect(() => {
    if (!any) return;
    // 250ms：3 秒自动执行的倒计时要按时触发、读秒不跳
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [any]);

  // 未选中、收起状态下的项同样到期出队；勾了「自动执行」的到点批准并记住
  useEffect(() => {
    approvals.forEach((a) => {
      if (a.autoAt !== undefined && now >= a.autoAt) answer(a.id, true, true);
      else if (remainingMs(a, now) <= 0) answer(a.id, false);
    });
  }, [approvals, now, answer]);

  if (!any) return null;
  const current = approvals.find((a) => a.id === focus) ?? approvals[0];

  if (hidden) {
    return (
      <div className="fixed left-1/2 top-11 z-40 flex -translate-x-1/2 items-center gap-2 rounded-full border border-amber-600 bg-amber-950/95 px-3 py-1 text-xs text-amber-100 shadow-lg">
        {t('dialogs.approvalReminder', { count: approvals.length })}
        <button
          type="button"
          className="rounded-full bg-amber-600 px-2 py-0.5 text-white hover:bg-amber-500"
          onClick={() => focusApproval(current.id)}
        >
          {t('dialogs.approvalReopen')}
        </button>
      </div>
    );
  }

  const groups = groupApprovals(approvals);
  const sameSession = groups.find((g) => g.sessionId === current.sessionId)?.items ?? [current];
  const recOf = (id: string) => sessions.find((r) => r.id === id);
  const rec = recOf(current.sessionId);
  const name = current.sessionName ?? rec?.name ?? current.sessionId;
  const user = current.user ?? rec?.username ?? '';
  const host = current.host ?? rec?.host ?? '';
  const group = rec?.groupPath ? rec.groupPath.replace(/\//g, ' / ') : t('chrome.ungrouped');
  const deny = () => answer(current.id, false);
  const prod = current.production === true;
  const summary = current.parts?.length ? summarizeParts(current.parts, t, current.command) : null;
  const autoAt = current.autoAt;
  const autoOn = autoAt !== undefined;
  const secLeft = Math.max(0, Math.ceil(remainingMs(current, now) / 1000));
  const multi = approvals.length > 1;
  const jump = () => {
    if (!rec) return;
    useAppStore.getState().connectOrActivate(rec.id, rec.name);
    setHidden(true);
  };

  return (
    <Dialog
      // 换题即重挂载：焦点重新落到「拒绝」，Esc 拒绝的也是当前这条
      key={current.id}
      title={t('dialogs.approvalTitle')}
      onClose={deny}
      closeOnBackdrop={false}
      backdropClass="z-20"
      panelClass={`flex gap-3 rounded-lg border p-4 text-sm shadow-xl ${
        multi ? 'w-[54rem]' : 'w-[36rem]'
      } ${
        prod
          ? 'border-red-700 bg-red-950 text-red-100'
          : 'border-amber-700 bg-neutral-900 text-neutral-200'
      }`}
    >
      {multi && (
        <aside className="max-h-[70vh] w-56 shrink-0 overflow-y-auto border-r border-neutral-700/70 pr-2">
          <div className="mb-1 text-xs text-neutral-400">
            {t('dialogs.approvalListTitle', { count: approvals.length })}
          </div>
          {groups.map((g) => {
            const r = recOf(g.sessionId);
            const gProd = g.items[0].production === true;
            return (
              <div key={g.sessionId} className="mb-2">
                <div
                  className={`flex items-center gap-1 text-xs ${gProd ? 'text-red-300' : 'text-neutral-300'}`}
                >
                  <span
                    className="h-2 w-2 shrink-0 rounded-full"
                    style={{ backgroundColor: r?.color ?? '#737373' }}
                  />
                  <span className="truncate">
                    {g.items[0].sessionName ?? r?.name ?? g.sessionId}
                  </span>
                  <span className="shrink-0 text-neutral-500">{g.items.length}</span>
                </div>
                {g.items.map((a) => (
                  <button
                    key={a.id}
                    type="button"
                    title={a.command}
                    onClick={() => focusApproval(a.id)}
                    className={`mt-0.5 flex w-full items-center gap-1 rounded px-1.5 py-0.5 text-left font-mono text-[11px] ${
                      a.id === current.id
                        ? 'bg-amber-700/60 text-white'
                        : 'text-neutral-400 hover:bg-neutral-800'
                    }`}
                  >
                    <span className="min-w-0 flex-1 truncate">{firstLine(a.command)}</span>
                    <span className="shrink-0 text-[10px] text-neutral-500">
                      {Math.max(0, Math.ceil(remainingMs(a, now) / 1000))}s
                    </span>
                  </button>
                ))}
              </div>
            );
          })}
        </aside>
      )}
      <div className="min-w-0 flex-1">
        <h2 className="mb-2 text-base font-semibold">{t('dialogs.approvalTitle')}</h2>
        <div
          className={`mb-2 flex items-center gap-2 rounded border px-2 py-1.5 ${
            prod ? 'border-red-600 bg-red-900/60' : 'border-neutral-700 bg-black/30'
          }`}
        >
          <span
            className="h-3 w-3 shrink-0 rounded-full"
            style={{ backgroundColor: rec?.color ?? '#737373' }}
          />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="truncate text-sm font-semibold">{name}</span>
              {prod && (
                <span className="shrink-0 rounded bg-red-600 px-1 text-[11px] font-semibold text-white">
                  {t('dialogs.approvalProduction')}
                </span>
              )}
            </div>
            <div className="truncate text-xs text-neutral-400">
              {user}@{host} · {t('dialogs.approvalGroup', { group })}
              {current.level && <> · {t('dialogs.approvalLevel', { level: current.level })}</>}
            </div>
          </div>
          <button
            type="button"
            className="shrink-0 rounded border border-neutral-600 px-2 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-40"
            title={t('dialogs.approvalJumpTitle')}
            disabled={!rec}
            onClick={jump}
          >
            {t('dialogs.approvalJump')}
          </button>
        </div>
        {current.purpose && (
          <div className="mb-2 rounded border border-sky-800/60 bg-sky-950/40 p-2 text-xs">
            <div className="mb-0.5 text-sky-300">{t('dialogs.approvalPurpose')}</div>
            <div className="whitespace-pre-wrap break-words text-neutral-200">
              {current.purpose}
            </div>
          </div>
        )}
        <pre className="mb-2 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-black/50 p-2 font-mono text-xs">
          {current.command}
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
        <p className="mb-3 text-xs text-neutral-400">{t('dialogs.approvalHint')}</p>
        <label
          className={`mb-3 flex items-center gap-2 text-xs ${
            current.highRisk ? 'text-neutral-500' : 'cursor-pointer'
          }`}
          title={t('dialogs.approvalAlwaysHint')}
        >
          <input
            type="checkbox"
            className="accent-amber-500"
            checked={autoOn}
            disabled={current.highRisk === true}
            onChange={(e) => setAuto(current.id, e.target.checked)}
          />
          <span>{t('dialogs.approvalAlways')}</span>
          {current.highRisk && <span>{t('dialogs.approvalAutoBlocked')}</span>}
          {autoOn && (
            <span className="rounded bg-amber-700/70 px-1.5 text-amber-50">
              {t('dialogs.approvalAutoIn', {
                sec: Math.max(0, Math.ceil((autoAt - now) / 1000)),
              })}
            </span>
          )}
        </label>
        <div className="flex items-center justify-end gap-2">
          {sameSession.length > 1 && (
            <button
              type="button"
              className="mr-auto rounded border border-neutral-600 px-3 py-1 text-xs hover:bg-neutral-800"
              onClick={() => sameSession.forEach((a) => answer(a.id, false))}
            >
              {t('dialogs.approvalDenySession', { count: sameSession.length })}
            </button>
          )}
          <button
            data-autofocus
            className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
            onClick={deny}
          >
            {t('dialogs.approvalDeny', { sec: secLeft })}
          </button>
          <button
            className={`rounded px-3 py-1 text-white ${
              prod ? 'bg-red-600 hover:bg-red-500' : 'bg-amber-600 hover:bg-amber-500'
            }`}
            onClick={() => answer(current.id, true, autoOn)}
          >
            {t(autoOn ? 'dialogs.approvalAllowNow' : 'dialogs.approvalAllow')}
          </button>
        </div>
        <p className="mt-2 text-right text-[11px] text-neutral-500">
          {t('dialogs.approvalAlwaysHint')}
        </p>
      </div>
    </Dialog>
  );
}
