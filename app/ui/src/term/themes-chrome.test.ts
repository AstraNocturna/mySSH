//! deriveChromeVars 边界：非法/ shorthand 色值回退 + 深浅档派生方向。
import { describe, expect, it } from 'vitest';
import {
  BUILTIN_THEMES,
  DEFAULT_THEME_ID,
  SYSTEM_DEFAULTS,
  deriveChromeVars,
  type ThemeDef,
} from './themes';

const def = (over: Partial<ThemeDef['xterm']>, ui: 'dark' | 'light'): ThemeDef => ({
  id: 't',
  label: 't',
  ui,
  xterm: over,
});

describe('deriveChromeVars', () => {
  it('暗色主题：chrome 背景比终端背景更暗', () => {
    const v = deriveChromeVars(def({ background: '#282c34' }, 'dark'));
    expect(parseInt(v['--myssh-chrome-body'].slice(1), 16)).toBeLessThan(0x282c34);
  });

  it('亮色主题：强调色刻度含主题 blue', () => {
    const v = deriveChromeVars(def({ blue: '#268bd2' }, 'light'));
    expect(v['--myssh-chrome-accent']).toBe('#268bd2');
  });

  it('#rgb shorthand 与非法色回退默认值，不产出 NaN', () => {
    const v = deriveChromeVars(
      def({ background: '#abc', foreground: 'transparent', blue: 'red' }, 'dark'),
    );
    for (const val of Object.values(v)) expect(val).toMatch(/^#[0-9a-f]{6}$/);
  });

  it('全部内置主题派生合法色值', () => {
    for (const t of BUILTIN_THEMES) {
      for (const val of Object.values(deriveChromeVars(t))) expect(val).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it('主题显式 accent 优先于调色板 blue', () => {
    const v = deriveChromeVars({ ...def({ blue: '#268bd2' }, 'dark'), accent: '#cba6f7' });
    expect(v['--myssh-chrome-accent']).toBe('#cba6f7');
  });

  it('暗色分层：窗口底 < 侧栏 < 终端底 < 卡片/hover 层（亮度递增）', () => {
    const lum = (hex: string) => {
      const n = parseInt(hex.slice(1), 16);
      return ((n >> 16) & 0xff) + ((n >> 8) & 0xff) + (n & 0xff);
    };
    for (const t of BUILTIN_THEMES.filter((x) => x.ui === 'dark')) {
      const v = deriveChromeVars(t);
      const bg = t.xterm.background as string;
      expect(lum(v['--myssh-chrome-body'])).toBeLessThan(lum(v['--myssh-chrome-panel']));
      expect(lum(v['--myssh-chrome-panel'])).toBeLessThan(lum(bg));
      expect(lum(bg)).toBeLessThan(lum(v['--myssh-chrome-panel-2']));
    }
  });

  it('默认主题与系统深浅默认主题均为内置主题', () => {
    const ids = BUILTIN_THEMES.map((t) => t.id);
    expect(ids).toContain(DEFAULT_THEME_ID);
    expect(ids).toContain(SYSTEM_DEFAULTS.dark);
    expect(ids).toContain(SYSTEM_DEFAULTS.light);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
