import { useState } from 'react';
import { useAppStore } from '../state/app-store';
import { GROUP_KEYS, isEmptyPolicy, readGroupPolicies } from '../state/groups';
import type { GroupPolicy } from '../term/bindings/GroupPolicy';
import { Dialog } from './Dialog';
import { MCP_LEVELS, MCP_PERM_GROUPS, setPermMode } from './mcp-options';
import { useT } from '../i18n';

const selectCls = 'w-44 rounded border border-neutral-700 bg-neutral-800 px-2 py-1';

/**
 * 分组 MCP 策略编辑：档位 / 生产标记 / 工具权限，各字段缺省 = 继续向上继承。
 * 写 settings 键 mcp.group_policies（后端每次调用现读，无需重启 MCP 服务）。
 */
export function GroupPolicyDialog({ path, onClose }: { path: string; onClose: () => void }) {
  const t = useT();
  const settings = useAppStore((s) => s.settings);
  const setSetting = useAppStore((s) => s.setSetting);
  const [draft, setDraft] = useState<GroupPolicy>(
    () => readGroupPolicies(settings[GROUP_KEYS.mcpGroupPolicies])[path] ?? { perms: {} },
  );

  const save = () => {
    const all = readGroupPolicies(useAppStore.getState().settings[GROUP_KEYS.mcpGroupPolicies]);
    const next = Object.fromEntries(Object.entries(all).filter(([k]) => k !== path));
    if (!isEmptyPolicy(draft)) next[path] = draft;
    setSetting(GROUP_KEYS.mcpGroupPolicies, next);
    onClose();
  };

  return (
    <Dialog
      title={t('dialogs.groupPolicyTitle', { path })}
      onClose={onClose}
      panelClass="w-[30rem] rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-200 shadow-xl"
    >
      <h2 className="mb-1 text-sm font-semibold text-neutral-100">
        {t('dialogs.groupPolicyTitle', { path: path.replace(/\//g, ' / ') })}
      </h2>
      <p className="mb-3 text-neutral-500">{t('dialogs.groupPolicyHint')}</p>
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-3">
          <span className="text-neutral-300">{t('dialogs.mcpLevel')}</span>
          <select
            className={selectCls}
            value={draft.level ?? 'inherit'}
            onChange={(e) =>
              setDraft((d) => ({
                ...d,
                level: e.target.value === 'inherit' ? null : e.target.value,
              }))
            }
          >
            <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
            {MCP_LEVELS.map(([v, k]) => (
              <option key={v} value={v}>
                {t(k)}
              </option>
            ))}
          </select>
        </div>
        <div className="flex items-center justify-between gap-3">
          <span className="text-neutral-300">{t('dialogs.mcpProduction')}</span>
          <select
            className={selectCls}
            value={draft.production == null ? 'inherit' : draft.production ? 'yes' : 'no'}
            onChange={(e) =>
              setDraft((d) => ({
                ...d,
                production: e.target.value === 'inherit' ? null : e.target.value === 'yes',
              }))
            }
          >
            <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
            <option value="yes">{t('dialogs.mcpProductionYes')}</option>
            <option value="no">{t('dialogs.mcpProductionNo')}</option>
          </select>
        </div>
        {MCP_PERM_GROUPS.map(([group, labelKey]) => {
          const v = draft.perms[group];
          return (
            <div key={group} className="flex items-center justify-between gap-3">
              <span className="text-neutral-300">{t(labelKey)}</span>
              <select
                className={selectCls}
                value={v === undefined ? 'inherit' : v ? 'allow' : 'deny'}
                onChange={(e) => {
                  const mode = e.target.value;
                  setDraft((d) => ({ ...d, perms: setPermMode(d.perms, group, mode) }));
                }}
              >
                <option value="inherit">{t('dialogs.mcpPermInherit')}</option>
                <option value="allow">{t('dialogs.mcpPermAllow')}</option>
                <option value="deny">{t('dialogs.mcpPermDeny')}</option>
              </select>
            </div>
          );
        })}
      </div>
      <div className="mt-4 flex justify-end gap-2">
        <button
          className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
          onClick={onClose}
        >
          {t('dialogs.cancel')}
        </button>
        <button
          className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
          onClick={save}
        >
          {t('dialogs.save')}
        </button>
      </div>
    </Dialog>
  );
}
