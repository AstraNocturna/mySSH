import type { MsgKey } from '../i18n';
import { zhCommands } from '../i18n/messages/commands.zh-CN';
import type { ApprovalCmd, ApprovalPart } from '../state/app-store';

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
/** 总说明最多列出的步骤数；段数更多或含循环/分支时按整段脚本说明 */
const MAX_STEPS = 6;
/** 每类原因最多列出的名字数 */
const MAX_NAMES = 4;
const LOOP_KEYWORDS = new Set(['for', 'while', 'until', 'select']);
const BRANCH_KEYWORDS = new Set(['if', 'case']);

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

/** 段里实际执行的命令；旧版后端没有 cmds 时退回主命令 */
function cmdsOf(p: ApprovalPart): ApprovalCmd[] {
  if (p.cmds) return p.cmds;
  return p.head ? [{ head: p.head, sub: p.sub, risk: p.risk }] : [];
}

/** 改关键环境变量的纯赋值，后端记为 `PATH=` */
const envName = (c: ApprovalCmd) => (c.head.endsWith('=') ? c.head.slice(0, -1) : null);

export interface ApprovalSummary {
  /** 一句话总说明；没有可识别的步骤时为空串 */
  text: string;
  /** 没有内置说明的命令名 */
  unknown: string;
  /** 需要确认的原因 */
  reasons: string;
}

/** 按顺序把各段连成一句（简单组合命令） */
function stepsSummary(parts: ApprovalPart[], t: T): { text: string; unknown: string[] } {
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
  return { text, unknown };
}

/** 去掉简介里的括号补充，放进「用到 awk（…）」时不套两层括号 */
const brief = (s: string) => s.replace(/\s*[（(][^）)]*[）)]/g, '');

/** 脚本整体说明：几行、含循环/分支、用到哪些命令、写不写文件 */
function scriptSummary(parts: ApprovalPart[], t: T, command: string): string {
  const lines = command.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith('#')).length;
  let text = lines > 1 ? t('dialogs.approvalScript', { n: lines }) : t('dialogs.approvalCombo');
  const kws = parts.flatMap((p) => p.keyword ?? []);
  const has: string[] = [];
  if (kws.some((k) => LOOP_KEYWORDS.has(k))) has.push(t('dialogs.approvalHasLoop'));
  if (kws.some((k) => BRANCH_KEYWORDS.has(k))) has.push(t('dialogs.approvalHasBranch'));
  if (has.length > 0)
    text += t('dialogs.approvalHas', { list: has.join(t('dialogs.approvalListSep')) });

  const seen = new Map<string, string>();
  for (const c of parts.flatMap(cmdsOf)) {
    if (envName(c) || seen.has(c.head)) continue;
    const help = commandHelpKey(c.head, c.sub);
    seen.set(
      c.head,
      help ? t('dialogs.approvalUseItem', { cmd: c.head, desc: brief(t(help)) }) : c.head,
    );
  }
  const uses = [...seen.values()];
  if (uses.length > 0) {
    text += t('dialogs.approvalUses', {
      list: uses.slice(0, MAX_STEPS).join(t('dialogs.approvalListSep')),
    });
    if (uses.length > MAX_STEPS) text += t('dialogs.approvalUsesMore', { n: uses.length });
  }
  const files = parts.flatMap((p) => p.writes ?? []);
  if (files.length > 0) text += t('dialogs.approvalWritesTo', { list: names(files, t) });
  else if (!parts.flatMap(cmdsOf).some((c) => c.risk === 'write' || c.risk === 'blacklist'))
    text += t('dialogs.approvalNoWrite');
  return text;
}

/**
 * 把逐段分析合成总说明。简单组合命令按步骤连成一句；含循环/分支或段数多的脚本只给整体说明。
 * 原因按段里实际执行的命令归类（不会把 do、for 等关键字当成命令）。
 */
export function summarizeParts(parts: ApprovalPart[], t: T, command = ''): ApprovalSummary {
  const script = parts.length > MAX_STEPS || parts.some((p) => p.keyword);
  let text: string;
  let unknown: string[] = [];
  if (script) {
    text = scriptSummary(parts, t, command);
  } else {
    ({ text, unknown } = stepsSummary(parts, t));
  }

  const blacklist: string[] = [];
  const indirect: string[] = [];
  const env: string[] = [];
  const change: string[] = [];
  for (const p of parts) {
    for (const c of cmdsOf(p)) {
      const main = c.head === p.head;
      if (c.risk === 'blacklist') blacklist.push(c.head);
      else if (c.risk === 'indirect') {
        const v = envName(c);
        if (v) env.push(v);
        else indirect.push(main && p.wrapper ? p.wrapper : c.head);
      } else if (c.risk === 'write' && !(main && p.writes)) {
        change.push(
          commandHelpKey(c.head, c.sub) === `cmd.${c.head} ${c.sub}`
            ? `${c.head} ${c.sub}`
            : c.head,
        );
      }
    }
  }
  const reasons: string[] = [];
  const add = (key: MsgKey, list: string[]) => {
    if (list.length > 0) reasons.push(t(key, { list: names(list, t) }));
  };
  add('dialogs.approvalReasonBlacklist', blacklist);
  add('dialogs.approvalReasonIndirect', indirect);
  add('dialogs.approvalReasonEnv', env);
  if (parts.some((p) => p.risk === 'unparsed')) reasons.push(t('dialogs.approvalReasonUnparsed'));
  add(
    'dialogs.approvalReasonWrite',
    parts.filter((p) => p.risk !== 'readonly').flatMap((p) => p.writes ?? []),
  );
  add('dialogs.approvalReasonChange', change);
  return {
    text,
    unknown: unknown.length > 0 ? t('dialogs.approvalUnknown', { list: names(unknown, t) }) : '',
    reasons: reasons.join(t('dialogs.approvalReasonSep')),
  };
}
