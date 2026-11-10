import { aiClientItems, useAppStore } from '../state/app-store';
import { useT } from '../i18n';
import { Dialog } from './Dialog';

/**
 * 没开长连接的 AI 客户端长时间无动作：无法确证它已关闭，由用户决定是否退出它的 AI 托管。
 * 弹出时用户可能正在终端里打字，初始焦点与 Enter/Esc 都落在「保留」，避免误清理。
 */
export function AiIdleDialog() {
  const t = useT();
  const prompt = useAppStore((s) => s.aiIdlePrompts[0]);
  if (!prompt) return null;
  const answer = (release: boolean) =>
    useAppStore.getState().answerAiIdle(prompt.clientId, release);
  const title = t('state.aiIdleTitle', {
    name: prompt.name || t('state.aiClientUnnamed'),
    minutes: prompt.idleMinutes,
  });
  return (
    <Dialog
      key={prompt.clientId}
      title={title}
      onClose={() => answer(false)}
      enterAction={() => answer(false)}
      panelClass="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
      <h2 className="mb-2 text-base font-semibold text-neutral-100">{title}</h2>
      <div className="mb-4 text-xs leading-5 text-neutral-400">
        <p className="mb-1 text-neutral-300">
          {t('state.aiIdleBody', { items: aiClientItems(prompt) })}
        </p>
        <p>{t('state.aiIdleHint')}</p>
      </div>
      <div className="flex justify-end gap-2">
        <button
          type="button"
          className="rounded bg-red-600 px-3 py-1 text-white hover:bg-red-500"
          onClick={() => answer(true)}
        >
          {t('state.aiIdleRelease')}
        </button>
        <button
          data-autofocus
          type="button"
          className="rounded bg-neutral-700 px-3 py-1 text-neutral-100 hover:bg-neutral-600"
          onClick={() => answer(false)}
        >
          {t('state.aiIdleKeep')}
        </button>
      </div>
    </Dialog>
  );
}
