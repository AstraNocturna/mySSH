import { isLocalTarget, useAppStore } from '../state/app-store';
import { usePanelWidth } from './panel-height';
import { MetricsPanel } from './MetricsPanel';
import { useT } from '../i18n';

/**
 * 右侧面板（监控）：与底部 dock 互不抢占，可同时打开；左缘拖拽调宽。
 * 监控是 SSH 专属能力：活跃终端为本地会话时内容区显示占位提示。
 */
export function SideDock() {
  const sideTab = useAppStore((s) => s.sideTab);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
  const t = useT();
  const { width, handle } = usePanelWidth('ui.sideDockWidth', 340);

  if (!sideTab || !activeId) return null;

  const activeTab = tabs.find((x) => x.id === activeId);
  const isLocal = activeTab ? isLocalTarget(activeTab.target) : false;

  return (
    <aside
      className="myssh-tool relative flex shrink-0 flex-col border-l border-neutral-700 bg-neutral-900"
      style={{ width }}
      aria-label={t('chrome.sideDockAria')}
    >
      {handle}
      <div className="min-h-0 flex-1">
        {isLocal ? (
          <div className="px-3 py-2 text-xs text-neutral-500">{t('chrome.dockLocalMetrics')}</div>
        ) : (
          <MetricsPanel tabId={activeId} />
        )}
      </div>
    </aside>
  );
}
