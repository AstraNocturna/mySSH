import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { useAppStore, type Pane, type Tab } from '../state/app-store';
import { fitRegistry, termRegistry } from '../term/registry';
import { resolveTheme } from '../term/themes';
import { effectiveXtermTheme, readTerminalSettings } from '../state/apply-settings';
import { feedOf } from '../term/exec-mirror';
import { ContextMenu, type MenuItem } from '../components/ContextMenu';
import { useT, tNow } from '../i18n';

/**
 * AI 命令镜像视图（只读）：显示 AI 经 ssh_exec / recipe_run 执行的命令、输出与退出码。
 * 不连接、不接收键盘输入；内容来自 exec-mirror 输出流（挂载前的输出已缓冲）。
 */
export function MirrorView({ tab, pane }: { tab: Tab; pane: Pane }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; canCopy: boolean } | null>(null);
  const t = useT();

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
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
          typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark',
          typeof settings['theme.customJson'] === 'string'
            ? settings['theme.customJson']
            : undefined,
        ),
      ),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    const unicode = new Unicode11Addon();
    term.loadAddon(unicode);
    term.unicode.activeVersion = '11';
    term.open(host);
    fit.fit();
    // 隐藏光标：只读视图没有输入位置
    term.write(`\x1b[?25l\x1b[2m${tNow('chrome.mirrorBanner')}\x1b[0m\r\n\r\n`);
    const detach = feedOf(tab.id).attach((s) => term.write(s));
    termRegistry.set(pane.id, term);
    fitRegistry.set(pane.id, () => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });
    term.onSelectionChange(() => {
      if (useAppStore.getState().settings['terminal.copyOnSelect'] === false) return;
      const sel = term.getSelection();
      if (sel) void writeText(sel).catch(() => undefined);
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
      detach();
      termRegistry.delete(pane.id);
      fitRegistry.delete(pane.id);
      term.dispose();
    };
  }, [tab.id, pane.id]);

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
      label: t('chrome.menuClose'),
      icon: '✕',
      onSelect: () => useAppStore.getState().closeTab(tab.id),
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
