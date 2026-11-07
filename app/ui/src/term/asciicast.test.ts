import { describe, expect, it } from 'vitest';
import { framesUntil, parseCast, parseSize, timeline } from './asciicast';

const header = JSON.stringify({ version: 2, width: 100, height: 30, title: 'web' });

describe('asciicast', () => {
  it('parses header and events, skipping a truncated last line', () => {
    const text = [header, '[0.5,"o","hi"]', '[1.0,"i","ls\\r"]', '[1.2,"o","x'].join('\n');
    const c = parseCast(text);
    expect(c.header).toMatchObject({ width: 100, height: 30, title: 'web' });
    expect(c.events).toEqual([
      [0.5, 'o', 'hi'],
      [1.0, 'i', 'ls\r'],
    ]);
  });

  it('rejects non-v2 files', () => {
    expect(() => parseCast('{"version":1,"width":1,"height":1}')).toThrow();
    expect(() => parseCast('')).toThrow();
  });

  it('timeline drops input events and caps idle gaps', () => {
    const frames = timeline(
      [
        [0.5, 'o', 'a'],
        [0.7, 'i', 'typed'],
        [60, 'o', 'b'],
        [60.5, 'r', '80x24'],
      ],
      2,
    );
    expect(frames.map((f) => [f.at, f.kind, f.data])).toEqual([
      [0.5, 'o', 'a'],
      [2.5, 'o', 'b'],
      [3, 'r', '80x24'],
    ]);
  });

  it('framesUntil counts frames at or before t', () => {
    const frames = timeline(
      [
        [1, 'o', 'a'],
        [2, 'o', 'b'],
        [3, 'o', 'c'],
      ],
      10,
    );
    expect(framesUntil(frames, 0.5)).toBe(0);
    expect(framesUntil(frames, 2)).toBe(2);
    expect(framesUntil(frames, 99)).toBe(3);
  });

  it('parseSize', () => {
    expect(parseSize('120x40')).toEqual({ cols: 120, rows: 40 });
    expect(parseSize('0x5')).toBeNull();
    expect(parseSize('junk')).toBeNull();
  });
});
