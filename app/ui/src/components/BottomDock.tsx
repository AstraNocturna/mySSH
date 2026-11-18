import { useState } from 'react';
import { isLocalTarget, useAppStore, type BottomDockTab } from '../state/app-store';
import { keymapFromSettings } from '../term/keymap';
import { usePanelHeight } from './panel-height';
import { SftpPanel } from './SftpPanel';
import { TransferCenter } from './TransferCenter';
import { Icon, type IconName } from './Icon';
import { useT, type MsgKey } from '../i18n';

/** 底部 dock 页签（顺序即 tab 栏顺序）；labelKey 为 chrome 组文案键，icon 与图标栏入口一致 */
const DOCK_TABS: { id: BottomDockTab; labelKey: MsgKey; icon: IconName; keyId?: string }[] = [
  { id: 'sftp', labelKey: 'chrome.dockSftp', icon: 'folder', keyId: 'sftp' },
  { id: 'transfer', labelKey: 'chrome.transferLabel', icon: 'transfer' },
];

/** 最大化时占所在列的比例：给终端留出可见的一截，且无需隐藏终端（隐藏会打断 xterm 尺寸适配） */
const MAXIMIZED_HEIGHT = '72%';

/**
 * 底部 dock（Chrome DevTools 风格）：顶部 tab 栏（SFTP/传输中心 + 最大化 + 关闭），
 * 下方内容区按 app-store 的 dockTab 渲染对应面板；顶缘拖拽调高度（usePanelHeight）。
 * 监控与隧道不在此处（右侧面板 / 弹出面板），因此可与 SFTP 同时打开。
 *
 * SFTP 是 SSH 专属能力：活跃终端为本地会话时 tab 禁用（降透明度 + tooltip），
 * 已打开时内容区显示占位提示而非面板。
 */
export function BottomDock() {
  const dockTab = useAppStore((s) => s.dockTab);
  const t = useT();
  const toggleDock = useAppStore((s) => s.toggleDock);
  const closeDock = useAppStore((s) => s.closeDock);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
  const settings = useAppStore((s) => s.settings);
  const { height, handle } = usePanelHeight('ui.dockHeight', 400);
  const [maximized, setMaximized] = useState(false);

  if (!dockTab || !activeId) return null;

  const activeTab = tabs.find((t) => t.id === activeId);
  // 本地会话：SFTP tab 禁用；dock 已停在此页签时内容区给占位提示
  const isLocal = activeTab ? isLocalTarget(activeTab.target) : false;

  const bindings = keymapFromSettings(settings);
  const kb = (id?: string) => (id && bindings[id] ? t('chrome.kbHint', { key: bindings[id] }) : '');
  const maxLabel = t(maximized ? 'chrome.dockRestore' : 'chrome.dockMaximize');

  return (
    <div
      className="myssh-tool flex shrink-0 flex-col border-t border-neutral-700 bg-neutral-900"
      style={{ height: maximized ? MAXIMIZED_HEIGHT : height }}
    >
      {!maximized && handle}
      {/* tab 栏：页签 + 右侧最大化/关闭；双击空白处切换最大化 */}
      <div
        className="flex shrink-0 items-center gap-0.5 border-b border-neutral-800 px-2"
        role="tablist"
        aria-label={t('chrome.dockTablistAria')}
        onDoubleClick={(e) => {
          if (e.target === e.currentTarget) setMaximized((m) => !m);
        }}
      >
        {DOCK_TABS.map((tab) => {
          const label = t(tab.labelKey);
          const disabled = tab.id === 'sftp' && isLocal;
          const active = dockTab === tab.id;
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={active}
              aria-label={t('chrome.dockTabAria', { label })}
              disabled={disabled}
              title={
                disabled
                  ? t('chrome.dockTabDisabledTip', { label })
                  : t('chrome.dockTabTip', { label, kb: kb(tab.keyId) })
              }
              className={`mt-1 flex items-center gap-1.5 rounded-t px-3 py-1 text-xs focus-visible:ring-1 focus-visible:ring-neutral-500 ${
                active
                  ? 'bg-neutral-800 font-medium text-neutral-100'
                  : 'text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200'
              } disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent`}
              onClick={() => toggleDock(tab.id)}
            >
              <Icon name={tab.icon} size={14} />
              {label}
            </button>
          );
        })}
        <span className="flex-1" />
        <button
          type="button"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200 focus-visible:ring-1 focus-visible:ring-neutral-500"
          onClick={() => setMaximized((m) => !m)}
          aria-label={maxLabel}
          title={maxLabel}
        >
          <Icon name={maximized ? 'minimize' : 'maximize'} size={14} />
        </button>
        <button
          type="button"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200 focus-visible:ring-1 focus-visible:ring-neutral-500"
          onClick={() => closeDock()}
          aria-label={t('chrome.dockClose')}
          title={t('chrome.dockClose')}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
      {/* 内容区 */}
      <div className="min-h-0 flex-1">
        {dockTab === 'sftp' && isLocal ? (
          <div className="px-3 py-2 text-xs text-neutral-500">{t('chrome.dockLocalSftp')}</div>
        ) : dockTab === 'sftp' ? (
          <SftpPanel tabId={activeId} />
        ) : (
          <TransferCenter />
        )}
      </div>
    </div>
  );
}
