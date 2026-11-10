import { describe, expect, it } from 'vitest';
import { addCommands, normalizeList, readPolicyList, sameAsDefault } from './policy-lists';

describe('readPolicyList', () => {
  it('缺省与坏值视为未自定义，回落内置名单', () => {
    expect(readPolicyList(undefined)).toBeNull();
    expect(readPolicyList('not json')).toBeNull();
    expect(readPolicyList({ a: 1 })).toBeNull();
    expect(readPolicyList([1, 2])).toBeNull();
    expect(readPolicyList('"rm"')).toBeNull();
  });

  it('数组与「内容为数组的 JSON 字符串」都认，并规范化', () => {
    expect(readPolicyList([' rm ', 'kill', 'rm', ''])).toEqual(['rm', 'kill']);
    expect(readPolicyList('["rm","dd"]')).toEqual(['rm', 'dd']);
  });

  it('显式空数组是真空名单，不等同于未自定义', () => {
    expect(readPolicyList([])).toEqual([]);
    expect(readPolicyList('[]')).toEqual([]);
  });
});

describe('addCommands', () => {
  it('按空白/逗号/顿号拆分，去重并保持顺序', () => {
    expect(addCommands(['ls'], 'cat, grep  ls、awk，sed')).toEqual([
      'ls',
      'cat',
      'grep',
      'awk',
      'sed',
    ]);
  });

  it('空输入不改变名单', () => {
    expect(addCommands(['ls'], '   ')).toEqual(['ls']);
  });
});

describe('sameAsDefault / normalizeList', () => {
  it('顺序无关比较', () => {
    expect(sameAsDefault(['b', 'a'], ['a', 'b'])).toBe(true);
    expect(sameAsDefault(['a'], ['a', 'b'])).toBe(false);
  });
  it('normalizeList 去空白去重', () => {
    expect(normalizeList([' x', 'x ', 'y'])).toEqual(['x', 'y']);
  });
});
