/** asciicast v2 解析与播放时间轴（会话录像播放器用；文件由后端 recording.rs 写出） */

export interface CastHeader {
  width: number;
  height: number;
  title?: string;
  timestamp?: number;
}

/** [秒, 类型, 数据]：o=输出 i=输入 r=尺寸("COLSxROWS") m=标记 */
export type CastEvent = [number, string, string];

export interface Cast {
  header: CastHeader;
  events: CastEvent[];
}

/** 播放帧：只含会影响屏幕的输出与尺寸变化 */
export interface Frame {
  at: number;
  kind: 'o' | 'r';
  data: string;
}

export function parseCast(text: string): Cast {
  const lines = text.split('\n').filter((l) => l.trim() !== '');
  if (lines.length === 0) throw new Error('empty recording');
  const h = JSON.parse(lines[0]) as Partial<CastHeader> & { version?: number };
  if (h.version !== 2 || typeof h.width !== 'number' || typeof h.height !== 'number') {
    throw new Error('not an asciicast v2 file');
  }
  const events: CastEvent[] = [];
  for (const line of lines.slice(1)) {
    let ev: unknown;
    try {
      ev = JSON.parse(line);
    } catch {
      // 异常退出时最后一行可能写了一半：跳过
      continue;
    }
    if (
      Array.isArray(ev) &&
      typeof ev[0] === 'number' &&
      typeof ev[1] === 'string' &&
      typeof ev[2] === 'string'
    ) {
      events.push([ev[0], ev[1], ev[2]]);
    }
  }
  return {
    header: { width: h.width, height: h.height, title: h.title, timestamp: h.timestamp },
    events,
  };
}

/** 播放时间轴：丢弃非屏幕事件，任意两帧间的空闲压到 idleCap 秒以内 */
export function timeline(events: CastEvent[], idleCap: number): Frame[] {
  const frames: Frame[] = [];
  let last = 0;
  let at = 0;
  for (const [t, kind, data] of events) {
    if (kind !== 'o' && kind !== 'r') continue;
    at += Math.min(Math.max(t - last, 0), idleCap);
    last = t;
    frames.push({ at, kind, data });
  }
  return frames;
}

/** 时刻 t（含）之前的帧数：二分 */
export function framesUntil(frames: Frame[], t: number): number {
  let lo = 0;
  let hi = frames.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (frames[mid].at <= t) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** "COLSxROWS" → 尺寸；非法返回 null */
export function parseSize(data: string): { cols: number; rows: number } | null {
  const m = /^(\d+)x(\d+)$/.exec(data);
  if (!m) return null;
  const cols = Number(m[1]);
  const rows = Number(m[2]);
  return cols > 0 && rows > 0 ? { cols, rows } : null;
}
