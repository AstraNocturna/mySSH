import { useEffect, useRef } from 'react';
import { isLocalTarget, useAppStore } from '../state/app-store';
import { TunnelPanel } from './TunnelPanel';
import { Icon } from './Icon';
import { useT } from '../i18n';

/**
 * 隧道弹出面板：浮在终端区右上角（图标栏左侧），不占布局空间。
 * 隧道通常只是看状态、开关一下，不值得常驻一块区域。
 * 点面板外部 / Esc / 再点图标栏按钮关闭；有模态对话框（编辑器、删除确认）时不响应，
 * 否则点对话框会把承载它们的 TunnelPanel 一起卸载。
 */
export function TunnelPopover() {
  const open = useAppStore((s) => s.tunnelOpen);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
  const t = useT();
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = () => useAppStore.getState().closeDock('tunnel');
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Element | null;
      if (!target || ref.current?.contains(target)) return;
      if (target.closest('[data-rail-toggle="tunnel"]')) return;
      if (document.querySelector('.myssh-backdrop')) return;
      close();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !document.querySelector('.myssh-backdrop')) close();
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  if (!open || !activeId) return null;

  const activeTab = tabs.find((x) => x.id === activeId);
  const isLocal = activeTab ? isLocalTarget(activeTab.target) : false;

  return (
    <div
      ref={ref}
      role="dialog"
      aria-label={t('chrome.tunnelPopoverAria')}
      className="myssh-tool myssh-dialog-pop absolute top-2 right-14 z-30 flex max-h-[calc(100%-1rem)] w-[min(520px,calc(100%-4.5rem))] flex-col rounded-lg border border-neutral-700 bg-neutral-900 shadow-2xl"
    >
      <div className="flex shrink-0 items-center gap-2 border-b border-neutral-800 px-3 py-2 text-xs font-medium text-neutral-200">
        <Icon name="tunnel" size={15} />
        <span className="flex-1">{t('chrome.dockTunnel')}</span>
        <button
          type="button"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200 focus-visible:ring-1 focus-visible:ring-neutral-500"
          onClick={() => useAppStore.getState().closeDock('tunnel')}
          aria-label={t('chrome.tunnelPopoverClose')}
          title={t('chrome.tunnelPopoverClose')}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {isLocal ? (
          <div className="px-3 py-2 text-xs text-neutral-500">{t('chrome.dockLocalTunnel')}</div>
        ) : (
          <TunnelPanel />
        )}
      </div>
    </div>
  );
}
