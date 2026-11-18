import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import {
  retryHistoryTransfer,
  transferCmd,
  transferJobCmd,
  useTransferStore,
} from '../state/transfer-store';
import type { TransferHistoryView, TransferJobView, TransferView } from '../term/types';
import { ConfirmDialog } from './ConfirmDialog';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { Icon, type IconName } from './Icon';
import { useT, type MsgKey } from '../i18n';

/** 传输管理中心（批次六 5）：跨 session 聚合全部传输任务。
 *  SftpPanel 只保留摘要入口，完整列表与逐任务控制集中在此。
 *  渲染位置：底部 dock 的「传输中心」页签内容（原右侧抽屉已并入 dock，全宽渲染）；
 *  开关经 app-store openDock/closeDock 同步 transfer-store.open，
 *  打开时为当前窗口全部 session 标签建立订阅（transfer-store 惰性去重）。
 *  展示：按状态分页签（全部/进行中/失败取消/已完成，带数量角标），任务行两行布局
 *  （文件名 + 状态 / 完整路径 + 大小速度），默认停在「进行中」。 */

type Bucket = 'active' | 'failed' | 'done';
type TabKey = 'all' | Bucket;

/** 状态 → 页签归属（取消与失败同属可重试组） */
function bucketOf(state: TransferView['state'] | TransferJobView['state']): Bucket {
  switch (state) {
    case 'done':
    case 'completed':
      return 'done';
    case 'failed':
    case 'canceled':
      return 'failed';
    default:
      return 'active';
  }
}

/** 数字部分格式化（模块级缓存；观感同 toFixed(1)/toFixed(2)） */
const sizeNum1 = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
});
const sizeNum2 = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${sizeNum1.format(n / 1024)} K`;
  if (n < 1024 * 1024 * 1024) return `${sizeNum1.format(n / 1048576)} M`;
  return `${sizeNum2.format(n / 1073741824)} G`;
}

/** 历史时间列格式化（MM-DD HH:mm 观感；模块级缓存） */
const histTimeFmt = new Intl.DateTimeFormat('en-US', {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  hourCycle: 'h23',
});

/** Intl → 固定 MM-DD HH:mm（不随语言环境分隔符漂移） */
function fmtHistTime(d: Date): string {
  const parts = histTimeFmt.formatToParts(d);
  const get = (t: string) => parts.find((p) => p.type === t)?.value ?? '';
  return `${get('month')}-${get('day')} ${get('hour')}:${get('minute')}`;
}

const STATE_KEY: Record<TransferView['state'], MsgKey> = {
  queued: 'panels.stateQueued',
  running: 'panels.stateRunning',
  paused: 'panels.statePaused',
  done: 'panels.stateDone',
  failed: 'panels.stateFailed',
  canceled: 'panels.stateCanceled',
};

const JOB_STATE_KEY: Record<TransferJobView['state'], MsgKey> = {
  scanning: 'panels.jobScanning',
  transferring: 'panels.jobTransferring',
  finalizing: 'panels.jobFinalizing',
  completed: 'panels.stateDone',
  failed: 'panels.stateFailed',
  canceled: 'panels.stateCanceled',
};

const STATE_COLOR: Record<Bucket | 'paused', string> = {
  active: 'text-blue-300',
  paused: 'text-amber-300',
  done: 'text-green-400',
  failed: 'text-red-400',
};

/** 行内图标按钮（≥28px 点击区） */
const iconBtn =
  'flex h-7 w-7 shrink-0 items-center justify-center rounded text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100';

function ActionBtn({
  icon,
  label,
  onClick,
  className = '',
}: {
  icon: IconName;
  label: string;
  onClick: () => void;
  className?: string;
}) {
  return (
    <button
      className={`${iconBtn} ${className}`}
      title={label}
      aria-label={label}
      onClick={onClick}
    >
      <Icon name={icon} size={16} />
    </button>
  );
}

function DirIcon({ direction, label }: { direction: 'upload' | 'download'; label: string }) {
  return (
    <span
      className={`mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md ${
        direction === 'upload' ? 'bg-sky-950/60 text-sky-400' : 'bg-emerald-950/60 text-emerald-400'
      }`}
      title={label}
    >
      <Icon name={direction === 'upload' ? 'upload' : 'download'} size={16} />
    </span>
  );
}

const rowBase = 'border-b border-neutral-800/70 px-2 py-2';

/** 目录任务行（PR-8/10）：父任务聚合展示——已发现/完成/失败/吞吐；扫描中总量未知显示 ?；
 *  ⓘ 展开在途文件与失败条目（失败条目单独保留，后端 cap 256；展示 cap 50 + 溢出计数）。 */
function JobRow({ j, sessionId }: { j: TransferJobView; sessionId: string }) {
  const tr = useT();
  const [showDetail, setShowDetail] = useState(false);
  const terminal = j.state === 'completed' || j.state === 'failed' || j.state === 'canceled';
  const hasDetail = j.current.length > 0 || j.failedEntries.length > 0;
  const bucket = bucketOf(j.state);
  const pct =
    j.state === 'completed'
      ? 100
      : j.scanDone && j.discoveredFiles > 0
        ? Math.min(100, (j.completedFiles / j.discoveredFiles) * 100)
        : 0;
  const stateText = j.paused
    ? tr('panels.statePaused')
    : !terminal && (j.rate ?? 0) > 0
      ? `${fmtSize(j.rate ?? 0)}/s`
      : tr(JOB_STATE_KEY[j.state]);
  return (
    <div className={rowBase}>
      <div className="flex items-start gap-3">
        <DirIcon
          direction={j.direction}
          label={j.direction === 'upload' ? tr('panels.upload') : tr('panels.download')}
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-3">
            <span
              className="min-w-0 flex-1 truncate font-medium text-neutral-100"
              title={j.summary}
            >
              {j.summary}
            </span>
            <span
              className={`shrink-0 tabular-nums ${j.paused ? STATE_COLOR.paused : STATE_COLOR[bucket]}`}
            >
              {stateText}
            </span>
          </div>
          <div className="mt-0.5 flex items-baseline gap-3 text-xs text-neutral-400">
            <span className="min-w-0 flex-1 truncate tabular-nums">
              {tr('panels.jobFiles', {
                done: j.completedFiles,
                total: j.scanDone ? j.discoveredFiles : '?',
              })}
              {' · '}
              {fmtSize(j.bytesDone)}
            </span>
            {j.failedFiles > 0 && (
              <span className="shrink-0 text-red-400">
                {tr('panels.jobFailedCount', { count: j.failedFiles })}
              </span>
            )}
          </div>
          {!terminal && (
            <div className="mt-1.5 h-1.5 overflow-hidden rounded bg-neutral-800">
              <div
                className="myssh-progress-running h-full bg-blue-600 transition-[width] duration-300"
                style={{ width: `${pct}%` }}
              />
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {!terminal && !j.paused && (
            <ActionBtn
              icon="pause"
              label={tr('panels.pause')}
              onClick={() => void transferJobCmd(sessionId, 'transfer_job_pause', j.id)}
            />
          )}
          {!terminal && j.paused && (
            <ActionBtn
              icon="play"
              label={tr('panels.resume')}
              onClick={() => void transferJobCmd(sessionId, 'transfer_job_resume', j.id)}
            />
          )}
          {!terminal && (
            <ActionBtn
              icon="x"
              label={tr('panels.cancel')}
              onClick={() => void transferJobCmd(sessionId, 'transfer_job_cancel', j.id)}
            />
          )}
          {terminal && (
            <ActionBtn
              icon="refresh"
              label={tr('panels.retryResume')}
              onClick={() => void transferJobCmd(sessionId, 'transfer_job_retry', j.id)}
            />
          )}
          {terminal && (
            <ActionBtn
              icon="trash"
              label={tr('panels.removeFromQueue')}
              onClick={() => void transferJobCmd(sessionId, 'transfer_job_remove', j.id)}
            />
          )}
          {hasDetail && (
            <ActionBtn
              icon="info"
              label={tr('panels.viewError')}
              className={showDetail ? 'bg-neutral-800 text-neutral-100' : ''}
              onClick={() => setShowDetail((v) => !v)}
            />
          )}
        </div>
      </div>
      {showDetail && (
        <div className="mt-1 ml-10 text-xs break-all">
          {j.current.length > 0 && (
            <div className="text-neutral-400">
              {tr('panels.jobCurrent')}: {j.current.join(' · ')}
            </div>
          )}
          {j.failedEntries.length > 0 && (
            <div className="text-red-400">
              {tr('panels.jobFailures')}:
              {j.failedEntries.slice(0, 50).map((f, i) => (
                <div key={i} className="ml-2">
                  {f.path} — {f.error}
                </div>
              ))}
              {j.failedFiles > j.failedEntries.length && (
                <div className="ml-2 text-neutral-400">
                  {tr('panels.jobMoreEntries', { count: j.failedFiles - j.failedEntries.length })}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function baseName(path: string): string {
  const norm = path.replace(/\\/g, '/').replace(/\/+$/, '');
  return norm.split('/').pop() || path;
}

/** 文件名展示：源/目标同名只显示一次，改名才显示 a → b */
function titleName(src: string, dst: string): string {
  const a = baseName(src);
  const b = baseName(dst);
  return a === b ? a : `${a} → ${b}`;
}

/** 在资源管理器中定位本地路径（上传=源文件，下载=目标文件，两侧都是本机路径） */
function revealLocal(
  path: string,
  notify: (msg: string, level: 'error') => void,
  failMsg: (e: string) => string,
): void {
  void invoke('open_in_explorer', { path }).catch((e) => notify(failMsg(String(e)), 'error'));
}

/** 历史记录行（失败/取消可一键重试续传；其余终态只读） */
function HistoryRow({ h, serverName }: { h: TransferHistoryView; serverName: string }) {
  const t = useT();
  const src = h.direction === 'upload' ? h.local : h.remote;
  const dst = h.direction === 'upload' ? h.remote : h.local;
  // SQLite UTC 时间串 → 本地 MM-DD HH:mm
  const d = new Date(`${h.updatedAt.replace(' ', 'T')}Z`);
  const time = Number.isNaN(d.getTime()) ? h.updatedAt : fmtHistTime(d);
  return (
    <div
      className={rowBase}
      title={h.error ? h.error : undefined}
      // 历史列表行：屏外行跳过渲染（两行布局 ≈ 58px）
      style={{ contentVisibility: 'auto', containIntrinsicSize: 'auto 58px' }}
    >
      <div className="flex items-start gap-3">
        <DirIcon
          direction={h.direction}
          label={h.direction === 'upload' ? t('panels.upload') : t('panels.download')}
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-3">
            <span className="min-w-0 flex-1 truncate font-medium text-neutral-100">
              {titleName(src, dst)}
            </span>
            <span className={`shrink-0 ${STATE_COLOR[bucketOf(h.state)]}`}>
              {t(STATE_KEY[h.state])}
            </span>
          </div>
          <div className="mt-0.5 flex items-baseline gap-3 text-xs text-neutral-400">
            <span className="min-w-0 flex-1 truncate" title={`${src}\n→ ${dst}`}>
              {src} → {dst}
            </span>
            <span className="max-w-32 shrink-0 truncate" title={serverName}>
              {serverName}
            </span>
            <span className="shrink-0 tabular-nums">{time}</span>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {(h.state === 'failed' || h.state === 'canceled') && (
            <ActionBtn
              icon="refresh"
              label={t('panels.retryResume')}
              onClick={() => void retryHistoryTransfer(h)}
            />
          )}
          <ActionBtn
            icon="folder"
            label={t('panels.revealLocal')}
            onClick={() =>
              revealLocal(h.local, useAppStore.getState().notify, (e) =>
                t('panels.revealFailed', { error: e }),
              )
            }
          />
        </div>
      </div>
    </div>
  );
}

function TransferRow({ t, sessionId }: { t: TransferView; sessionId: string }) {
  const tr = useT();
  const [showErr, setShowErr] = useState(false);
  // 终态闪烁（批次十 1）：state 转移进 done/failed 时整行底色闪一次。
  // 渲染期派生重置（React 官方模式）；挂载即终态（历史回放帧）不闪。
  // 动画播完底色自动回透明，无需计时器清 class；再次转移时类名变化即重播。
  const [seen, setSeen] = useState<{ state: string; flash: 'done' | 'failed' | null }>({
    state: t.state,
    flash: null,
  });
  if (seen.state !== t.state) {
    setSeen({
      state: t.state,
      flash: t.state === 'done' || t.state === 'failed' ? t.state : null,
    });
  }
  const flash = seen.flash;
  // 0 字节文件完成时应显示 100%（bytesTotal=0 会让公式恒得 0，「0% 完成」误导）
  const pct =
    t.state === 'done'
      ? 100
      : t.bytesTotal > 0
        ? Math.min(100, (t.bytesDone / t.bytesTotal) * 100)
        : 0;
  // upload: 源=本地 目标=远程；download: 源=远程 目标=本地
  const src = t.direction === 'upload' ? t.local : t.remote;
  const dst = t.direction === 'upload' ? t.remote : t.local;
  const terminal = t.state === 'done' || t.state === 'failed' || t.state === 'canceled';
  const notify = useAppStore((s) => s.notify);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const reveal = () => revealLocal(t.local, notify, (e) => tr('panels.revealFailed', { error: e }));
  const menuItems: MenuItem[] = [
    ...(!t.history && (t.state === 'queued' || t.state === 'running')
      ? [
          {
            label:
              t.direction === 'upload'
                ? tr('panels.prioritizeUpload')
                : tr('panels.prioritizeDownload'),
            icon: '⚡',
            disabled: t.priority === true,
            onSelect: () =>
              void transferCmd(sessionId, 'transfer_prioritize', { transferId: t.id }),
          },
        ]
      : []),
    { label: tr('panels.revealLocal'), icon: '📂', onSelect: reveal },
  ];
  const stateColor = t.state === 'paused' ? STATE_COLOR.paused : STATE_COLOR[bucketOf(t.state)];
  return (
    <div
      className={`myssh-row-in ${rowBase} ${
        flash === 'done' ? 'myssh-flash-done' : flash === 'failed' ? 'myssh-flash-fail' : ''
      }`}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      <div className="flex items-start gap-3">
        <DirIcon
          direction={t.direction}
          label={t.direction === 'upload' ? tr('panels.upload') : tr('panels.download')}
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-3">
            <span
              className="min-w-0 flex-1 truncate font-medium text-neutral-100"
              title={`${src}\n→ ${dst}`}
            >
              {titleName(src, dst)}
            </span>
            {!t.history && t.priority && (t.state === 'queued' || t.state === 'running') && (
              <span className="shrink-0 text-amber-400" title={tr('panels.prioritized')}>
                <Icon name="zap" size={14} />
              </span>
            )}
            <span className={`shrink-0 tabular-nums ${stateColor}`}>
              {t.state === 'running'
                ? `${pct.toFixed(0)}%`
                : t.state === 'paused' || t.state === 'queued'
                  ? `${tr(STATE_KEY[t.state])} ${pct.toFixed(0)}%`
                  : tr(STATE_KEY[t.state])}
            </span>
          </div>
          <div className="mt-0.5 flex items-baseline gap-3 text-xs text-neutral-400">
            <span className="min-w-0 flex-1 truncate" title={`${src}\n→ ${dst}`}>
              {src} → {dst}
            </span>
            <span className="shrink-0 tabular-nums">
              {t.bytesTotal > 0 || t.state === 'running'
                ? `${fmtSize(Math.min(t.bytesDone, t.bytesTotal || t.bytesDone))} / ${fmtSize(t.bytesTotal)}`
                : ''}
              {t.state === 'running' && ` · ${fmtSize(t.rate ?? 0)}/s`}
              {t.history && ` · ${tr('panels.lastTime')}`}
            </span>
          </div>
          {!terminal && (
            <div className="mt-1.5 h-1.5 overflow-hidden rounded bg-neutral-800">
              <div
                className={`h-full transition-[width] duration-300 ${
                  t.state === 'running' ? 'myssh-progress-running bg-blue-600' : 'bg-blue-600/70'
                }`}
                style={{ width: `${pct}%` }}
              />
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {!t.history && t.state === 'running' && (
            <ActionBtn
              icon="pause"
              label={tr('panels.pause')}
              onClick={() => void transferCmd(sessionId, 'transfer_pause', { transferId: t.id })}
            />
          )}
          {!t.history && t.state === 'paused' && (
            <ActionBtn
              icon="play"
              label={tr('panels.resume')}
              onClick={() => void transferCmd(sessionId, 'transfer_resume', { transferId: t.id })}
            />
          )}
          {!t.history &&
            (t.state === 'running' || t.state === 'queued' || t.state === 'paused') && (
              <ActionBtn
                icon="x"
                label={tr('panels.cancel')}
                onClick={() => void transferCmd(sessionId, 'transfer_cancel', { transferId: t.id })}
              />
            )}
          {!t.history && (t.state === 'failed' || t.state === 'canceled') && (
            <ActionBtn
              icon="refresh"
              label={tr('panels.retryResume')}
              onClick={() => void transferCmd(sessionId, 'transfer_retry', { transferId: t.id })}
            />
          )}
          {!t.history && t.error && (
            <ActionBtn
              icon="info"
              label={tr('panels.viewError')}
              className={showErr ? 'bg-neutral-800 text-neutral-100' : ''}
              onClick={() => setShowErr((v) => !v)}
            />
          )}
          {terminal && (
            <ActionBtn
              icon="trash"
              label={tr('panels.removeFromQueue')}
              onClick={() =>
                void (async () => {
                  await transferCmd(sessionId, 'transfer_remove', { transferId: t.id });
                  // 历史回放行不在队列、无事件流：删除后前端自行下账，否则看着像删不掉
                  if (t.history) useTransferStore.getState().dropTransfer(sessionId, t.id);
                })()
              }
            />
          )}
          <ActionBtn icon="folder" label={tr('panels.revealLocal')} onClick={reveal} />
        </div>
      </div>
      {showErr && t.error && (
        <div className="mt-1 ml-10 text-xs break-all text-red-400">{t.error}</div>
      )}
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} items={menuItems} onClose={() => setMenu(null)} />
      )}
    </div>
  );
}
const TAB_ORDER: { key: TabKey; label: MsgKey; empty: MsgKey }[] = [
  { key: 'all', label: 'panels.tabAll', empty: 'panels.noTransfers' },
  { key: 'active', label: 'panels.tabActive', empty: 'panels.noActiveTransfers' },
  { key: 'failed', label: 'panels.tabFailed', empty: 'panels.noFailedTransfers' },
  { key: 'done', label: 'panels.tabDone', empty: 'panels.noDoneTransfers' },
];

export function TransferCenter() {
  const t = useT();
  const open = useTransferStore((s) => s.open);
  const closeDock = useAppStore((s) => s.closeDock);
  const bySession = useTransferStore((s) => s.bySession);
  const jobsBySession = useTransferStore((s) => s.jobsBySession);
  const tabs = useAppStore((s) => s.tabs);
  const history = useTransferStore((s) => s.history);
  const clearHistory = useTransferStore((s) => s.clearHistory);
  const sessions = useAppStore((s) => s.sessions);
  const [confirmClear, setConfirmClear] = useState(false);
  const [tab, setTab] = useState<TabKey>('active');

  // Esc 关闭 dock
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeDock();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [open, closeDock]);

  if (!open) return null;

  const sessionIds = [
    ...new Set([...Object.keys(bySession), ...Object.keys(jobsBySession)]),
  ].filter((id) => (bySession[id]?.length ?? 0) > 0 || (jobsBySession[id]?.length ?? 0) > 0);
  // 总进度（批次十 1）：全部活动任务的字节合计，标题栏下一条动画总条
  const active = Object.values(bySession)
    .flat()
    .filter((t) => t.state === 'queued' || t.state === 'running' || t.state === 'paused');
  const sumTotal = active.reduce((n, t) => n + t.bytesTotal, 0);
  const sumDone = active.reduce((n, t) => n + Math.min(t.bytesDone, t.bytesTotal), 0);
  const sumRate = active
    .filter((t) => t.state === 'running')
    .reduce((n, t) => n + (t.rate ?? 0), 0);
  const totalPct = sumTotal > 0 ? Math.min(100, (sumDone / sumTotal) * 100) : 0;
  const titleOf = (sid: string): string =>
    tabs.find((t) => t.target.kind === 'session' && t.target.sessionId === sid)?.title ?? sid;
  // 本次运行的 live 项与历史表同源（终态落表），按 id 去重避免重复展示
  const liveIds = new Set(
    Object.values(bySession)
      .flat()
      .map((t) => t.id),
  );
  const hist = history.filter((h) => !liveIds.has(h.id));
  const serverName = (sid: string) => sessions.find((x) => x.id === sid)?.name ?? sid;
  /** 全局批量操作：逐 session 下发（pause_all/resume_all/clear 均为会话级命令） */
  const forEachSession = (cmd: string, extra: Record<string, unknown> = {}) => {
    for (const sid of sessionIds) void transferCmd(sid, cmd, extra);
  };

  const match = (b: Bucket) => tab === 'all' || tab === b;
  const counts: Record<TabKey, number> = { all: 0, active: 0, failed: 0, done: 0 };
  for (const list of Object.values(bySession))
    for (const x of list) {
      counts[bucketOf(x.state)]++;
      counts.all++;
    }
  for (const list of Object.values(jobsBySession))
    for (const j of list) {
      counts[bucketOf(j.state)]++;
      counts.all++;
    }
  for (const h of hist) {
    counts[bucketOf(h.state)]++;
    counts.all++;
  }
  const histShown = hist.filter((h) => match(bucketOf(h.state)));
  const groups = sessionIds
    .map((sid) => ({
      sid,
      jobs: (jobsBySession[sid] ?? []).filter((j) => match(bucketOf(j.state))),
      items: (bySession[sid] ?? []).filter((x) => match(bucketOf(x.state))),
    }))
    .filter((g) => g.jobs.length + g.items.length > 0);
  const empty = groups.length === 0 && histShown.length === 0;

  const qbtn =
    'flex h-8 shrink-0 items-center gap-1.5 rounded px-2.5 text-neutral-300 hover:bg-neutral-800 hover:text-neutral-100 disabled:opacity-40 disabled:hover:bg-transparent';
  const noSession = sessionIds.length === 0;

  return (
    <div className="flex h-full min-h-0 flex-col bg-neutral-900 text-sm text-neutral-200">
      <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-700 px-3 py-1.5 whitespace-nowrap">
        <span className="mr-2 font-semibold text-neutral-100">{t('panels.transferManager')}</span>
        <div role="tablist" className="flex items-center gap-1">
          {TAB_ORDER.map((x) => {
            const selected = tab === x.key;
            return (
              <button
                key={x.key}
                role="tab"
                aria-selected={selected}
                className={`flex h-8 items-center gap-1.5 rounded-md px-3 ${
                  selected
                    ? 'bg-neutral-700 text-neutral-50'
                    : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200'
                }`}
                onClick={() => setTab(x.key)}
              >
                {t(x.label)}
                {counts[x.key] > 0 && (
                  <span
                    className={`min-w-5 rounded-full px-1.5 text-center text-xs leading-5 tabular-nums ${
                      x.key === 'failed'
                        ? 'bg-red-900/70 text-red-200'
                        : x.key === 'active'
                          ? 'bg-blue-900/70 text-blue-200'
                          : 'bg-neutral-800 text-neutral-300'
                    }`}
                  >
                    {counts[x.key]}
                  </span>
                )}
              </button>
            );
          })}
        </div>
        <span className="ml-auto" />
        {(tab === 'all' || tab === 'active') && (
          <>
            <button
              className={qbtn}
              disabled={noSession}
              onClick={() => forEachSession('transfer_pause_all')}
            >
              <Icon name="pause" size={16} />
              {t('panels.pauseAll')}
            </button>
            <button
              className={qbtn}
              disabled={noSession}
              onClick={() => forEachSession('transfer_resume_all')}
            >
              <Icon name="play" size={16} />
              {t('panels.resumeAll')}
            </button>
          </>
        )}
        {(tab === 'all' || tab === 'done') && (
          <button
            className={qbtn}
            disabled={noSession}
            onClick={() => forEachSession('transfer_clear', { filter: 'done' })}
          >
            <Icon name="checkCircle" size={16} />
            {t('panels.clearCompleted')}
          </button>
        )}
        {(tab === 'all' || tab === 'failed') && (
          <button
            className={qbtn}
            disabled={noSession}
            onClick={() => forEachSession('transfer_clear', { filter: 'failed' })}
          >
            <Icon name="trash" size={16} />
            {t('panels.clearFailed')}
          </button>
        )}
        <button
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
          onClick={() => closeDock()}
          aria-label={t('panels.closeTransferManager')}
          title={t('panels.closeTransferManager')}
        >
          <Icon name="x" size={18} />
        </button>
      </div>
      {active.length > 0 && (
        <div className="flex items-center gap-3 border-b border-neutral-800 px-4 py-2">
          <div className="h-2 min-w-0 flex-1 overflow-hidden rounded bg-neutral-800">
            <div
              className="myssh-progress-running h-full bg-blue-600 transition-[width] duration-300"
              style={{ width: `${totalPct}%` }}
            />
          </div>
          <span className="shrink-0 text-xs text-neutral-400 tabular-nums">
            {sumRate > 0
              ? t('panels.transferSummaryRate', {
                  count: active.length,
                  done: fmtSize(sumDone),
                  total: fmtSize(sumTotal),
                  rate: fmtSize(sumRate),
                })
              : t('panels.transferSummary', {
                  count: active.length,
                  done: fmtSize(sumDone),
                  total: fmtSize(sumTotal),
                })}
          </span>
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-1">
        {empty && (
          <div className="px-1 py-6 text-center text-neutral-400">
            {t(TAB_ORDER.find((x) => x.key === tab)?.empty ?? 'panels.noTransfers')}
          </div>
        )}
        {groups.map((g) => (
          <div key={g.sid} className="mb-2">
            <div className="flex items-center gap-2 py-1.5 text-xs font-medium text-neutral-400">
              <Icon name="server" size={14} />
              {titleOf(g.sid)}
            </div>
            {g.jobs.map((j) => (
              <JobRow key={j.id} j={j} sessionId={g.sid} />
            ))}
            {g.items.map((x) => (
              <TransferRow key={x.id} t={x} sessionId={g.sid} />
            ))}
          </div>
        ))}
        {histShown.length > 0 && (
          <>
            <div className="mt-2 flex items-center gap-2 py-1 text-xs font-medium text-neutral-400">
              <Icon name="activity" size={14} />
              {t('panels.history')}
              <button
                className="ml-auto flex h-7 items-center gap-1.5 rounded px-2 text-neutral-300 hover:bg-neutral-800 hover:text-neutral-100 disabled:opacity-40"
                disabled={history.length === 0}
                onClick={() => setConfirmClear(true)}
              >
                <Icon name="trash" size={14} />
                {t('panels.clear')}
              </button>
            </div>
            {histShown.map((h) => (
              <HistoryRow key={h.id} h={h} serverName={serverName(h.sessionId)} />
            ))}
          </>
        )}
      </div>
      {confirmClear && (
        <ConfirmDialog
          title={t('panels.clearHistoryTitle')}
          confirmLabel={t('panels.clear')}
          onCancel={() => setConfirmClear(false)}
          onConfirm={() => {
            setConfirmClear(false);
            void clearHistory();
          }}
        >
          <p className="text-neutral-300">
            {t('panels.clearHistoryBody', { count: history.length })}
          </p>
          <p className="text-red-300">{t('panels.clearHistoryWarning')}</p>
        </ConfirmDialog>
      )}
    </div>
  );
}
