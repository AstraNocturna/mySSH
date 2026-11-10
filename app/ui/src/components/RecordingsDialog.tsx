import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { Terminal } from '@xterm/xterm';
import { useAppStore } from '../state/app-store';
import { readTerminalSettings } from '../state/apply-settings';
import { DEFAULT_THEME_ID, resolveTheme } from '../term/themes';
import {
  framesUntil,
  parseCast,
  parseSize,
  timeline,
  type Cast,
  type Frame,
} from '../term/asciicast';
import { Dialog } from './Dialog';
import { useT } from '../i18n';

interface RecordingMeta {
  id: string;
  title: string;
  source: 'user' | 'agent';
  sessionId: string | null;
  cols: number;
  rows: number;
  startedAt: number;
  endedAt: number | null;
  durationMs: number;
  bytes: number;
  sha256: string | null;
  truncated: boolean;
}

interface VerifyReport {
  ok: boolean;
  reason: string;
  fileSha256: string;
  auditSha256: string | null;
}

interface ChainReport {
  ok: boolean;
  checked: number;
  legacy: number;
  firstBadId: number | null;
}

/** 播放时空闲间隔上限（秒）：长时间无输出直接跳过 */
const IDLE_CAP = 2;
const SPEEDS = [1, 2, 4, 8];

const fmtDuration = (ms: number) => {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};
const fmtSize = (b: number) =>
  b >= 1024 * 1024
    ? `${(b / 1024 / 1024).toFixed(1)} MB`
    : b >= 1024
      ? `${Math.round(b / 1024)} KB`
      : `${b} B`;

/** 会话录像：列表 / 播放 / 导出文本 / 防篡改校验 / 删除 */
export function RecordingsDialog() {
  const t = useT();
  const toggle = useAppStore((s) => s.toggleRecordings);
  const notify = useAppStore((s) => s.notify);
  const [list, setList] = useState<RecordingMeta[] | null>(null);
  const [playing, setPlaying] = useState<{ meta: RecordingMeta; cast: Cast } | null>(null);
  const [verify, setVerify] = useState<Record<string, VerifyReport>>({});
  const [chain, setChain] = useState<ChainReport | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const reload = () =>
    invoke<RecordingMeta[]>('recordings_list')
      .then(setList)
      .catch((e: unknown) => {
        setList([]);
        notify(String(e), 'error');
      });
  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const play = async (meta: RecordingMeta) => {
    try {
      const text = await invoke<string>('recording_load', { id: meta.id });
      setPlaying({ meta, cast: parseCast(text) });
    } catch (e) {
      notify(t('chrome.recLoadFailed', { error: String(e) }), 'error');
    }
  };

  const exportText = async (meta: RecordingMeta) => {
    const dest = await save({
      defaultPath: `${meta.title.replace(/[\\/:*?"<>|]/g, '_')}.txt`,
      filters: [{ name: 'Text', extensions: ['txt'] }],
    });
    if (!dest) return;
    try {
      await invoke('recording_export_text', { id: meta.id, dest });
      notify(t('chrome.recExported', { path: dest }), 'success');
    } catch (e) {
      notify(String(e), 'error');
    }
  };

  const runVerify = async (meta: RecordingMeta) => {
    try {
      const r = await invoke<VerifyReport>('recording_verify', { id: meta.id });
      setVerify((v) => ({ ...v, [meta.id]: r }));
    } catch (e) {
      notify(String(e), 'error');
    }
  };

  const remove = async (meta: RecordingMeta) => {
    setConfirmDelete(null);
    try {
      await invoke('recording_delete', { id: meta.id });
      await reload();
    } catch (e) {
      notify(String(e), 'error');
    }
  };

  const verifyChain = async () => {
    try {
      setChain(await invoke<ChainReport>('audit_verify_chain'));
    } catch (e) {
      notify(String(e), 'error');
    }
  };

  return (
    <Dialog
      title={t('chrome.recTitle')}
      onClose={playing ? () => setPlaying(null) : toggle}
      panelClass="flex max-h-[85vh] w-[56rem] flex-col rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-200 shadow-xl"
    >
      {/* 初始焦点落在标题：从命令面板回车打开时，回车不会顺带触发某个按钮 */}
      <h2
        data-autofocus
        tabIndex={-1}
        className="mb-1 text-sm font-semibold text-neutral-100 outline-none"
      >
        {playing ? playing.meta.title : t('chrome.recTitle')}
      </h2>
      {playing ? (
        <CastPlayer cast={playing.cast} onBack={() => setPlaying(null)} />
      ) : (
        <>
          <p className="mb-3 text-neutral-500">{t('chrome.recHint')}</p>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {list === null ? null : list.length === 0 ? (
              <p className="text-neutral-500">{t('chrome.recEmpty')}</p>
            ) : (
              <ul>
                {list.map((m) => {
                  const v = verify[m.id];
                  const live = m.endedAt === null;
                  return (
                    <li key={m.id} className="border-t border-neutral-800 py-1.5 first:border-t-0">
                      <div className="flex items-center gap-2">
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-2">
                            <span className="truncate text-neutral-100">{m.title}</span>
                            {m.source === 'agent' && (
                              <span className="shrink-0 rounded bg-purple-600/70 px-1 text-[10px] leading-4 text-white">
                                AI
                              </span>
                            )}
                            {live && (
                              <span className="shrink-0 text-red-400">{t('chrome.recLive')}</span>
                            )}
                            {m.truncated && (
                              <span className="shrink-0 text-amber-300">
                                {t('chrome.recTruncated')}
                              </span>
                            )}
                          </div>
                          <div className="text-neutral-500">
                            {new Date(m.startedAt).toLocaleString()} · {fmtDuration(m.durationMs)} ·{' '}
                            {fmtSize(m.bytes)}
                          </div>
                        </div>
                        <button
                          className="shrink-0 rounded bg-neutral-700 px-2 py-0.5 hover:bg-neutral-600"
                          onClick={() => void play(m)}
                        >
                          {t('chrome.recPlay')}
                        </button>
                        <button
                          className="shrink-0 rounded px-2 py-0.5 text-neutral-300 hover:bg-neutral-800"
                          onClick={() => void exportText(m)}
                        >
                          {t('chrome.recExport')}
                        </button>
                        <button
                          className="shrink-0 rounded px-2 py-0.5 text-neutral-300 hover:bg-neutral-800 disabled:opacity-50"
                          disabled={live}
                          onClick={() => void runVerify(m)}
                        >
                          {t('chrome.recVerify')}
                        </button>
                        <button
                          className="shrink-0 rounded px-2 py-0.5 text-red-300 hover:bg-neutral-800 disabled:opacity-50"
                          disabled={live}
                          onClick={() =>
                            confirmDelete === m.id ? void remove(m) : setConfirmDelete(m.id)
                          }
                        >
                          {confirmDelete === m.id
                            ? t('chrome.recConfirmDelete')
                            : t('chrome.snippetDelete')}
                        </button>
                      </div>
                      {v && (
                        <div className={v.ok ? 'mt-0.5 text-green-400' : 'mt-0.5 text-red-300'}>
                          {v.ok ? '✓ ' : '✗ '}
                          {v.reason}
                          <span className="ml-2 font-mono text-neutral-500">
                            sha256 {v.fileSha256.slice(0, 16)}…
                          </span>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
          <div className="mt-3 flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <button
                className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
                onClick={() => void verifyChain()}
              >
                {t('chrome.recVerifyChain')}
              </button>
              {chain && (
                <span className={chain.ok ? 'text-green-400' : 'text-red-300'}>
                  {chain.ok
                    ? t('chrome.recChainOk', { n: chain.checked })
                    : t('chrome.recChainBad', { id: chain.firstBadId ?? '?' })}
                </span>
              )}
            </div>
            <button
              className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
              onClick={toggle}
            >
              {t('dialogs.close')}
            </button>
          </div>
        </>
      )}
    </Dialog>
  );
}

/** 录像播放器：只读 xterm + 定时按帧写入；进度条可拖动（重放到目标时刻） */
function CastPlayer({ cast, onBack }: { cast: Cast; onBack: () => void }) {
  const t = useT();
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const frames = useMemo<Frame[]>(() => timeline(cast.events, IDLE_CAP), [cast]);
  const total = frames.length > 0 ? frames[frames.length - 1].at : 0;
  const [pos, setPos] = useState(0);
  const [paused, setPaused] = useState(false);
  const [speed, setSpeed] = useState(1);
  /** 已写入终端的帧数 */
  const written = useRef(0);
  /** 每次拖动进度 +1：重启计时器，并让旧计时器的残余 tick 作废 */
  const [epoch, setEpoch] = useState(0);
  const epochRef = useRef(0);

  useEffect(() => {
    const settings = useAppStore.getState().settings;
    const opts = readTerminalSettings(settings);
    const term = new Terminal({
      cols: cast.header.width,
      rows: cast.header.height,
      fontFamily: opts.fontFamily,
      fontSize: opts.fontSize,
      scrollback: 5000,
      disableStdin: true,
      theme: resolveTheme(
        typeof settings['theme'] === 'string' ? settings['theme'] : DEFAULT_THEME_ID,
        typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : undefined,
      ).xterm,
    });
    if (hostRef.current) term.open(hostRef.current);
    termRef.current = term;
    return () => {
      term.dispose();
      termRef.current = null;
    };
  }, [cast]);

  /** 把 [written, upto) 的帧写进终端 */
  const writeUpTo = (upto: number) => {
    const term = termRef.current;
    if (!term) return;
    let out = '';
    for (let i = written.current; i < upto; i++) {
      const f = frames[i];
      if (f.kind === 'o') {
        out += f.data;
      } else {
        if (out) term.write(out);
        out = '';
        const size = parseSize(f.data);
        if (size) term.resize(size.cols, size.rows);
      }
    }
    if (out) term.write(out);
    written.current = upto;
  };

  useEffect(() => {
    if (paused) return;
    const wallStart = performance.now();
    const posStart = pos;
    const mine = epochRef.current;
    const timer = window.setInterval(() => {
      if (epochRef.current !== mine) return;
      const now = Math.min(posStart + ((performance.now() - wallStart) / 1000) * speed, total);
      writeUpTo(framesUntil(frames, now));
      setPos(now);
      if (now >= total) setPaused(true);
    }, 33);
    return () => window.clearInterval(timer);
    // pos 只在开始/恢复/改速时取一次起点
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paused, speed, epoch]);

  const seek = (target: number) => {
    const term = termRef.current;
    if (!term) return;
    term.reset();
    term.resize(cast.header.width, cast.header.height);
    written.current = 0;
    writeUpTo(framesUntil(frames, target));
    setPos(target);
    epochRef.current += 1;
    setEpoch(epochRef.current);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="min-h-0 flex-1 overflow-auto rounded border border-neutral-800 bg-black p-1">
        <div ref={hostRef} />
      </div>
      <div className="flex items-center gap-2">
        <button
          className="w-16 rounded bg-neutral-700 px-2 py-0.5 hover:bg-neutral-600"
          onClick={() => {
            if (paused && pos >= total) seek(0);
            setPaused(!paused);
          }}
        >
          {paused ? t('chrome.recResume') : t('chrome.recPause')}
        </button>
        <input
          type="range"
          className="flex-1"
          min={0}
          max={total || 1}
          step={0.1}
          value={pos}
          onChange={(e) => seek(Number(e.target.value))}
        />
        <span className="w-24 text-right font-mono text-neutral-400">
          {fmtDuration(pos * 1000)} / {fmtDuration(total * 1000)}
        </span>
        <select
          className="rounded border border-neutral-700 bg-neutral-950 px-1 py-0.5"
          value={speed}
          onChange={(e) => setSpeed(Number(e.target.value))}
        >
          {SPEEDS.map((s) => (
            <option key={s} value={s}>
              {s}×
            </option>
          ))}
        </select>
        <button
          className="rounded px-3 py-0.5 text-neutral-400 hover:bg-neutral-800"
          onClick={onBack}
        >
          {t('chrome.recBack')}
        </button>
      </div>
    </div>
  );
}
