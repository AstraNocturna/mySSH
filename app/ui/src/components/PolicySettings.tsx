import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { useT, type MsgKey } from '../i18n';
import { Icon } from './Icon';
import {
  POLICY_LIST_KEYS,
  addCommands,
  readPolicyList,
  sameAsDefault,
  type PolicyDefaults,
  type PolicyListKind,
} from './policy-lists';

const LISTS: {
  kind: PolicyListKind;
  title: MsgKey;
  hint: MsgKey;
  chip: string;
}[] = [
  {
    kind: 'blacklist',
    title: 'dialogs.policyBlacklist',
    hint: 'dialogs.policyBlacklistHint',
    chip: 'border-red-500/30 bg-red-500/10 text-red-300',
  },
  {
    kind: 'indirect',
    title: 'dialogs.policyIndirect',
    hint: 'dialogs.policyIndirectHint',
    chip: 'border-amber-500/30 bg-amber-500/10 text-amber-300',
  },
  {
    kind: 'whitelist',
    title: 'dialogs.policyWhitelist',
    hint: 'dialogs.policyWhitelistHint',
    chip: 'border-green-500/30 bg-green-500/10 text-green-300',
  },
];

/** 设置页「命令策略」：三份名单增删 + 恢复默认；后端每次 ssh_exec 现读，改完即生效 */
export function PolicySettings() {
  const t = useT();
  const [defaults, setDefaults] = useState<PolicyDefaults | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    invoke<PolicyDefaults>('policy_defaults')
      .then(setDefaults)
      .catch((e) => setLoadError(String(e)));
  }, []);

  return (
    <div className="flex flex-col gap-3">
      <p className="leading-5 text-neutral-400">{t('dialogs.policyIntro')}</p>
      {loadError && (
        <p className="text-red-300">{t('dialogs.policyLoadFailed', { msg: loadError })}</p>
      )}
      {LISTS.map((l) => (
        <PolicyListEditor key={l.kind} {...l} defaults={defaults?.[l.kind] ?? null} />
      ))}
    </div>
  );
}

function PolicyListEditor({
  kind,
  title,
  hint,
  chip,
  defaults,
}: {
  kind: PolicyListKind;
  title: MsgKey;
  hint: MsgKey;
  chip: string;
  defaults: string[] | null;
}) {
  const t = useT();
  const key = POLICY_LIST_KEYS[kind];
  const raw = useAppStore((s) => s.settings[key]);
  const setSetting = useAppStore((s) => s.setSetting);
  const deleteSetting = useAppStore((s) => s.deleteSetting);
  const notify = useAppStore((s) => s.notify);
  const [draft, setDraft] = useState('');

  const custom = readPolicyList(raw);
  const list = custom ?? defaults;
  const customized = custom !== null && !(defaults && sameAsDefault(custom, defaults));

  const save = (next: string[]) => setSetting(key, next);
  const add = () => {
    if (!list) return;
    const next = addCommands(list, draft);
    setDraft('');
    if (next.length !== list.length) save(next);
  };
  const reset = () => {
    deleteSetting(key).catch((e) => notify(String(e), 'error'));
  };

  return (
    <section className="rounded-lg border border-neutral-800 bg-neutral-950/40 p-4">
      <div className="mb-1 flex items-center gap-2">
        <h3 className="text-base font-semibold text-neutral-100">{t(title)}</h3>
        {list && <span className="tabular-nums text-neutral-500">{list.length}</span>}
        <span
          className={`rounded-full px-2 text-xs leading-5 ${
            customized ? 'bg-blue-600/20 text-blue-300' : 'bg-neutral-800 text-neutral-500'
          }`}
        >
          {customized ? t('dialogs.policyCustomized') : t('dialogs.policyDefault')}
        </span>
        <button
          type="button"
          className="ml-auto flex items-center gap-1.5 rounded-md px-2.5 py-1 text-neutral-300 hover:bg-neutral-800 hover:text-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent"
          disabled={custom === null}
          onClick={reset}
        >
          <Icon name="reset" size={12} />
          {t('dialogs.policyReset')}
        </button>
      </div>
      <p className="mb-2 leading-5 text-neutral-500">{t(hint)}</p>
      {list === null ? (
        <p className="text-neutral-600">{t('dialogs.policyLoading')}</p>
      ) : (
        <>
          <div className="mb-2 flex max-h-36 flex-wrap gap-1 overflow-y-auto">
            {list.length === 0 && (
              <span className="text-neutral-600">{t('dialogs.policyEmpty')}</span>
            )}
            {list.map((cmd) => (
              <span
                key={cmd}
                className={`flex items-center gap-0.5 rounded-md border py-1 pr-1 pl-2 font-mono text-xs ${chip}`}
              >
                {cmd}
                <button
                  type="button"
                  className="flex h-5 w-5 items-center justify-center rounded opacity-60 hover:bg-black/20 hover:opacity-100"
                  aria-label={t('dialogs.policyRemoveAria', { cmd })}
                  title={t('dialogs.policyRemoveAria', { cmd })}
                  onClick={() => save(list.filter((x) => x !== cmd))}
                >
                  <Icon name="x" size={10} strokeWidth={2.25} />
                </button>
              </span>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <input
              className="h-9 min-w-0 flex-1 rounded-md border border-neutral-700 bg-neutral-800 px-2.5 font-mono text-sm text-neutral-200"
              placeholder={t('dialogs.policyAddPh')}
              aria-label={t('dialogs.policyAddAria', { list: t(title) })}
              spellCheck={false}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  add();
                }
              }}
            />
            <button
              type="button"
              className="flex h-9 items-center gap-1.5 rounded-md bg-blue-600 px-4 text-white hover:bg-blue-500 disabled:opacity-40"
              disabled={!draft.trim()}
              onClick={add}
            >
              <Icon name="plus" size={12} strokeWidth={2.25} />
              {t('dialogs.policyAdd')}
            </button>
          </div>
        </>
      )}
    </section>
  );
}
