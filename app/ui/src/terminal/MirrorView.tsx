import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { useAppStore, type Pane, type Tab } from '../state/app-store';
import { fitRegistry, termRegistry } from '../term/registry';
import { DEFAULT_THEME_ID, resolveTheme } from '../term/themes';
import { effectiveXtermTheme, readTerminalSettings } from '../state/apply-settings';
import { feedOf } from '../term/exec-mirror';
import { ContextMenu, type MenuItem } from '../components/ContextMenu';
import { useT, tNow } from '../i18n';

interface MirrorRuntime {
  term: Terminal;
  fit: FitAddon;
  detach: () => void;
}

/** pane id → 镜像终端保活池：分屏结构变化导致重挂时复用同一 xterm，已显示的内容不丢 */
const mirrorRuntimes = new Map<string, MirrorRuntime>();

function createRuntime(paneId: string): MirrorRuntime {
  const settings = useAppStore.getState().settings;
  const opts = readTerminalSettings(settings);
  const term = new Terminal({
    scrollback: opts.scrollback,
    fontFamily: opts.fontFamily,
    fontSize: opts.fontSize,
    allowProposedApi: true,
    allowTransparency: true,
    disableStdin: true,
    cursorBlink: false,
    theme: effectiveXtermTheme(
      settings,
      resolveTheme(
        typeof settings['theme'] === 'string' ? settings['theme'] : DEFAULT_THEME_ID,
        typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : undefined,
      ),
    ),
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon());
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = '11';
  term.onSelectionChange(() => {
    if (useAppStore.getState().settings['terminal.copyOnSelect'] === false) return;
    const sel = term.getSelection();
    if (sel) void writeText(sel).catch(() => undefined);
  });
  // 隐藏光标：只读视图没有输入位置
  term.write(`\x1b[?25l\x1b[2m${tNow('chrome.mirrorBanner')}\x1b[0m\r\n\r\n`);
  const detach = feedOf(paneId).attach((s) => term.write(s));
  return { term, fit, detach };
}

/**
 * AI 命令镜像（只读分屏）：显示 AI 经 ssh_exec / recipe_run 在本会话执行的命令、输出与退出码。
 * 不连接、不接收键盘输入；内容来自 exec-mirror 输出流（挂载前的输出已缓冲）。
 */
export function MirrorView({ tab, pane }: { tab: Tab; pane: Pane }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; canCopy: boolean } | null>(null);
  const t = useT();

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let rt = mirrorRuntimes.get(pane.id);
    if (rt) {
      if (rt.term.element && rt.term.element.parentElement !== host)
        host.appendChild(rt.term.element);
    } else {
      rt = createRuntime(pane.id);
      mirrorRuntimes.set(pane.id, rt);
      rt.term.open(host);
    }
    const { term, fit } = rt;
    fit.fit();
    termRegistry.set(pane.id, term);
    fitRegistry.set(pane.id, () => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });
    host.oncontextmenu = (e) => {
      e.preventDefault();
      setMenu({ x: e.clientX, y: e.clientY, canCopy: term.hasSelection() });
    };
    const observer = new ResizeObserver(() => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });
    observer.observe(host);
    return () => {
      observer.disconnect();
      termRegistry.delete(pane.id);
      fitRegistry.delete(pane.id);
      // pane 仍在 store = 布局重排重挂，留池复用；已移除 = 真关闭，释放
      const alive = useAppStore.getState().tabs.some((x) => x.panes[pane.id] !== undefined);
      if (!alive) {
        const r = mirrorRuntimes.get(pane.id);
        mirrorRuntimes.delete(pane.id);
        r?.detach();
        r?.term.dispose();
      }
    };
  }, [pane.id]);

  const items = (canCopy: boolean): MenuItem[] => [
    {
      label: t('state.menu.copy'),
      icon: '⧉',
      disabled: !canCopy,
      onSelect: () => {
        const sel = termRegistry.get(pane.id)?.getSelection();
        if (sel) void writeText(sel).catch(() => undefined);
      },
    },
    { label: t('state.menu.clearScreen'), onSelect: () => termRegistry.get(pane.id)?.clear() },
    'separator',
    {
      label: t('chrome.mirrorClose'),
      icon: '✕',
      onSelect: () => useAppStore.getState().closePane(tab.id, pane.id),
    },
  ];

  return (
    <div className="relative h-full w-full">
      <div ref={hostRef} className="myssh-term-host relative h-full w-full p-1" />
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={items(menu.canCopy)}
        />
      )}
    </div>
  );
}
