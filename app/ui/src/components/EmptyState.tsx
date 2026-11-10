import { useAppStore } from '../state/app-store';
import { GROUP_KEYS, readStringList } from '../state/groups';
import { KEY_ACTIONS, keymapFromSettings } from '../term/keymap';
import { useT } from '../i18n';
import { Icon } from './Icon';

/** 空态展示的常用快捷键（取生效绑定） */
const EMPTY_KEY_IDS = ['palette', 'newTab', 'closeTab', 'search', 'sftp'] as const;

/**
 * 空态（12.5）：无打开标签时的起始页。
 * 全部数据来自已加载的 sessions/settings，无任何新增订阅或轮询。
 */
export function EmptyState() {
  const openConnect = useAppStore((s) => s.openConnect);
  const t = useT();
  const toggleQuickConnect = useAppStore((s) => s.toggleQuickConnect);
  const togglePalette = useAppStore((s) => s.togglePalette);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const sessions = useAppStore((s) => s.sessions);
  const settings = useAppStore((s) => s.settings);

  const recent = readStringList(settings[GROUP_KEYS.recent])
    .slice(0, 3)
    .map((id) => sessions.find((s) => s.id === id))
    .filter((s) => s !== undefined);
  const bindings = keymapFromSettings(settings);
  const keyRows = EMPTY_KEY_IDS.map((id) => ({
    label: KEY_ACTIONS.find((a) => a.id === id)?.label ?? id,
    combo: bindings[id] ?? '',
  }));

  const btnCls =
    'flex items-center gap-2 rounded-lg border border-neutral-700 bg-neutral-800 px-4 py-2 text-[13px] text-neutral-200 hover:border-neutral-600 hover:bg-neutral-700 hover:text-neutral-100';

  return (
    <div className="flex h-full items-center justify-center" data-testid="empty-state">
      <div className="w-[420px] max-w-[90vw]">
        <div className="mb-6 flex flex-col items-center gap-3">
          <span className="myssh-logo flex h-12 w-12 items-center justify-center rounded-xl text-white">
            <Icon name="terminal" size={24} strokeWidth={2.25} />
          </span>
          <p className="text-[15px] font-medium text-neutral-200">{t('chrome.emptyTitle')}</p>
        </div>
        <div className="flex justify-center gap-2">
          <button
            className="flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-[13px] font-medium text-white shadow-sm hover:bg-blue-500"
            onClick={() => openConnect()}
          >
            <Icon name="plus" size={15} />
            {t('chrome.emptyNewServer')}
          </button>
          <button className={btnCls} onClick={toggleQuickConnect}>
            <Icon name="zap" size={14} />
            {t('chrome.emptyQuickConnect')}
          </button>
          <button className={btnCls} onClick={togglePalette}>
            <Icon name="command" size={14} />
            {t('chrome.emptyPalette')}
          </button>
        </div>
        {recent.length > 0 && (
          <div className="mt-8">
            <p className="mb-2 px-1 text-[11px] font-semibold tracking-wider text-neutral-500 uppercase">
              {t('chrome.emptyRecent')}
            </p>
            <ul className="overflow-hidden rounded-lg border border-neutral-800 bg-neutral-900">
              {recent.map((s) => (
                <li key={s.id} className="border-t border-neutral-800 first:border-t-0">
                  <button
                    className="flex w-full items-center gap-3 px-3 py-2 text-left text-[13px] text-neutral-200 hover:bg-neutral-800"
                    onClick={() => connectBySession(s.id, s.name)}
                  >
                    <Icon name="server" size={14} className="text-neutral-500" />
                    <span className="min-w-0 flex-1 truncate">{s.name}</span>
                    <span className="shrink-0 font-mono text-[11px] text-neutral-500">
                      {s.username}@{s.host}:{s.port}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        <div className="mt-8">
          <p className="mb-2 px-1 text-[11px] font-semibold tracking-wider text-neutral-500 uppercase">
            {t('chrome.emptyShortcuts')}
          </p>
          <ul className="space-y-1.5 px-1 text-xs text-neutral-400">
            {keyRows.map((r) => (
              <li key={r.label} className="flex items-center justify-between">
                <span>{r.label}</span>
                {r.combo && <kbd className="myssh-kbd">{r.combo}</kbd>}
              </li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
