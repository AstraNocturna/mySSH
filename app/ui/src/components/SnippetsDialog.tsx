import { useState } from 'react';
import { useAppStore } from '../state/app-store';
import { termRegistry } from '../term/registry';
import {
  SNIPPETS_KEY,
  insertSnippet,
  newSnippetId,
  readSnippets,
  validateSnippet,
  type Snippet,
} from '../term/snippets';
import { Dialog } from './Dialog';
import { tNow, useT } from '../i18n';

/** 插入到活跃标签的活跃 pane 并还焦点给终端；无终端时提示 */
export function insertIntoActivePane(s: Snippet): void {
  const st = useAppStore.getState();
  const tab = st.tabs.find((t) => t.id === st.activeId);
  const term = tab ? termRegistry.get(tab.activePaneId) : undefined;
  if (!term) {
    st.notify(tNow('chrome.snippetNoTerminal'), 'warning');
    return;
  }
  insertSnippet(term, s);
  term.focus();
}

type Draft = Pick<Snippet, 'id' | 'name' | 'command' | 'trigger' | 'run'>;

/** 命令片段管理：增删改 + 插入当前终端。存于 settings KV terminal.snippets */
export function SnippetsDialog() {
  const t = useT();
  const toggle = useAppStore((s) => s.toggleSnippets);
  const setSetting = useAppStore((s) => s.setSetting);
  const list = readSnippets(useAppStore((s) => s.settings[SNIPPETS_KEY]));
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);

  const save = () => {
    if (!draft) return;
    const trigger = draft.trigger?.trim() || undefined;
    const next: Snippet = { ...draft, name: draft.name.trim(), trigger };
    const err = validateSnippet(next, list);
    if (err) {
      setError(t(`chrome.snippetErr.${err}`));
      return;
    }
    const exists = list.some((s) => s.id === next.id);
    setSetting(
      SNIPPETS_KEY,
      exists ? list.map((s) => (s.id === next.id ? next : s)) : [...list, next],
    );
    setDraft(null);
    setError(null);
  };

  const input =
    'w-full rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-neutral-200 outline-none focus:border-neutral-500';

  return (
    <Dialog
      title={t('chrome.snippetsTitle')}
      onClose={toggle}
      panelClass="flex max-h-[80vh] w-[44rem] flex-col rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-200 shadow-xl"
    >
      <h2 className="mb-1 text-sm font-semibold text-neutral-100">{t('chrome.snippetsTitle')}</h2>
      <p className="mb-3 text-neutral-500">{t('chrome.snippetsHint')}</p>
      {draft && (
        <div className="mb-3 space-y-2 rounded border border-neutral-800 p-3">
          <div className="flex gap-2">
            <label className="flex-1">
              <span className="mb-0.5 block text-neutral-400">{t('chrome.snippetName')}</span>
              <input
                data-autofocus
                className={input}
                value={draft.name}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              />
            </label>
            <label className="w-56">
              <span className="mb-0.5 block text-neutral-400">{t('chrome.snippetTrigger')}</span>
              <input
                className={`${input} font-mono`}
                value={draft.trigger ?? ''}
                onChange={(e) => setDraft({ ...draft, trigger: e.target.value })}
              />
            </label>
          </div>
          <label className="block">
            <span className="mb-0.5 block text-neutral-400">{t('chrome.snippetCommand')}</span>
            <textarea
              className={`${input} h-24 resize-y font-mono`}
              spellCheck={false}
              value={draft.command}
              onChange={(e) => setDraft({ ...draft, command: e.target.value })}
            />
          </label>
          <div className="flex items-center justify-between">
            <label className="flex items-center gap-1.5 text-neutral-300">
              <input
                type="checkbox"
                checked={draft.run === true}
                onChange={(e) => setDraft({ ...draft, run: e.target.checked })}
              />
              {t('chrome.snippetRun')}
            </label>
            <div className="flex items-center gap-2">
              {error && <span className="text-red-300">{error}</span>}
              <button
                className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={() => {
                  setDraft(null);
                  setError(null);
                }}
              >
                {t('chrome.cancel')}
              </button>
              <button
                className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
                onClick={save}
              >
                {t('chrome.snippetSave')}
              </button>
            </div>
          </div>
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {list.length === 0 ? (
          <p className="text-neutral-500">{t('chrome.snippetsEmpty')}</p>
        ) : (
          <ul>
            {list.map((s) => (
              <li
                key={s.id}
                className="flex items-center gap-2 border-t border-neutral-800 py-1.5 first:border-t-0"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-neutral-100">{s.name}</span>
                    {s.trigger && (
                      <span className="shrink-0 rounded bg-neutral-800 px-1 font-mono text-neutral-400">
                        {s.trigger} ⇥
                      </span>
                    )}
                    {s.run && <span className="shrink-0 text-amber-300">↵</span>}
                  </div>
                  <div className="truncate font-mono text-neutral-500" title={s.command}>
                    {s.command.replace(/\n/g, ' ⏎ ')}
                  </div>
                </div>
                <button
                  className="shrink-0 rounded bg-neutral-700 px-2 py-0.5 hover:bg-neutral-600"
                  onClick={() => {
                    toggle();
                    insertIntoActivePane(s);
                  }}
                >
                  {t('chrome.snippetInsert')}
                </button>
                <button
                  className="shrink-0 rounded px-2 py-0.5 text-neutral-300 hover:bg-neutral-800"
                  onClick={() => {
                    setDraft({ ...s });
                    setError(null);
                  }}
                >
                  {t('chrome.snippetEdit')}
                </button>
                <button
                  className="shrink-0 rounded px-2 py-0.5 text-red-300 hover:bg-neutral-800"
                  onClick={() =>
                    setSetting(
                      SNIPPETS_KEY,
                      list.filter((x) => x.id !== s.id),
                    )
                  }
                >
                  {t('chrome.snippetDelete')}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="mt-3 flex justify-between">
        <button
          className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600 disabled:opacity-50"
          disabled={draft !== null}
          onClick={() => setDraft({ id: newSnippetId(), name: '', command: '', run: false })}
        >
          {t('chrome.snippetNew')}
        </button>
        <button
          className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
          onClick={toggle}
        >
          {t('dialogs.close')}
        </button>
      </div>
    </Dialog>
  );
}
