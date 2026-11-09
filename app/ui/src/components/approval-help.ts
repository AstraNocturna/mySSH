import type { MsgKey } from '../i18n';
import { zhCommands } from '../i18n/messages/commands.zh-CN';
import type { ApprovalPart } from '../state/app-store';

type T = (key: MsgKey, vars?: Record<string, string | number>) => string;

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

/** 重定向到文件时本身只是「写内容」的命令：直接说写入哪个文件 */
const PLAIN_WRITERS = new Set(['cat', 'echo', 'printf']);
/** 总说明最多列出的步骤数 */
const MAX_STEPS = 6;
/** 每类原因最多列出的名字数 */
const MAX_NAMES = 4;

function stepText(p: ApprovalPart, t: T): string | null {
  const help = commandHelpKey(p.head, p.sub);
  const file = p.writes;
  let desc: string | null;
  if (file && p.heredocLines > 0) {
    desc = t('dialogs.approvalHeredocWrite', { n: p.heredocLines, file });
  } else if (file && (!help || (p.head && PLAIN_WRITERS.has(p.head)))) {
    desc = t(p.append ? 'dialogs.approvalAppendFile' : 'dialogs.approvalWriteFile', { file });
  } else if (file && help) {
    desc = t(p.append ? 'dialogs.approvalDescAppend' : 'dialogs.approvalDescWrite', {
      desc: t(help),
      file,
    });
  } else {
    desc = help ? t(help) : null;
  }
  if (!desc) return null;
  if (p.wrapper === 'sudo') desc = t('dialogs.approvalAsRoot', { desc });
  if (p.op === '&') desc += t('dialogs.approvalBg');
  return desc;
}

const lowerFirst = (s: string) => s.charAt(0).toLowerCase() + s.slice(1);

function names(list: string[], t: T): string {
  const uniq = [...new Set(list)];
  const shown = uniq.slice(0, MAX_NAMES).join(t('dialogs.approvalListSep'));
  return uniq.length > MAX_NAMES ? `${shown}…` : shown;
}

export interface ApprovalSummary {
  /** 一句话总说明；没有可识别的步骤时为空串 */
  text: string;
  /** 没有内置说明的命令名 */
  unknown: string;
  /** 需要确认的原因 */
  reasons: string;
}

/** 把逐段分析合成一段总说明：步骤按顺序连成一句，没有说明的命令只列名字，原因按类别归并 */
export function summarizeParts(parts: ApprovalPart[], t: T): ApprovalSummary {
  const steps: string[] = [];
  const unknown: string[] = [];
  let joiner: MsgKey = 'dialogs.approvalThen';
  let last: string | null = null;
  for (const p of parts) {
    const desc = stepText(p, t);
    if (desc === null) {
      unknown.push(p.head ?? p.text);
    } else if (desc !== last) {
      steps.push(steps.length === 0 ? desc : t(joiner) + lowerFirst(desc));
      last = desc;
    }
    joiner = p.op === '||' ? 'dialogs.approvalOrElse' : 'dialogs.approvalThen';
  }
  let text = steps.slice(0, MAX_STEPS).join('');
  if (steps.length > MAX_STEPS) text += t('dialogs.approvalMore', { n: steps.length });

  const pick = (risk: ApprovalPart['risk'], f: (p: ApprovalPart) => string | null) =>
    parts.filter((p) => p.risk === risk).flatMap((p) => f(p) ?? []);
  const reasons: string[] = [];
  const add = (key: MsgKey, list: string[]) => {
    if (list.length > 0) reasons.push(t(key, { list: names(list, t) }));
  };
  add(
    'dialogs.approvalReasonBlacklist',
    pick('blacklist', (p) => p.head),
  );
  add(
    'dialogs.approvalReasonIndirect',
    pick('indirect', (p) => p.wrapper ?? p.head),
  );
  if (parts.some((p) => p.risk === 'unparsed')) reasons.push(t('dialogs.approvalReasonUnparsed'));
  add(
    'dialogs.approvalReasonWrite',
    parts.filter((p) => p.risk !== 'readonly').flatMap((p) => p.writes ?? []),
  );
  add(
    'dialogs.approvalReasonChange',
    pick('write', (p) =>
      p.writes || !p.head
        ? null
        : commandHelpKey(p.head, p.sub) === `cmd.${p.head} ${p.sub}`
          ? `${p.head} ${p.sub}`
          : p.head,
    ),
  );
  return {
    text,
    unknown: unknown.length > 0 ? t('dialogs.approvalUnknown', { list: names(unknown, t) }) : '',
    reasons: reasons.join(t('dialogs.approvalReasonSep')),
  };
}
