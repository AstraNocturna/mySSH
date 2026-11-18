import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { ConfirmDialog } from './ConfirmDialog';
import {
  START_MODE_LABEL,
  TUNNEL_KIND_KEY,
  TUNNEL_STATUS_KEY,
  fmtRate,
  startModeOf,
  tunnelDisplayName,
} from '../state/tunnel-utils';
import type { TunnelDef, TunnelInfo } from '../term/types';
import { useT } from '../i18n';

/**
 * 隧道面板（§9.1）：当前活动会话的隧道定义 × 1Hz 运行态合并视图。
 * 行操作：启动/停止/编辑/复制/删除；新建经 TunnelEditor（含端口预检与模板）。
 *
 * 只显示活动页签关联会话的隧道（隧道固定归属会话，跨会话全量列表无操作意义）。
 * 运行态无 sessionId 字段的临时（非持久化）隧道无法归属，批次二十九起已无
 * 产生路径（一切隧道必经定义），旧 adhoc 区块随之移除。
 *
 * 展现形式：TunnelPopover 弹出面板的内容（开关 app-store tunnelOpen）；
 * 编辑器/删除确认仍是 fixed 模态。每条隧道一张卡片，窄宽度下也不需要横向滚动。
 */
export function TunnelPanel() {
  const t = useT();
  const tunnels = useAppStore((s) => s.tunnels);
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const deleteTunnel = useAppStore((s) => s.deleteTunnel);
  const loadTunnelDefs = useAppStore((s) => s.loadTunnelDefs);
  const notify = useAppStore((s) => s.notify);
  const duplicateTunnel = useAppStore((s) => s.duplicateTunnel);

  /** 编辑器目标：undefined=关闭；{sessionId, def} def=null 为新建 */
  const [editor, setEditor] = useState<{ sessionId: string; def: TunnelDef | null } | undefined>(
    undefined,
  );
  const [pendingDelete, setPendingDelete] = useState<TunnelDef | null>(null);

  // 挂载（弹出面板打开）即拉取定义；1Hz 运行态由 App 级 subscribeTunnels 提供
  useEffect(() => {
    void loadTunnelDefs();
  }, [loadTunnelDefs]);

  // 活动页签关联的会话 id（快速连接等 spec 目标无档案 → null）
  const activeTab = tabs.find((t2) => t2.id === activeId);
  const sessionId = activeTab?.target.kind === 'session' ? activeTab.target.sessionId : null;
  const defs = sessionId ? tunnelDefs.filter((d) => d.sessionId === sessionId) : [];

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
  const statusLabel = (status: string) => {
    const k = TUNNEL_STATUS_KEY[status];
    return k ? t(k) : status;
  };
  const kindLabel = (kind: string) => {
    const k = TUNNEL_KIND_KEY[kind];
    return k ? t(k) : kind;
  };

  const duplicate = async (d: TunnelDef) => {
    try {
      await duplicateTunnel(d);
    } catch (e) {
      notify(t('panels.copyFailed', { error: String(e) }), 'error');
    }
  };

  const startDef = async (d: TunnelDef) => {
    try {
      await saveTunnel(d, true);
    } catch (e) {
      notify(t('panels.startFailed', { error: String(e) }), 'error');
    }
  };

  return (
    <div className="min-h-0 px-3 py-2 text-xs text-neutral-200">
      <div className="mb-2 flex items-center gap-3 text-neutral-500">
        <span className="min-w-0 flex-1 truncate" title={t('panels.tunnelHeaderNote')}>
          {t('panels.tunnelHeaderNote')}
        </span>
        {sessionId && (
          <button
            className="shrink-0 rounded-md bg-blue-600 px-2.5 py-1 text-white hover:bg-blue-500"
            onClick={() => setEditor({ sessionId, def: null })}
          >
            {t('panels.newTunnel')}
          </button>
        )}
      </div>

      {!sessionId ? (
        <p className="py-2 text-neutral-400">{t('panels.tunnelNoActiveSession')}</p>
      ) : defs.length === 0 ? (
        <p className="py-2 text-neutral-400">{t('panels.noTunnels')}</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {defs.map((d) => {
            const rt = runtimeById.get(d.id);
            const addr = `${d.bindHost}:${d.bindPort}${d.targetHost ? ` → ${d.targetHost}:${d.targetPort}` : ''}`;
            const dot = !rt
              ? 'bg-neutral-600'
              : rt.status === 'listening'
                ? 'bg-green-500'
                : rt.status === 'failed'
                  ? 'bg-red-500'
                  : 'bg-yellow-500';
            return (
              <li
                key={d.id}
                className="rounded-md border border-neutral-800 bg-neutral-950/40 px-3 py-2"
              >
                <div className="flex items-center gap-2">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${dot}`} />
                  <span
                    className="min-w-0 flex-1 truncate font-medium text-neutral-100"
                    title={tunnelDisplayName(d)}
                  >
                    {tunnelDisplayName(d)}
                  </span>
                  <span className="shrink-0 rounded bg-neutral-800 px-1.5 py-0.5 text-xs text-neutral-400">
                    {kindLabel(d.kind)}
                  </span>
                </div>
                <div className="mt-1 truncate font-mono text-xs text-neutral-400" title={addr}>
                  {addr}
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-x-3 text-xs text-neutral-500 tabular-nums">
                  <span className={rt?.status === 'failed' ? 'text-red-400' : undefined}>
                    {rt ? statusLabel(rt.status) : t('panels.tunnelNotRunning')}
                  </span>
                  <span>{START_MODE_LABEL[startModeOf(d)]}</span>
                  {rt && (
                    <>
                      <span>
                        ↑{fmtRate(rt.rateUp)} ↓{fmtRate(rt.rateDown)}
                      </span>
                      <span title={t('panels.colConnsHint')}>
                        {t('panels.connCount', { count: rt.activeConns })}
                      </span>
                    </>
                  )}
                </div>
                {rt?.lastError && (
                  <div className="mt-1 truncate text-xs text-red-400" title={rt.lastError}>
                    {rt.lastError}
                  </div>
                )}
                <div className="mt-2 flex items-center gap-1">
                  {rt ? (
                    <button
                      className="rounded-md border border-neutral-700 px-2 py-0.5 text-neutral-200 hover:border-red-700 hover:text-red-400"
                      onClick={() => void stopTunnel(d.id)}
                    >
                      {t('panels.stop')}
                    </button>
                  ) : (
                    <button
                      className="rounded-md border border-neutral-700 px-2 py-0.5 text-neutral-200 hover:border-green-700 hover:text-green-400"
                      onClick={() => void startDef(d)}
                    >
                      {t('panels.start')}
                    </button>
                  )}
                  <span className="flex-1" />
                  <button
                    className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
                    onClick={() => setEditor({ sessionId: d.sessionId, def: d })}
                  >
                    {t('panels.edit')}
                  </button>
                  <button
                    className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
                    onClick={() => void duplicate(d)}
                  >
                    {t('panels.duplicate')}
                  </button>
                  <button
                    className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-red-400"
                    onClick={() => setPendingDelete(d)}
                  >
                    {t('panels.delete')}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {editor && (
        <TunnelEditor
          sessionId={editor.sessionId}
          initial={editor.def}
          running={editor.def ? runtimeById.has(editor.def.id) : false}
          onClose={() => setEditor(undefined)}
        />
      )}
      {pendingDelete && (
        <ConfirmDialog
          title={t('panels.deleteTunnelTitle', { name: tunnelDisplayName(pendingDelete) })}
          confirmLabel={t('panels.delete')}
          onConfirm={() => {
            void deleteTunnel(pendingDelete.id)
              .then(() => notify(t('panels.tunnelDeleted'), 'success'))
              .catch((e) => notify(t('panels.deleteFailed', { error: String(e) }), 'error'));
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        >
          {t('panels.deleteTunnelBody', {
            addr: `${pendingDelete.bindHost}:${pendingDelete.bindPort}${pendingDelete.targetHost ? ` → ${pendingDelete.targetHost}:${pendingDelete.targetPort}` : ''}`,
          })}
        </ConfirmDialog>
      )}
    </div>
  );
}
