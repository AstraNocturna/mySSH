import { Channel, invoke } from '@tauri-apps/api/core';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { create } from 'zustand';
import { TerminalSession } from '../term/terminal-session';
import { GROUP_KEYS, readFailedList, readStringList, type FailedEntry } from './groups';
import {
  appendLeaf,
  firstLeaf,
  leaf,
  paneIds,
  removeLeaf,
  setRatio,
  splitLeaf,
  type LayoutNode,
} from '../term/layout';
import type {
  ConnectTarget,
  HostKeyPromptFrame,
  KiChallengeFrame,
  SessionRecord,
  SessionStateFrame,
  TermEvent,
  TermOpenSpec,
  TunnelDef,
  TunnelInfo,
  SessionTunnelResult,
} from '../term/types';
import { tunnelDisplayName, tunnelFeedback } from './tunnel-utils';
import { reconnectRegistry } from '../term/registry';
import { broadcastInput } from '../term/broadcast';
import { useTransferStore } from './transfer-store';
import { tNow } from '../i18n';
import {
  feedOf,
  formatEnd,
  formatOutput,
  formatStart,
  mirrorFeeds,
  type ExecMirrorFrame,
} from '../term/exec-mirror';

export type PaneState = 'connecting' | 'connected' | 'reconnecting' | 'closed' | 'error';

/** 复合命令的一段（后端 core-policy explain）：原文、连接符、主命令、写入的文件、风险 */
export interface ApprovalPart {
  text: string;
  op: '|' | '||' | '&&' | ';' | '&' | null;
  head: string | null;
  sub: string | null;
  /** sudo / nohup 等前缀命令 */
  wrapper: string | null;
  /** 输出重定向写入的文件 */
  writes: string | null;
  append: boolean;
  /** heredoc 正文行数 */
  heredocLines: number;
  risk: 'readonly' | 'write' | 'unparsed' | 'indirect' | 'blacklist';
}

/** MCP 危险命令审批请求（后端 mcp_ui.rs approval_request 帧 + 前端收到时刻） */
export interface McpApprovalRequest {
  type: 'approval_request';
  id: string;
  sessionId: string;
  command: string;
  expiresInMs: number;
  sessionName?: string;
  host?: string;
  user?: string;
  level?: string;
  production?: boolean;
  parts?: ApprovalPart[];
  /** agent 自述的命令用途（未经验证，仅展示） */
  purpose?: string | null;
  /** 前端收到时刻（Date.now），用于倒计时与到期自动关闭 */
  receivedAt: number;
}
/** 底部 dock 页签（DevTools 风格工具面板）；单值天然互斥 */
export type DockTab = 'sftp' | 'metrics' | 'tunnel' | 'transfer';
/** 通知分级（批次一 7.7）：success/info 短时自动消失，warning 较长，error 常驻手动关 */
export type NotificationLevel = 'success' | 'info' | 'warning' | 'error';

export interface Notice {
  id: number;
  level: NotificationLevel;
  message: string;
  /** 可序列化动作（12.1）：label 展示，actionId 经 notice-actions 注册表分发，arg 为上下文 */
  action?: { label: string; actionId: string; arg?: string };
}

/** 各级别自动消失时长（ms）；null = 常驻手动关闭 */
const NOTICE_TTL: Record<NotificationLevel, number | null> = {
  success: 3000,
  info: 4000,
  warning: 8000,
  error: null,
};

/** 堆叠上限：溢出时优先丢弃最旧的非 error 通知 */
const MAX_NOTICES = 5;

let noticeSeq = 1;
/** 一次性自动消失定时器（非空转轮询；手动关闭时清理） */
const noticeTimers = new Map<number, number>();

export interface Pane {
  id: string;
  session: TerminalSession;
  state: PaneState;
  /** AI 命令镜像（只读分屏）：不连接，内容由 exec-mirror 帧写入 */
  mirror?: boolean;
}

/** pane id → MCP terminal_open 请求 id：首次开链后回报后端（mcp_agent_tab_ready）并移除 */
const agentTabRequests = new Map<string, string>();

/** 取出并移除 pane 的 agent 请求 id（仅首次 attach 有值） */
export function takeAgentRequest(paneId: string): string | undefined {
  const id = agentTabRequests.get(paneId);
  agentTabRequests.delete(paneId);
  return id;
}

/** AI 控制中的后端终端标签：agent = MCP terminal_open 打开，shared = 用户共享 */
export type AiTabKind = 'agent' | 'shared';

/** MCP 前端交互事件帧（后端 mcp_ui.rs / mcp_terminal.rs） */
type McpUiFrame =
  | Omit<McpApprovalRequest, 'receivedAt'>
  | { type: 'agent_terminal_open'; requestId: string; sessionId: string; sessionName?: string }
  | { type: 'agent_terminal_attached'; tabId: string; terminalId: string; kind: AiTabKind }
  /** closeTab：terminal_close 时 AI 自己开的标签为 true（前端一并关闭）、共享标签为 false；
   *  MCP 服务停止清场时缺省（只摘标记） */
  | { type: 'agent_terminal_closed'; tabId: string; closeTab?: boolean }
  | ExecMirrorFrame;

function withoutAiTab(m: Record<string, AiTabKind>, tabId: string): Record<string, AiTabKind> {
  return Object.fromEntries(Object.entries(m).filter(([k]) => k !== tabId));
}

/** 回报 agent 标签开链结果（失败/取消带 error）；后端已超时则忽略 */
export function reportAgentTab(requestId: string, tabId: string | null, error?: string): void {
  void invoke('mcp_agent_tab_ready', { requestId, tabId, error: error ?? null }).catch(
    () => undefined,
  );
}

export interface Tab {
  /** 本地稳定 id（React key） */
  id: string;
  title: string;
  /** 分屏新 pane 复用同一连接目标 */
  target: ConnectTarget;
  layout: LayoutNode;
  panes: Record<string, Pane>;
  activePaneId: string;
}

/** 标签的主终端 pane：活动 pane 是镜像时取第一个真实终端（标签栏/状态栏的连接状态以它为准） */
export function primaryPane(tab: Tab): Pane | undefined {
  const active = tab.panes[tab.activePaneId];
  if (active && !active.mirror) return active;
  return paneIds(tab.layout)
    .map((id) => tab.panes[id])
    .find((p) => p && !p.mirror);
}

let tabSeq = 1;
let paneSeq = 1;
/** 多窗口隔离（M5 标签分离）：窗口 label 作 id 前缀，防跨窗口 tab/pane id 碰撞 */
let idPrefix = '';
/** 本窗口 label（广播跨窗口事件帧 source 字段/回环防护；live binding） */
export let windowLabel = 'main';
/** App 挂载时以窗口 label 初始化一次 */
export function initIdPrefix(label: string): void {
  windowLabel = label;
  idPrefix = label === 'main' ? '' : `${label}-`;
}

// Shell 集成不注入：曾向远程 shell 写 stty/OSC 7 钩子（PROMPT_COMMAND/precmd）
// 与分屏 cd，在密码过期强制改密等登录交互里会污染输入造成死循环。OSC 7 仅被动
// 解析（terminal-session.ts）：服务器 shell 原生上报时「跟随终端目录」仍可用。

/** 目标是否为本地会话（session 档案 kind==='local'）；spec/档案缺失一律按远程处理。
 *  本地会话禁用：SFTP/监控/隧道等 SSH 专属功能 */
export function isLocalTarget(target: ConnectTarget): boolean {
  if (target.kind !== 'session') return false;
  // 建标签时已定死 kind，直接读；旧标签（无 sessionKind）回落查表
  if (target.sessionKind) return target.sessionKind === 'local';
  const rec = useAppStore.getState().sessions.find((r) => r.id === target.sessionId);
  return rec?.kind === 'local';
}

interface AppStore {
  tabs: Tab[];
  activeId: string | null;
  showConnect: boolean;
  /** 决策帧队列：多 pane 并发弹窗时逐个处理 */
  pendingHostKeys: HostKeyPromptFrame[];
  pendingKis: KiChallengeFrame[];
  /** MCP 危险命令审批队列（逐个弹窗） */
  pendingApprovals: McpApprovalRequest[];
  /** 订阅 MCP 前端交互事件（App 挂载时调用一次） */
  subscribeMcpUi(): void;
  /** 应答审批并出队；到期未答的由弹窗以 allow=false 出队 */
  /** always：批准且本会话此命令以后免确认（后端内存规则，应用重启失效） */
  answerApproval(id: string, allow: boolean, always?: boolean): void;
  /** AI 控制中的终端（键 = 后端 tabId） */
  aiTabs: Record<string, AiTabKind>;
  /** 把终端共享给 AI / 收回 AI 控制（参数为后端 tabId） */
  shareTabWithAi(backendTabId: string): Promise<void>;
  revokeAiTab(backendTabId: string): Promise<void>;

  /** 打开连接对话框；editTarget=编辑既有会话，presetGroup=预填分组路径（分组菜单「新建连接」） */
  openConnect(editTarget?: SessionRecord, presetGroup?: string): void;
  closeConnect(): void;
  /** 命令面板（Ctrl+Shift+P） */
  paletteOpen: boolean;
  togglePalette(): void;
  /** 命令片段管理框 */
  snippetsOpen: boolean;
  toggleSnippets(): void;
  /** 会话录像管理框 */
  recordingsOpen: boolean;
  toggleRecordings(): void;
  /** 录制中的终端（键 = 后端 tabId；仅记录用户手动开始的录像） */
  recTabs: Record<string, true>;
  startRecording(backendTabId: string, title: string): Promise<void>;
  stopRecording(backendTabId: string): Promise<void>;
  /** 分级通知堆叠（toast）；12.1：可选可序列化动作（actionId 经 notice-actions 注册表分发） */
  notices: Notice[];
  notify(
    message: string,
    level?: NotificationLevel,
    action?: { label: string; actionId: string; arg?: string },
  ): void;
  dismissNotice(id: number): void;
  /** 终端 bell 待读标签（12.6）：激活即清 */
  bellTabs: string[];
  markBell(tabId: string): void;
  /** 全局传输活跃数（12.2 状态栏）：由 SFTP 面板现有订阅顺带发布；null=无订阅来源不显示 */
  transferActive: number | null;
  setTransferActive(n: number | null): void;
  /** 快速连接对话框（12.5 空态；不保存档案的临时连接） */
  quickConnectOpen: boolean;
  toggleQuickConnect(): void;
  /** 断开单个 pane（12.4 命令面板「断开当前连接」；终态 closed，终端内容保留） */
  disconnectPane(tabId: string, paneId: string): void;
  /** 待确认删除的会话档案（删除会级联清凭据，必须确认） */
  pendingDeleteSession: SessionRecord | null;
  requestDeleteSession(rec: SessionRecord): void;
  confirmDeleteSession(): Promise<void>;
  cancelDeleteSession(): void;
  /** 待确认关闭的标签 id 列表（任一含活跃连接时汇总确认；单标签为单元素数组） */
  pendingCloseTabs: string[] | null;
  confirmCloseTab(): void;
  cancelCloseTab(): void;
  /** 已关闭标签栈（批次十一：Ctrl+Shift+T 重开；仅 session 类标签可重连，新→旧，cap 10） */
  closedTabs: { sessionId: string; title: string }[];
  /** 弹栈重开最近关闭的标签（复用 connectBySession 重连） */
  reopenClosedTab(): void;
  /** 请求关闭一组标签：确认守卫命中时汇总弹一次确认（§17.2 说明影响） */
  requestCloseTabs(ids: string[]): void;
  /** 断开标签全部连接但保留标签（pane 终态 closed，终端内容保留） */
  disconnectTab(id: string): void;
  /** 重连标签全部 pane（复用各 pane 在 TerminalView 注册的原位重连闭包） */
  reconnectTab(id: string): void;
  closeOtherTabs(id: string): void;
  closeTabsToRight(id: string): void;
  closeAllTabs(): void;
  /** 导入/导出（错误也走 notices） */
  exportConfig(encrypted: boolean, passphrase?: string): Promise<void>;
  importConfigFile(path: string, passphrase?: string): Promise<void>;
  connect(spec: TermOpenSpec): void;
  connectBySession(sessionId: string, title: string): void;
  /** 连接语义：已有该会话标签则激活，否则新标签 */
  connectOrActivate(sessionId: string, title: string): void;
  /** 标签分离：新窗口连接（TabBar ⧉ 与右键菜单共用） */
  connectInNewWindow(sessionId: string, title: string): void;
  /** 复制服务器档案（新 id + 「副本」后缀） */
  duplicateSession(rec: SessionRecord): Promise<void>;
  /** 收藏切换（KV: sessions.favorites） */
  toggleFavorite(sessionId: string): void;
  /** 最近连接记录（KV: sessions.recent，cap 20，新→旧） */
  recordRecent(sessionId: string): void;
  /** 连接失败记录（KV: sessions.failed，cap 20）；成功连接时清除 */
  recordConnectFailure(sessionId: string, message: string): void;
  clearConnectFailure(sessionId: string): void;
  /** 分组 KV 集合更新（extras/collapsed 共用改写入口） */
  setGroupList(key: 'groups.extra' | 'groups.collapsed', list: string[]): void;
  /** 会话档案 CRUD（秘密经 cred_set 单独进保险库） */
  loadSessions(): Promise<void>;
  deleteSession(id: string): Promise<void>;
  /** 编辑既有会话（null=新建） */
  editing: SessionRecord | null;
  /** 新建连接时预填的分组路径（null=不预填） */
  connectPreset: string | null;
  sessions: SessionRecord[];
  sidebarOpen: boolean;
  toggleSidebar(): void;

  /** 内部：订阅去重标记 */
  _tunnelsSubscribed: boolean;
  /** 底部 dock（窗口级）：当前激活的工具页签；null = 收起。
   *  单值天然互斥（替代原 sftpOpen/metricsOpen 逐标签互斥）。
   *  'transfer' 与 transfer-store.open 联动（openDock/closeDock 内同步，订阅逻辑不变） */
  dockTab: DockTab | null;
  openDock(tab: DockTab): void;
  closeDock(): void;
  /** 同页签再次触发 = 关闭 */
  toggleDock(tab: DockTab): void;
  /** 应用设置（settings KV 全量缓存；启动时 loadSettings 拉一次） */
  settings: Record<string, unknown>;
  settingsLoaded: boolean;
  loadSettings(): Promise<void>;
  /** 乐观本地更新 + 落库（theme/terminal 键的副作用由 App 效果层统一应用） */
  setSetting(key: string, value: unknown): void;
  /** 设置面板开关 */
  settingsOpen: boolean;
  toggleSettings(): void;
  /** 隧道面板 */
  tunnels: TunnelInfo[];
  /** 持久化隧道定义 */
  tunnelDefs: TunnelDef[];
  /** 1Hz 订阅（App 挂载时调用一次；重复调用幂等） */
  subscribeTunnels(): void;
  stopTunnel(id: string): Promise<void>;
  loadTunnelDefs(): Promise<void>;
  /** 保存定义；start=true 时立即建立 */
  saveTunnel(def: TunnelDef, start: boolean): Promise<void>;
  deleteTunnel(id: string): Promise<void>;
  /** 复制定义（新 id，名称加「副本」，不启动） */
  duplicateTunnel(def: TunnelDef): Promise<void>;
  /** §9.6 连接反馈：随会话隧道启动结果汇总成通知 */
  notifySessionTunnels(sessionId: string, results: SessionTunnelResult[]): void;
  splitActive(dir: 'row' | 'col'): void;
  /** 广播输入开关（11）：开启后输入同步写入本窗口全部已连接 pane，并经事件转发其它窗口 */
  broadcastEnabled: boolean;
  toggleBroadcast(): void;
  closePane(tabId: string, paneId: string): void;
  setActive(id: string): void;
  /** 拖拽重排：把 dragId 移到 targetId 之前 */
  moveTab(dragId: string, targetId: string): void;
  setActivePane(tabId: string, paneId: string): void;
  setSplitRatio(tabId: string, splitId: string, ratio: number): void;
  setPaneState(tabId: string, paneId: string, state: PaneState): void;
  closeTab(id: string): void;
  shiftHostKey(): void;
  shiftKi(): void;
}

/** MCP 交互事件订阅只建一次（HMR/StrictMode 重复挂载不重复订阅） */
let mcpUiSubscribed = false;

export const useAppStore = create<AppStore>((set, get) => {
  /** 标签活跃连接数（connected/connecting/reconnecting 计为活跃） */
  const countLive = (tab: Tab): number =>
    paneIds(tab.layout).filter((pid) => {
      const st = tab.panes[pid]?.state;
      return st === 'connected' || st === 'connecting' || st === 'reconnecting';
    }).length;

  /** 实际执行关标签：关闭全部 pane 会话并移除标签（确认守卫见 requestCloseTabs） */
  const doCloseTab = (id: string) => {
    const { tabs, activeId } = get();
    const tab = tabs.find((t) => t.id === id);
    if (tab)
      for (const pid of paneIds(tab.layout)) {
        const req = takeAgentRequest(pid);
        if (req) reportAgentTab(req, null, tNow('state.aiTabClosedByUser'));
        void tab.panes[pid]?.session.close();
        mirrorFeeds.delete(pid);
      }
    const next = tabs.filter((t) => t.id !== id);
    set((s) => ({
      tabs: next,
      activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
      bellTabs: s.bellTabs.filter((t) => t !== id),
      // 批次十一：session 类标签压入已关闭栈（cap 10），供 Ctrl+Shift+T 重开；
      // 快速连接（spec）无档案可重连，不入栈
      closedTabs:
        tab && tab.target.kind === 'session'
          ? [...s.closedTabs, { sessionId: tab.target.sessionId, title: tab.title }].slice(-10)
          : s.closedTabs,
    }));
  };

  const makePane = (tabId: string): Pane => {
    const id = `${idPrefix}p${paneSeq++}`;
    const onEvent = (ev: TermEvent) => {
      if (ev.type === 'hostkey_prompt')
        set((s) => ({ pendingHostKeys: [...s.pendingHostKeys, ev] }));
      else if (ev.type === 'ki_challenge') set((s) => ({ pendingKis: [...s.pendingKis, ev] }));
      else if (ev.type === 'session_tunnels') get().notifySessionTunnels(ev.sessionId, ev.results);
      else if (ev.type === 'macro_skipped') get().notify(tNow('state.macroSkipped'), 'warning');
      else {
        handleSessionState(set, tabId, id, ev);
        // 连接成功（含重连成功）→ 清掉该会话的「最近失败」记录
        if (ev.type === 'session_state' && ev.state === 'connected') {
          const tab = get().tabs.find((t) => t.id === tabId);
          if (tab && tab.target.kind === 'session') get().clearConnectFailure(tab.target.sessionId);
          // 连接成功（含重连）不向远程 shell 注入任何字节——批次六的 stty/OSC 7 钩子
          // 与分屏 cd 已移除：密码过期强制改密等登录交互会被注入污染成死循环
        }
      }
    };
    const session = new TerminalSession(onEvent, (m, l) => get().notify(m, l));
    // 广播输入（11）：输入帧旁路钩子，开关状态在触发时读取
    session.inputHook = (data) => {
      if (get().broadcastEnabled) broadcastInput(tabId, id, data);
    };
    return { id, session, state: 'connecting' };
  };

  /** agentRequestId 非空 = MCP agent 请求的标签：不抢焦点（已有活跃标签时），仅通知 */
  const openTabWithTarget = (
    target: ConnectTarget,
    title: string,
    agentRequestId?: string,
  ): string => {
    const tabId = `${idPrefix}tab${tabSeq++}`;
    const pane = makePane(tabId);
    if (agentRequestId) {
      agentTabRequests.set(pane.id, agentRequestId);
      set((s) => ({
        tabs: [
          ...s.tabs,
          {
            id: tabId,
            title,
            target,
            layout: leaf(pane.id),
            panes: { [pane.id]: pane },
            activePaneId: pane.id,
          },
        ],
        activeId: s.activeId ?? tabId,
      }));
      return tabId;
    }
    const tab: Tab = {
      id: tabId,
      title,
      target,
      layout: leaf(pane.id),
      panes: { [pane.id]: pane },
      activePaneId: pane.id,
    };
    set((s) => ({ tabs: [...s.tabs, tab], activeId: tabId, showConnect: false }));
    return tabId;
  };

  const isSessionTab = (t: Tab, sessionId: string) =>
    t.target.kind === 'session' && t.target.sessionId === sessionId;

  /** 会话已有镜像 pane 的 id（执行中途用户关掉镜像则为 undefined） */
  const findMirrorPane = (sessionId: string): string | undefined => {
    for (const t of get().tabs) {
      if (!isSessionTab(t, sessionId)) continue;
      const p = Object.values(t.panes).find((x) => x.mirror);
      if (p) return p.id;
    }
    return undefined;
  };

  /**
   * 会话的 AI 命令镜像 pane：挂在该会话的终端标签里（下方只读分屏）。
   * 优先当前标签，其次第一个该会话标签；都没有则先打开该会话的终端。
   * 新挂镜像时切到所在标签（让用户看到 AI 在做什么），但不抢 pane 焦点。
   */
  const ensureMirrorPane = (sessionId: string, sessionName: string): string => {
    const existing = findMirrorPane(sessionId);
    if (existing) return existing;
    const { tabs, activeId, sessions } = get();
    const active = tabs.find((t) => t.id === activeId);
    let tabId = (active && isSessionTab(active, sessionId) ? active : undefined)?.id;
    tabId ??= tabs.find((t) => isSessionTab(t, sessionId))?.id;
    if (!tabId) {
      const rec = sessions.find((r) => r.id === sessionId);
      tabId = openTabWithTarget(
        { kind: 'session', sessionId, sessionKind: 'ssh', encoding: rec?.encoding ?? null },
        rec?.name ?? sessionName,
      );
    }
    // 不 attach（无后端连接）；终态 closed：不计活跃连接、不进广播、关闭无需确认
    const pane: Pane = { ...makePane(tabId), state: 'closed', mirror: true };
    const target = tabId;
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === target
          ? {
              ...t,
              layout: appendLeaf(t.layout, 'col', pane.id, 0.6),
              panes: { ...t.panes, [pane.id]: pane },
            }
          : t,
      ),
      activeId: target,
    }));
    return pane.id;
  };

  const onExecMirror = (frame: ExecMirrorFrame) => {
    // 只在主窗口汇入，避免分离窗口重复开标签
    if (windowLabel !== 'main') return;
    if (frame.type === 'exec_start') {
      const paneId = ensureMirrorPane(frame.sessionId, frame.sessionName);
      feedOf(paneId).write(formatStart(frame, new Date()));
      return;
    }
    // 执行中途用户关了镜像：其余输出丢弃，下条命令再挂
    const paneId = findMirrorPane(frame.sessionId);
    if (!paneId) return;
    const feed = feedOf(paneId);
    if (frame.type === 'exec_output') feed.write(formatOutput(frame));
    else
      feed.write(
        formatEnd(frame, feed.atLineStart, {
          omitted: (n) => tNow('state.mirrorOmitted', { n }),
          timeout: tNow('state.mirrorTimeout'),
          error: tNow('state.mirrorError'),
        }),
      );
  };

  return {
    tabs: [],
    activeId: null,
    showConnect: false,
    pendingHostKeys: [],
    pendingKis: [],
    pendingApprovals: [],
    aiTabs: {},
    editing: null,
    connectPreset: null,
    sessions: [],
    sidebarOpen: true,
    tunnels: [],
    tunnelDefs: [],
    _tunnelsSubscribed: false,
    dockTab: null,
    settings: {},
    settingsLoaded: false,
    settingsOpen: false,

    toggleSettings: () => set((s) => ({ settingsOpen: !s.settingsOpen })),

    loadSettings: async () => {
      const res = await invoke<{ settings: Record<string, unknown> }>('settings_list');
      set({ settings: res.settings, settingsLoaded: true });
    },

    setSetting: (key, value) => {
      set((s) => ({ settings: { ...s.settings, [key]: value } }));
      invoke('settings_set', { key, value }).catch(() => undefined);
    },

    openDock: (tab) => {
      // 防线：本地会话无 SSH 通道，任何调用路径（含漏检入口）都不许打开
      const active = get().tabs.find((t) => t.id === get().activeId);
      if (
        (tab === 'sftp' || tab === 'metrics' || tab === 'tunnel') &&
        active &&
        isLocalTarget(active.target)
      ) {
        get().notify(
          tab === 'sftp'
            ? tNow('state.localNoSftp')
            : tab === 'metrics'
              ? tNow('state.localNoMetrics')
              : tNow('state.localNoTunnels'),
          'warning',
        );
        return;
      }
      set({ dockTab: tab });
      // 传输中心内容仍订阅 transfer-store.open（保持订阅建立逻辑不变）
      useTransferStore.getState().setOpen(tab === 'transfer');
    },

    closeDock: () => {
      set({ dockTab: null });
      useTransferStore.getState().setOpen(false);
    },

    toggleDock: (tab) => {
      if (get().dockTab === tab) get().closeDock();
      else get().openDock(tab);
    },

    subscribeMcpUi: () => {
      if (mcpUiSubscribed) return;
      mcpUiSubscribed = true;
      const events = new Channel<McpUiFrame>();
      events.onmessage = (frame) => {
        if (frame.type === 'approval_request') {
          set((s) =>
            s.pendingApprovals.some((a) => a.id === frame.id)
              ? s
              : { pendingApprovals: [...s.pendingApprovals, { ...frame, receivedAt: Date.now() }] },
          );
        } else if (frame.type === 'agent_terminal_open') {
          // 标签窗口只在主窗口开，避免分离窗口重复开
          if (windowLabel !== 'main') return;
          const rec = get().sessions.find((r) => r.id === frame.sessionId);
          if (!rec || rec.kind === 'local') {
            reportAgentTab(frame.requestId, null, tNow('state.aiTabNoSession'));
            return;
          }
          openTabWithTarget(
            {
              kind: 'session',
              sessionId: rec.id,
              sessionKind: 'ssh',
              encoding: rec.encoding ?? null,
            },
            `AI · ${frame.sessionName ?? rec.name}`,
            frame.requestId,
          );
          get().notify(tNow('state.aiTabOpened', { name: frame.sessionName ?? rec.name }), 'info');
        } else if (frame.type === 'agent_terminal_attached') {
          set((s) => ({ aiTabs: { ...s.aiTabs, [frame.tabId]: frame.kind } }));
        } else if (frame.type === 'agent_terminal_closed') {
          set((s) => ({ aiTabs: withoutAiTab(s.aiTabs, frame.tabId) }));
          if (frame.closeTab === undefined) return;
          const tab = get().tabs.find((t) =>
            Object.values(t.panes).some((p) => p.session.tabId === frame.tabId),
          );
          if (!tab) return;
          if (!frame.closeTab) {
            get().notify(tNow('state.aiReleased', { name: tab.title }), 'info');
            return;
          }
          const pane = Object.values(tab.panes).find((p) => p.session.tabId === frame.tabId);
          const terminals = Object.values(tab.panes).filter((p) => !p.mirror);
          // AI 自己开的标签：AI 已关闭，不再弹活跃连接确认；分屏里还有别的终端时只关这一格
          if (pane && terminals.length > 1) get().closePane(tab.id, pane.id);
          else doCloseTab(tab.id);
          get().notify(tNow('state.aiTabClosedByAi', { name: tab.title }), 'info');
        } else if (
          frame.type === 'exec_start' ||
          frame.type === 'exec_output' ||
          frame.type === 'exec_end'
        ) {
          onExecMirror(frame);
        }
      };
      void invoke('mcp_ui_subscribe', { events });
    },

    answerApproval: (id, allow, always = false) => {
      void invoke('mcp_approve', { id, allow, always }).catch(() => undefined);
      set((s) => ({ pendingApprovals: s.pendingApprovals.filter((a) => a.id !== id) }));
    },

    shareTabWithAi: async (backendTabId) => {
      try {
        const title = get().tabs.find((t) =>
          Object.values(t.panes).some((p) => p.session.tabId === backendTabId),
        )?.title;
        await invoke('mcp_share_tab', { tabId: backendTabId, title: title ?? null });
        set((s) => ({ aiTabs: { ...s.aiTabs, [backendTabId]: 'shared' } }));
        get().notify(tNow('state.aiShared'), 'success');
      } catch (e) {
        get().notify(tNow('state.aiShareFailed', { error: String(e) }), 'error');
      }
    },

    revokeAiTab: async (backendTabId) => {
      await invoke('mcp_unshare_tab', { tabId: backendTabId }).catch(() => undefined);
      set((s) => ({ aiTabs: withoutAiTab(s.aiTabs, backendTabId) }));
      get().notify(tNow('state.aiRevoked'), 'info');
    },

    subscribeTunnels: () => {
      if (get()._tunnelsSubscribed) return;
      set({ _tunnelsSubscribed: true });
      const events = new Channel<{ tunnels: TunnelInfo[] }>();
      events.onmessage = (frame) => set({ tunnels: frame.tunnels });
      void invoke('tunnel_subscribe', { events });
    },

    stopTunnel: async (id) => {
      await invoke('tunnel_stop', { tunnelId: id });
    },

    loadTunnelDefs: async () => {
      const defs = await invoke<TunnelDef[]>('tunnel_defs');
      set({ tunnelDefs: defs });
    },

    saveTunnel: async (def, start) => {
      await invoke('tunnel_save', { def: { ...def, start } });
      await get().loadTunnelDefs();
    },

    deleteTunnel: async (id) => {
      await invoke('tunnel_delete', { tunnelId: id });
      await get().loadTunnelDefs();
    },
    duplicateTunnel: async (def) => {
      await get().saveTunnel(
        {
          ...def,
          id: `td-${crypto.randomUUID()}`,
          name: tNow('state.copyName', { name: tunnelDisplayName(def) }),
        },
        false,
      );
      get().notify(tNow('state.tunnelDuplicated'), 'success');
    },

    notifySessionTunnels: (sessionId, results) => {
      const name = get().sessions.find((s) => s.id === sessionId)?.name ?? sessionId;
      const fb = tunnelFeedback(name, results);
      if (fb) get().notify(fb.message, fb.level);
    },

    toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),

    loadSessions: async () => {
      const sessions = await invoke<SessionRecord[]>('session_list');
      set({ sessions });
    },

    deleteSession: async (id) => {
      await invoke('session_delete', { sessionId: id });
      await get().loadSessions();
    },

    connectBySession: (sessionId, title) => {
      const rec = get().sessions.find((r) => r.id === sessionId);
      openTabWithTarget(
        {
          kind: 'session',
          sessionId,
          sessionKind: rec?.kind === 'local' ? 'local' : 'ssh',
          encoding: rec?.encoding ?? null,
        },
        title,
      );
      get().recordRecent(sessionId);
    },
    connectOrActivate: (sessionId, title) => {
      const existing = get().tabs.find(
        (t) => t.target.kind === 'session' && t.target.sessionId === sessionId,
      );
      if (existing) set({ activeId: existing.id });
      else get().connectBySession(sessionId, title);
    },

    connectInNewWindow: (sessionId, title) => {
      const label = `det-${sessionId}`.replace(/[^a-zA-Z0-9-]/g, '-');
      const win = new WebviewWindow(label, {
        url: `index.html?detach=${encodeURIComponent(sessionId)}`,
        title: `${title} · mySSH`,
        width: 1200,
        height: 800,
        // 与主窗口 tauri.conf.json 一致开启原生拖放（批次十三：真实路径 + 冲突确认）
        dragDropEnabled: true,
      });
      void win.once('tauri://error', () => undefined);
    },

    duplicateSession: async (rec) => {
      // 名称自动去重：「X 副本」「X 副本 2」…
      const names = new Set(get().sessions.map((x) => x.name));
      let name = tNow('state.copyName', { name: rec.name });
      for (let i = 2; names.has(name); i++)
        name = tNow('state.copyNameN', { name: rec.name, n: i });
      const copy: SessionRecord = {
        ...rec,
        id: crypto.randomUUID(),
        name,
        jumpChain: [...rec.jumpChain],
        tags: [...rec.tags],
      };
      try {
        await invoke('session_upsert', { record: copy });
        // 端口转发随档案克隆（新 id、绑定新会话、不启动）；
        // 凭据存保险库按会话 id 取且不可回读，按现有模型不复制，需重新录入
        // tunnelDefs 可能尚未加载（隧道弹层从未打开过），先拉取再筛，否则静默丢克隆
        await get().loadTunnelDefs();
        const tunnels = get().tunnelDefs.filter((t) => t.sessionId === rec.id);
        for (const t of tunnels) {
          await invoke('tunnel_save', {
            def: { ...t, id: `td-${crypto.randomUUID()}`, sessionId: copy.id, start: false },
          });
        }
        await get().loadSessions();
        if (tunnels.length > 0) await get().loadTunnelDefs();
        get().notify(
          tunnels.length > 0
            ? tNow('state.sessionDuplicatedWithTunnels', { name, count: tunnels.length })
            : tNow('state.sessionDuplicated', { name }),
          'success',
        );
      } catch (e) {
        get().notify(tNow('state.duplicateSessionFailed', { error: String(e) }), 'error');
      }
    },

    toggleFavorite: (sessionId) => {
      const cur = new Set(readStringList(get().settings[GROUP_KEYS.favorites]));
      const had = cur.has(sessionId);
      if (had) cur.delete(sessionId);
      else cur.add(sessionId);
      get().setSetting(GROUP_KEYS.favorites, [...cur]);
      get().notify(had ? tNow('state.unfavorited') : tNow('state.favorited'), 'success');
    },

    recordRecent: (sessionId) => {
      const cur = readStringList(get().settings[GROUP_KEYS.recent]);
      const next = [sessionId, ...cur.filter((id) => id !== sessionId)].slice(0, 20);
      get().setSetting(GROUP_KEYS.recent, next);
    },

    recordConnectFailure: (sessionId, message) => {
      const cur = readFailedList(get().settings[GROUP_KEYS.failed]);
      const next: FailedEntry[] = [
        { id: sessionId, message, ts: Date.now() },
        ...cur.filter((f) => f.id !== sessionId),
      ].slice(0, 20);
      get().setSetting(GROUP_KEYS.failed, next);
    },

    clearConnectFailure: (sessionId) => {
      const cur = readFailedList(get().settings[GROUP_KEYS.failed]);
      if (cur.some((f) => f.id === sessionId))
        get().setSetting(
          GROUP_KEYS.failed,
          cur.filter((f) => f.id !== sessionId),
        );
    },

    setGroupList: (key, list) => get().setSetting(key, [...new Set(list)]),

    openConnect: (editTarget, presetGroup) =>
      set({ showConnect: true, editing: editTarget ?? null, connectPreset: presetGroup ?? null }),
    closeConnect: () => set({ showConnect: false, editing: null, connectPreset: null }),

    paletteOpen: false,
    togglePalette: () => set((s) => ({ paletteOpen: !s.paletteOpen })),
    snippetsOpen: false,
    toggleSnippets: () => set((s) => ({ snippetsOpen: !s.snippetsOpen })),
    recordingsOpen: false,
    toggleRecordings: () => set((s) => ({ recordingsOpen: !s.recordingsOpen })),
    recTabs: {},
    startRecording: async (backendTabId, title) => {
      try {
        await invoke('term_record_start', { tabId: backendTabId, title });
        set((s) => ({ recTabs: { ...s.recTabs, [backendTabId]: true } }));
        get().notify(tNow('state.recStarted'), 'success');
      } catch (e) {
        get().notify(tNow('state.recFailed', { error: String(e) }), 'error');
      }
    },
    stopRecording: async (backendTabId) => {
      set((s) => ({
        recTabs: Object.fromEntries(Object.entries(s.recTabs).filter(([k]) => k !== backendTabId)),
      }));
      try {
        const meta = await invoke<{ title: string } | null>('term_record_stop', {
          tabId: backendTabId,
        });
        if (meta) get().notify(tNow('state.recStopped', { title: meta.title }), 'success');
      } catch (e) {
        get().notify(tNow('state.recFailed', { error: String(e) }), 'error');
      }
    },
    broadcastEnabled: false,
    toggleBroadcast: () => set((s) => ({ broadcastEnabled: !s.broadcastEnabled })),
    quickConnectOpen: false,
    toggleQuickConnect: () => set((s) => ({ quickConnectOpen: !s.quickConnectOpen })),
    bellTabs: [],
    markBell: (tabId) =>
      set((s) => (s.bellTabs.includes(tabId) ? {} : { bellTabs: [...s.bellTabs, tabId] })),
    transferActive: null,
    setTransferActive: (n) => set({ transferActive: n }),
    disconnectPane: (tabId, paneId) => {
      const tab = get().tabs.find((t) => t.id === tabId);
      const pane = tab?.panes[paneId];
      if (!tab || !pane) return;
      if (
        pane.state === 'connected' ||
        pane.state === 'connecting' ||
        pane.state === 'reconnecting'
      ) {
        void pane.session.close();
        get().setPaneState(tabId, paneId, 'closed');
      }
    },
    notices: [],
    notify: (message, level = 'info', action) => {
      const id = noticeSeq++;
      set((s) => {
        const notices = [...s.notices, { id, level, message, action }];
        while (notices.length > MAX_NOTICES) {
          const idx = notices.findIndex((n) => n.level !== 'error');
          const [dropped] = notices.splice(idx >= 0 ? idx : 0, 1);
          const t = noticeTimers.get(dropped.id);
          if (t !== undefined) {
            clearTimeout(t);
            noticeTimers.delete(dropped.id);
          }
        }
        return { notices };
      });
      const ttl = NOTICE_TTL[level];
      if (ttl !== null) {
        noticeTimers.set(
          id,
          setTimeout(() => {
            noticeTimers.delete(id);
            get().dismissNotice(id);
          }, ttl),
        );
      }
    },
    dismissNotice: (id) => {
      const t = noticeTimers.get(id);
      if (t !== undefined) {
        clearTimeout(t);
        noticeTimers.delete(id);
      }
      set((s) => ({ notices: s.notices.filter((n) => n.id !== id) }));
    },
    pendingDeleteSession: null,
    requestDeleteSession: (rec) => set({ pendingDeleteSession: rec }),
    cancelDeleteSession: () => set({ pendingDeleteSession: null }),
    confirmDeleteSession: async () => {
      const rec = get().pendingDeleteSession;
      if (!rec) return;
      set({ pendingDeleteSession: null });
      try {
        await get().deleteSession(rec.id);
        get().notify(tNow('state.sessionDeleted', { name: rec.name }), 'success');
      } catch (e) {
        get().notify(tNow('state.deleteSessionFailed', { error: String(e) }), 'error');
      }
    },
    pendingCloseTabs: null,
    confirmCloseTab: () => {
      const ids = get().pendingCloseTabs;
      set({ pendingCloseTabs: null });
      if (ids) for (const id of ids) doCloseTab(id);
    },
    cancelCloseTab: () => set({ pendingCloseTabs: null }),
    closedTabs: [],
    reopenClosedTab: () => {
      const stack = get().closedTabs;
      const top = stack[stack.length - 1];
      if (!top) return;
      set({ closedTabs: stack.slice(0, -1) });
      // 档案可能已删除：connectBySession 照常开标签，连接失败走既有 error 终态
      get().connectBySession(top.sessionId, top.title);
    },

    requestCloseTabs: (ids) => {
      const { tabs, settings } = get();
      const want = new Set(ids);
      const targets = tabs.filter((t) => want.has(t.id));
      if (targets.length === 0) return;
      const live = targets.reduce((n, t) => n + countLive(t), 0);
      if (settings['terminal.confirmCloseTab'] !== false && live > 0) {
        set({ pendingCloseTabs: targets.map((t) => t.id) });
        return;
      }
      for (const t of targets) doCloseTab(t.id);
    },

    disconnectTab: (id) => {
      const tab = get().tabs.find((t) => t.id === id);
      if (!tab) return;
      for (const pid of paneIds(tab.layout)) {
        const p = tab.panes[pid];
        if (!p) continue;
        if (p.state === 'connected' || p.state === 'connecting' || p.state === 'reconnecting') {
          void p.session.close();
          get().setPaneState(id, pid, 'closed');
        }
      }
    },

    reconnectTab: (id) => {
      const tab = get().tabs.find((t) => t.id === id);
      if (!tab) return;
      for (const pid of paneIds(tab.layout)) reconnectRegistry.get(pid)?.();
    },

    closeOtherTabs: (id) =>
      get().requestCloseTabs(
        get()
          .tabs.filter((t) => t.id !== id)
          .map((t) => t.id),
      ),

    closeTabsToRight: (id) => {
      const idx = get().tabs.findIndex((t) => t.id === id);
      if (idx < 0) return;
      get().requestCloseTabs(
        get()
          .tabs.slice(idx + 1)
          .map((t) => t.id),
      );
    },

    closeAllTabs: () => get().requestCloseTabs(get().tabs.map((t) => t.id)),

    exportConfig: async (encrypted, passphrase) => {
      try {
        const r = await invoke<{ path: string }>('config_export', {
          encrypted,
          passphrase: passphrase ?? null,
        });
        // 12.1：通知附加动作（actionId 注册表分发，arg 为导出文件路径）
        get().notify(tNow('state.exported', { path: r.path }), 'success', {
          label: tNow('state.openInExplorer'),
          actionId: 'open-in-explorer',
          arg: r.path,
        });
      } catch (e) {
        get().notify(tNow('state.exportFailed', { error: String(e) }), 'error');
      }
    },

    importConfigFile: async (path, passphrase) => {
      try {
        const r = await invoke<{ sessions: number; tunnels: number; credentials: number }>(
          'config_import',
          { path, passphrase: passphrase ?? null },
        );
        await get().loadSessions();
        await get().loadTunnelDefs();
        get().notify(
          tNow('state.importDone', {
            sessions: r.sessions,
            tunnels: r.tunnels,
            credentials: r.credentials,
          }),
          'success',
        );
      } catch (e) {
        get().notify(tNow('state.importFailed', { error: String(e) }), 'error');
      }
    },

    connect: (spec) => {
      openTabWithTarget({ kind: 'spec', spec }, `${spec.user}@${spec.host}`);
    },

    splitActive: (dir) => {
      const { tabs, activeId } = get();
      const tab = tabs.find((t) => t.id === activeId);
      if (!tab) return;
      const pane = makePane(tab.id);
      // 6b：新 pane 复用 tab.target（TerminalView 以同一 target attach = 同服务器新通道）。
      // 不再注入 cd 继承源 pane cwd（注入移除，见 makePane 内说明）
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tab.id
            ? {
                ...t,
                layout: splitLeaf(t.layout, t.activePaneId, dir, pane.id),
                panes: { ...t.panes, [pane.id]: pane },
                activePaneId: pane.id,
              }
            : t,
        ),
      }));
    },

    closePane: (tabId, paneId) => {
      const { tabs } = get();
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) return;
      const layout = removeLeaf(tab.layout, paneId);
      // 最后一叶（或只剩 AI 镜像）：走整标签关闭入口（含活跃连接确认守卫；
      // session 统一由确认后的 doCloseTab 关闭，避免取消确认留下僵尸 pane）
      if (!layout || paneIds(layout).every((id) => tab.panes[id]?.mirror)) {
        get().closeTab(tabId);
        return;
      }
      const pane = tab.panes[paneId];
      if (pane) void pane.session.close();
      mirrorFeeds.delete(paneId);
      const panes = Object.fromEntries(Object.entries(tab.panes).filter(([k]) => k !== paneId));
      const activePaneId =
        tab.activePaneId === paneId
          ? (paneIds(layout).find((id) => !panes[id]?.mirror) ?? firstLeaf(layout))
          : tab.activePaneId;
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, layout, panes, activePaneId } : t)),
      }));
    },

    // 激活即清 bell 待读标记（12.6）
    setActive: (id) => set((s) => ({ activeId: id, bellTabs: s.bellTabs.filter((t) => t !== id) })),

    moveTab: (dragId, targetId) =>
      set((s) => {
        if (dragId === targetId) return {};
        const from = s.tabs.findIndex((t) => t.id === dragId);
        if (from < 0) return {};
        const tabs = [...s.tabs];
        const [moved] = tabs.splice(from, 1);
        const to = tabs.findIndex((t) => t.id === targetId);
        tabs.splice(to < 0 ? tabs.length : to, 0, moved);
        return { tabs };
      }),

    setActivePane: (tabId, paneId) =>
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, activePaneId: paneId } : t)),
      })),

    setSplitRatio: (tabId, splitId, ratio) =>
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId ? { ...t, layout: setRatio(t.layout, splitId, ratio) } : t,
        ),
      })),

    setPaneState: (tabId, paneId, state) =>
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId
            ? {
                ...t,
                panes: {
                  ...t.panes,
                  [paneId]: { ...t.panes[paneId], state },
                },
              }
            : t,
        ),
      })),

    /** 关标签守卫（批次一 7.6）：并入 requestCloseTabs（单标签即单元素数组） */
    closeTab: (id) => get().requestCloseTabs([id]),

    shiftHostKey: () => set((s) => ({ pendingHostKeys: s.pendingHostKeys.slice(1) })),
    shiftKi: () => set((s) => ({ pendingKis: s.pendingKis.slice(1) })),
  };
});

function handleSessionState(
  set: (fn: (s: AppStore) => Partial<AppStore>) => void,
  tabId: string,
  paneId: string,
  ev: SessionStateFrame,
): void {
  set((s) => ({
    tabs: s.tabs.map((t) =>
      t.id === tabId
        ? { ...t, panes: { ...t.panes, [paneId]: { ...t.panes[paneId], state: ev.state } } }
        : t,
    ),
  }));
}
