//! 命令策略名单（settings policy.whitelist / policy.blacklist / policy.indirect）的前端读写口径。
//! 与后端 mcp_policy::parse_list_setting 一致：值为字符串数组，或内容为数组的 JSON 字符串；
//! 缺省/坏值 = 未自定义（用内置名单），显式空数组 = 真空名单。

export const POLICY_LIST_KEYS = {
  whitelist: 'policy.whitelist',
  blacklist: 'policy.blacklist',
  indirect: 'policy.indirect',
} as const;

export type PolicyListKind = keyof typeof POLICY_LIST_KEYS;

export type PolicyDefaults = Record<PolicyListKind, string[]>;

/** 规范化：去首尾空白、丢空项、按首次出现去重 */
export function normalizeList(items: readonly string[]): string[] {
  const out: string[] = [];
  for (const raw of items) {
    const v = raw.trim();
    if (v && !out.includes(v)) out.push(v);
  }
  return out;
}

/** 读设置值；null = 未自定义（缺省或无法解析） */
export function readPolicyList(raw: unknown): string[] | null {
  let v = raw;
  if (typeof v === 'string') {
    try {
      v = JSON.parse(v) as unknown;
    } catch {
      return null;
    }
  }
  if (!Array.isArray(v) || !v.every((x) => typeof x === 'string')) return null;
  return normalizeList(v as string[]);
}

/** 输入框文本拆成命令名（空白/逗号/顿号分隔），并入现有名单 */
export function addCommands(list: readonly string[], input: string): string[] {
  return normalizeList([...list, ...input.split(/[\s,，、]+/)]);
}

/** 与内置名单内容相同（顺序无关）即视为默认 */
export function sameAsDefault(list: readonly string[], defaults: readonly string[]): boolean {
  return list.length === defaults.length && list.every((x) => defaults.includes(x));
}
