import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow, UserAttentionType } from '@tauri-apps/api/window';
import { create } from 'zustand';
import type { McpApprovalRequest } from './app-store';

/** 后端 mcp_activity 快照（crates/app/src/mcp_ui.rs） */
export interface AiExec {
  execId: string;
  sessionId: string;
  tool: string;
  command: string;
  startedAt: number;
}
export interface AiJob {
  jobId: string;
  sessionId: string;
  command: string;
  startedAt: number;
  status: string;
}
export interface AiTerminal {
  terminalId: string;
  /** 用户共享的临时连接标签没有会话档案 */
  sessionId: string | null;
  kind: 'headless' | 'agent' | 'shared';
  tabId: string | null;
}
export interface AiActivitySnapshot {
  execs: AiExec[];
  jobs: AiJob[];
  terminals: AiTerminal[];
}
export type AiStopKind = 'exec' | 'job' | 'terminal';

/** 有活动时的兜底刷新间隔：后台命令自然结束、终端远端退出不推提示帧 */
const POLL_MS = 3000;

interface AiActivityStore extends AiActivitySnapshot {
  panelOpen: boolean;
  togglePanel(): void;
  /** 授权框当前要看的审批项（活动面板点选时设置） */
  approvalFocus: string | null;
  /** 选中并展开授权框 */
  focusApproval(id: string | null): void;
  /** 授权框收起（跳去看会话终端时）；新请求到达或点选审批项时重新展开 */
  approvalHidden: boolean;
  setApprovalHidden(hidden: boolean): void;
  refresh(): void;
  stop(kind: AiStopKind, id: string): Promise<void>;
}

function signature(s: AiActivitySnapshot): string {
  return [
    s.execs.map((e) => e.execId).join(','),
    s.jobs.map((j) => `${j.jobId}:${j.status}`).join(','),
    s.terminals.map((t) => `${t.terminalId}:${t.tabId ?? ''}`).join(','),
  ].join('|');
}

let inFlight = false;
let again = false;
let pollTimer: number | null = null;

export const useAiActivity = create<AiActivityStore>((set, get) => ({
  execs: [],
  jobs: [],
  terminals: [],
  panelOpen: false,
  togglePanel: () => set((s) => ({ panelOpen: !s.panelOpen })),
  approvalFocus: null,
  focusApproval: (id) => set({ approvalFocus: id, approvalHidden: false }),
  approvalHidden: false,
  setApprovalHidden: (hidden) => set({ approvalHidden: hidden }),

  refresh: () => {
    // 提示帧可能成串到达：在途时只记一笔，回来后再取一次
    if (inFlight) {
      again = true;
      return;
    }
    inFlight = true;
    Promise.resolve()
      .then(() => invoke<AiActivitySnapshot>('mcp_activity'))
      .then((snap) => {
        // 轮询多数时候没变化：不换引用，免得侧边栏等订阅方每 3 秒重渲染
        if (signature(snap) !== signature(get())) {
          set({ execs: snap.execs, jobs: snap.jobs, terminals: snap.terminals });
        }
      })
      .catch(() => undefined)
      .finally(() => {
        inFlight = false;
        if (again) {
          again = false;
          get().refresh();
          return;
        }
        if (pollTimer !== null) window.clearTimeout(pollTimer);
        pollTimer = null;
        const { execs, jobs, terminals } = get();
        if (execs.length + jobs.length + terminals.length > 0) {
          pollTimer = window.setTimeout(() => {
            pollTimer = null;
            get().refresh();
          }, POLL_MS);
        }
      });
  },

  stop: async (kind, id) => {
    try {
      await invoke('mcp_activity_stop', { kind, id });
    } finally {
      get().refresh();
    }
  },
}));

/** 窗口不在前台时闪任务栏（Windows 上持续闪到窗口被激活） */
export function flashTaskbar(): void {
  try {
    if (document.hasFocus()) return;
    void getCurrentWindow()
      .requestUserAttention(UserAttentionType.Informational)
      .catch(() => undefined);
  } catch {
    // 非 Tauri 环境（单测）没有 document / 窗口句柄
  }
}

export interface SessionAiStatus {
  /** 进行中的命令 + 运行中的后台命令（空闲的 AI 终端不算，标签上已有 AI 标记） */
  running: number;
  pending: number;
}

/** 各会话的 AI 状态（标签栏、侧边栏徽标用）；没有活动的会话不在表里 */
export function aiStatusBySession(
  activity: AiActivitySnapshot,
  approvals: McpApprovalRequest[],
): Map<string, SessionAiStatus> {
  const map = new Map<string, SessionAiStatus>();
  const at = (id: string) => {
    let s = map.get(id);
    if (!s) map.set(id, (s = { running: 0, pending: 0 }));
    return s;
  };
  activity.execs.forEach((e) => at(e.sessionId).running++);
  activity.jobs.forEach((j) => at(j.sessionId).running++);
  approvals.forEach((a) => at(a.sessionId).pending++);
  return map;
}

/** 待确认项按会话分组：组按首条到达先后排，组内保持到达顺序 */
export function groupApprovals(
  approvals: McpApprovalRequest[],
): { sessionId: string; items: McpApprovalRequest[] }[] {
  const groups: { sessionId: string; items: McpApprovalRequest[] }[] = [];
  for (const a of approvals) {
    const g = groups.find((x) => x.sessionId === a.sessionId);
    if (g) g.items.push(a);
    else groups.push({ sessionId: a.sessionId, items: [a] });
  }
  return groups;
}

/** 活动面板里出现的全部会话 id（待确认在前，其余按活动先后），去重 */
export function activeSessionIds(
  activity: AiActivitySnapshot,
  approvals: McpApprovalRequest[],
): string[] {
  const ids: string[] = [];
  const add = (id: string | null) => {
    if (id && !ids.includes(id)) ids.push(id);
  };
  approvals.forEach((a) => add(a.sessionId));
  activity.execs.forEach((e) => add(e.sessionId));
  activity.jobs.forEach((j) => add(j.sessionId));
  activity.terminals.forEach((t) => add(t.sessionId));
  return ids;
}

/** 已运行时长：秒 / 分秒 / 时分 */
export function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`;
}
