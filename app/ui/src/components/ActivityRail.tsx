import { useEffect, useId } from 'react';
import { isLocalTarget, useAppStore } from '../state/app-store';
import { initBroadcastReceiver } from '../term/broadcast';
import { keymapFromSettings } from '../term/keymap';
import { useT } from '../i18n';
import { useAiActivity } from '../state/ai-activity';
import { Icon, type IconName } from './Icon';

/**
 * 右边缘竖向图标栏（VS Code 活动栏风格）：面板入口在上，全局开关（广播/AI 活动/设置）在下。
 * 面板按位置分流（见 app-store openDock）：SFTP/传输中心 → 底部 dock，监控 → 右侧面板，
 * 隧道 → 弹出面板；三处互不抢占，图标高亮即「该面板当前打开」。
 * 分屏入口在右键菜单与快捷键（keymap splitRow/splitCol），此处不直挂。
 */
export function ActivityRail() {
  const tabs = useAppStore((s) => s.tabs);
  const t = useT();
  const activeId = useAppStore((s) => s.activeId);
  const dockTab = useAppStore((s) => s.dockTab);
  const sideTab = useAppStore((s) => s.sideTab);
  const tunnelOpen = useAppStore((s) => s.tunnelOpen);
  const toggleDock = useAppStore((s) => s.toggleDock);
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const broadcastEnabled = useAppStore((s) => s.broadcastEnabled);
  const toggleBroadcast = useAppStore((s) => s.toggleBroadcast);
  const transferActive = useAppStore((s) => s.transferActive);
  const settings = useAppStore((s) => s.settings);
  const aiPanelOpen = useAiActivity((s) => s.panelOpen);
  const toggleAiPanel = useAiActivity((s) => s.togglePanel);
  const aiRunning = useAiActivity((s) => s.execs.length + s.jobs.length);
  const aiPending = useAppStore((s) => s.pendingApprovals.length);

  // 广播接收端注册（幂等；detached 子窗口各跑一份 App 同样生效）
  useEffect(() => {
    void initBroadcastReceiver();
  }, []);

  const activeTab = tabs.find((t) => t.id === activeId);
  const isSession = activeTab?.target.kind === 'session';
  // 本地会话（kind==='local'）：SFTP/监控/隧道等 SSH 专属功能禁用；广播天然可用
  const isLocal = isSession && !!activeTab && isLocalTarget(activeTab.target);
  const isRemote = isSession && !isLocal;
  const remoteHint = isRemote
    ? undefined
    : t(isLocal ? 'chrome.tipLocalUnavailable' : 'chrome.tipNeedRemote');

  // 快捷键提示：跟随设置中的方案/自定义绑定
  const bindings = keymapFromSettings(settings);

  return (
    <div
      className="myssh-tool flex w-12 shrink-0 flex-col items-center gap-1 border-l border-neutral-800 bg-neutral-900 py-2"
      role="toolbar"
      aria-orientation="vertical"
      aria-label={t('chrome.toolbarLabel')}
    >
      <RailBtn
        icon="folder"
        label={t('chrome.sftpFiles')}
        tip={t('chrome.sftpFiles')}
        kb={bindings['sftp']}
        hint={remoteHint}
        active={dockTab === 'sftp'}
        disabled={!isRemote}
        onClick={() => toggleDock('sftp')}
      />
      <RailBtn
        icon="activity"
        label={t('chrome.metricsLabel')}
        tip={t('chrome.tipMetrics', { kb: '' })}
        kb={bindings['metrics']}
        hint={remoteHint}
        active={sideTab === 'metrics'}
        disabled={!isRemote}
        onClick={() => toggleDock('metrics')}
      />
      <RailBtn
        icon="tunnel"
        label={t('chrome.tunnelsLabel')}
        tip={t('chrome.tipTunnels', { kb: '' })}
        kb={bindings['tunnels']}
        hint={remoteHint}
        active={tunnelOpen}
        disabled={!isRemote}
        toggleId="tunnel"
        onClick={() => toggleDock('tunnel')}
      />
      <RailBtn
        icon="transfer"
        label={t('chrome.transferLabel')}
        tip={t('chrome.tipTransfer', {
          active: transferActive ? t('chrome.transferActive', { count: transferActive }) : '',
        })}
        active={dockTab === 'transfer'}
        // dock 未停在传输页签时用角标报告进行中数量；打开后列表自带状态
        badge={dockTab !== 'transfer' && transferActive ? transferActive : undefined}
        onClick={() => toggleDock('transfer')}
      />

      <span className="flex-1" />

      <RailBtn
        icon="broadcast"
        label={t('chrome.broadcastLabel')}
        tip={t('chrome.tipBroadcast')}
        active={broadcastEnabled}
        onClick={toggleBroadcast}
      />
      <RailBtn
        icon="sparkles"
        label={t('chrome.aiActivityLabel')}
        tip={t('chrome.tipAiActivity', { running: aiRunning, pending: aiPending })}
        active={aiPanelOpen}
        badge={aiPending + aiRunning}
        onClick={toggleAiPanel}
      />
      <RailBtn
        icon="settings"
        label={t('chrome.settingsLabel')}
        tip={t('chrome.tipSettings', { kb: '' })}
        kb={bindings['settings']}
        active={settingsOpen}
        onClick={toggleSettings}
      />
    </div>
  );
}

/**
 * 图标栏按钮：36px 方形、20px 图标；激活时右缘出现强调条（面板就在它左侧展开）。
 * tooltip 自绘而非原生 title：原生 title 延迟约 1s 且禁用按钮上不弹，用户看不到禁用原因；
 * 外层 span 承接 hover，因为 disabled 的 button 不派发鼠标事件。
 */
function RailBtn({
  icon,
  label,
  tip,
  kb,
  hint,
  active,
  disabled,
  onClick,
  badge,
  toggleId,
}: {
  icon: IconName;
  label: string;
  tip: string;
  /** 快捷键（未绑定为空） */
  kb?: string;
  /** 禁用原因，仅 disabled 时显示 */
  hint?: string;
  active?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  /** 进行中数量角标（>0 才显示） */
  badge?: number;
  /** 弹出类面板的外部点击关闭需要排除自己的开关按钮 */
  toggleId?: string;
}) {
  const tipId = useId();
  return (
    <span className="group relative flex" data-rail-toggle={toggleId}>
      <button
        type="button"
        className={`relative flex h-9 w-9 items-center justify-center rounded-lg transition-colors focus-visible:ring-1 focus-visible:ring-blue-500/60 focus-visible:outline-none ${
          active
            ? 'bg-blue-600/20 text-blue-300'
            : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100'
        } disabled:cursor-not-allowed disabled:opacity-35 disabled:hover:bg-transparent disabled:hover:text-neutral-400`}
        disabled={disabled}
        onClick={onClick}
        aria-label={label}
        aria-pressed={active ?? false}
        aria-describedby={tipId}
      >
        <Icon name={icon} size={20} />
        {active && (
          <span className="absolute top-1.5 -right-1 bottom-1.5 w-0.5 rounded-full bg-blue-400" />
        )}
        {badge !== undefined && badge > 0 && (
          <span className="absolute top-0 right-0 flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-blue-600 px-1 text-[10px] leading-none font-semibold text-white ring-2 ring-neutral-900">
            {badge > 99 ? '99+' : badge}
          </span>
        )}
      </button>
      <span
        id={tipId}
        role="tooltip"
        className="pointer-events-none absolute top-1/2 right-full z-50 mr-2 flex -translate-y-1/2 flex-col gap-0.5 rounded-md border border-neutral-700 bg-neutral-900 px-2 py-1.5 text-xs whitespace-nowrap text-neutral-200 opacity-0 shadow-lg transition-opacity duration-100 group-focus-within:opacity-100 group-hover:opacity-100 group-hover:delay-300"
      >
        <span className="flex items-center gap-2">
          {tip}
          {kb && (
            <kbd className="rounded border border-neutral-700 bg-neutral-800 px-1 font-sans text-[11px] text-neutral-400">
              {kb}
            </kbd>
          )}
        </span>
        {disabled && hint && <span className="text-amber-400/90">{hint}</span>}
      </span>
    </span>
  );
}
