import type { MsgKey } from '../i18n';
import { zhCommands } from '../i18n/messages/commands.zh-CN';
import type { ApprovalPart } from '../state/app-store';

const isCmdKey = (k: string): k is keyof typeof zhCommands =>
  Object.prototype.hasOwnProperty.call(zhCommands, k);

/** 一段命令的内置简介键：先找「命令 子命令」（systemctl restart），再找命令名；没有返回 null */
export function commandHelpKey(head: string | null, sub: string | null): MsgKey | null {
  if (!head) return null;
  const withSub = `cmd.${head} ${sub ?? ''}`;
  if (sub && isCmdKey(withSub)) return withSub;
  const plain = `cmd.${head}`;
  return isCmdKey(plain) ? plain : null;
}

export const RISK_KEY: Record<ApprovalPart['risk'], MsgKey> = {
  readonly: 'dialogs.approvalRiskReadonly',
  write: 'dialogs.approvalRiskWrite',
  unparsed: 'dialogs.approvalRiskUnparsed',
  indirect: 'dialogs.approvalRiskIndirect',
  blacklist: 'dialogs.approvalRiskBlacklist',
};

export const RISK_CLASS: Record<ApprovalPart['risk'], string> = {
  readonly: 'bg-emerald-900/60 text-emerald-200',
  write: 'bg-amber-900/60 text-amber-200',
  unparsed: 'bg-orange-900/60 text-orange-200',
  indirect: 'bg-red-900/60 text-red-200',
  blacklist: 'bg-red-700 text-white',
};

export function opKey(op: ApprovalPart['op']): MsgKey | null {
  switch (op) {
    case '&&':
      return 'dialogs.approvalOpAnd';
    case '||':
      return 'dialogs.approvalOpOr';
    case '|':
      return 'dialogs.approvalOpPipe';
    case ';':
      return 'dialogs.approvalOpSeq';
    case '&':
      return 'dialogs.approvalOpBg';
    default:
      return null;
  }
}
