import type { MsgKey } from '../i18n';

/** MCP 权限分组（与后端 tool_group_key 键名一致；标签复用设置页文案） */
export const MCP_PERM_GROUPS = [
  ['list_sessions', 'dialogs.mcpAllowListSessions'],
  ['ssh_exec', 'dialogs.mcpAllowSshExec'],
  ['sftp_read', 'dialogs.mcpAllowSftpRead'],
  ['sftp_write', 'dialogs.mcpAllowSftpWrite'],
  ['sftp_transfer', 'dialogs.mcpAllowSftpTransfer'],
  ['terminal', 'dialogs.mcpAllowTerminal'],
] as const satisfies readonly (readonly [string, MsgKey])[];

/** 访问档位（与后端 mcp_policy::parse_level 取值一致） */
export const MCP_LEVELS = [
  ['readonly', 'dialogs.mcpLevelReadonly'],
  ['standard', 'dialogs.mcpLevelStandard'],
  ['full', 'dialogs.mcpLevelFull'],
] as const satisfies readonly (readonly [string, MsgKey])[];

/** 稀疏权限映射的单键改写：'inherit' = 删键（禁止动态 delete，lint） */
export function setPermMode(
  prev: Record<string, boolean | undefined>,
  group: string,
  mode: string,
): Record<string, boolean> {
  const entries = Object.entries(prev).filter(
    (e): e is [string, boolean] => e[0] !== group && typeof e[1] === 'boolean',
  );
  if (mode !== 'inherit') entries.push([group, mode === 'allow']);
  return Object.fromEntries(entries);
}
