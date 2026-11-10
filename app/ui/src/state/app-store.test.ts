import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: unknown = null;
  },
}));
vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(() => Promise.resolve()),
  listen: vi.fn(() => Promise.resolve(() => undefined)),
}));

import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { BROADCAST_INPUT_EVENT, initBroadcastReceiver } from '../term/broadcast';
import { APPROVAL_AUTO_RUN_MS, takeAgentRequest, useAppStore } from './app-store';
import { feedOf } from '../term/exec-mirror';
import { useAiActivity } from './ai-activity';
import type { SessionRecord, TermOpenSpec } from '../term/types';

const spec: TermOpenSpec = {
  host: 'h',
  port: 22,
  user: 'u',
  auth: { type: 'agent' },
  jumpChain: [],
  encoding: 'utf-8',
};

const rec: SessionRecord = {
  id: 's1',
  name: 'web-01',
  host: 'example.com',
  port: 22,
  username: 'root',
  authType: 'password',
  kind: 'ssh',
  encoding: 'utf-8',
  mcpPerms: {},
  jumpChain: [],
  groupPath: '生产/Web',
  tags: [],
  createdAt: '2026-01-01',
  updatedAt: '2026-01-01',
};

const state = () => useAppStore.getState();

beforeEach(() => {
  useAppStore.setState({
    tabs: [],
    activeId: null,
    notices: [],
    pendingCloseTabs: null,
    pendingDeleteSession: null,
    closedTabs: [],
    settings: {},
    sessions: [],
    broadcastEnabled: false,
  });
  vi.mocked(invoke).mockReset();
});

describe('app-store tabs', () => {
  it('moveTab 把拖拽标签移到目标之前，顺序其余不变', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    const ids = state().tabs.map((t) => t.id);
    expect(ids).toHaveLength(3);

    // 第三个移到第一个之前
    s.moveTab(ids[2], ids[0]);
    const after = state().tabs.map((t) => t.id);
    expect(after).toEqual([ids[2], ids[0], ids[1]]);

    // 自移/未知 id 无副作用
    s.moveTab(ids[2], ids[2]);
    expect(state().tabs.map((t) => t.id)).toEqual(after);
    s.moveTab('nope', ids[0]);
    expect(state().tabs.map((t) => t.id)).toEqual(after);
  });
});

describe('关标签确认守卫（7.6）', () => {
  it('默认开启：有活跃连接时 closeTab 不直接关闭，而是进入待确认', () => {
    const s = state();
    s.connect(spec); // 新 pane 初始状态 connecting = 活跃
    const id = state().tabs[0].id;

    s.closeTab(id);
    expect(state().tabs).toHaveLength(1);
    expect(state().pendingCloseTabs).toEqual([id]);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('取消确认后标签保留', () => {
    const s = state();
    s.connect(spec);
    const id = state().tabs[0].id;

    s.closeTab(id);
    state().cancelCloseTab();
    expect(state().tabs).toHaveLength(1);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('设置关闭（terminal.confirmCloseTab=false）时直接关闭', () => {
    useAppStore.setState({ settings: { 'terminal.confirmCloseTab': false } });
    const s = state();
    s.connect(spec);
    s.closeTab(state().tabs[0].id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('全部 pane 已关闭时直接关闭（不弹确认）', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    s.setPaneState(tab.id, tab.activePaneId, 'closed');

    s.closeTab(tab.id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('closePane 关闭最后一叶也走确认守卫', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];

    s.closePane(tab.id, tab.activePaneId);
    expect(state().tabs).toHaveLength(1); // 守卫拦截，标签未关
    expect(state().pendingCloseTabs).toEqual([tab.id]);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
  });
});

describe('closePane 分屏关闭（7.3）', () => {
  it('关闭非末叶 pane：标签保留，活跃 pane 切换', () => {
    const s = state();
    s.connect(spec);
    s.splitActive('row');
    const tab = state().tabs[0];
    const [first, second] = Object.keys(tab.panes);
    expect(tab.activePaneId).toBe(second); // 分屏后新 pane 为活跃

    s.closePane(tab.id, second);
    const after = state().tabs[0];
    expect(Object.keys(after.panes)).toEqual([first]);
    expect(after.activePaneId).toBe(first);
    expect(state().pendingCloseTabs).toBeNull(); // 非末叶不触发关标签确认
  });
});

describe('多标签关闭（批次四 10.3）', () => {
  it('closeOtherTabs / closeTabsToRight / closeAllTabs 各自选中正确的标签集合', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    // 全部 pane 置为已关闭 → 不触发确认，直接关闭
    for (const t of state().tabs) s.setPaneState(t.id, t.activePaneId, 'closed');
    const [a, b] = state().tabs.map((t) => t.id);

    s.closeTabsToRight(b);
    expect(state().tabs.map((t) => t.id)).toEqual([a, b]);

    s.closeOtherTabs(a);
    expect(state().tabs.map((t) => t.id)).toEqual([a]);

    s.connect(spec);
    s.setPaneState(state().tabs[1].id, state().tabs[1].activePaneId, 'closed');
    s.closeAllTabs();
    expect(state().tabs).toHaveLength(0);
  });

  it('批量关闭含活跃连接：汇总一次确认，确认后全部关闭', () => {
    const s = state();
    s.connect(spec); // connecting = 活跃
    s.connect(spec);
    const closedTab = () => {
      s.connect(spec);
      const t = state().tabs[2];
      s.setPaneState(t.id, t.activePaneId, 'closed');
    };
    closedTab();

    s.closeAllTabs();
    expect(state().tabs).toHaveLength(3); // 守卫拦截
    expect(state().pendingCloseTabs).toHaveLength(3);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('批量关闭取消后一个都不关', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    const ids = state().tabs.map((t) => t.id);

    s.closeAllTabs();
    state().cancelCloseTab();
    expect(state().tabs.map((t) => t.id)).toEqual(ids);
  });
});

describe('删除服务器确认（7.1）', () => {
  it('request/cancel 管理待确认状态', () => {
    state().requestDeleteSession(rec);
    expect(state().pendingDeleteSession?.id).toBe('s1');
    state().cancelDeleteSession();
    expect(state().pendingDeleteSession).toBeNull();
  });

  it('确认删除成功 → success 通知', async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    state().requestDeleteSession(rec);
    await state().confirmDeleteSession();

    expect(invoke).toHaveBeenCalledWith('session_delete', { sessionId: 's1' });
    expect(state().pendingDeleteSession).toBeNull();
    const n = state().notices.at(-1);
    expect(n?.level).toBe('success');
    expect(n?.message).toContain('web-01');
  });

  it('确认删除失败 → error 通知（与成功不同级别）', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('db locked'));
    state().requestDeleteSession(rec);
    await state().confirmDeleteSession();

    const n = state().notices.at(-1);
    expect(n?.level).toBe('error');
    expect(n?.message).toContain('删除服务器失败');
  });
});

describe('通知分级生命周期（7.7）', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('默认 info 级，4s 后自动消失', () => {
    state().notify('hello');
    expect(state().notices).toHaveLength(1);
    expect(state().notices[0].level).toBe('info');

    vi.advanceTimersByTime(3999);
    expect(state().notices).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(state().notices).toHaveLength(0);
  });

  it('success 3s 消失；warning 8s 消失', () => {
    state().notify('ok', 'success');
    state().notify('warn', 'warning');
    vi.advanceTimersByTime(3000);
    expect(state().notices.map((n) => n.level)).toEqual(['warning']);
    vi.advanceTimersByTime(5000);
    expect(state().notices).toHaveLength(0);
  });

  it('error 常驻不自动消失，dismissNotice 手动关闭', () => {
    state().notify('boom', 'error');
    vi.advanceTimersByTime(60_000);
    expect(state().notices).toHaveLength(1);

    state().dismissNotice(state().notices[0].id);
    expect(state().notices).toHaveLength(0);
  });

  it('堆叠上限 5：溢出优先丢弃最旧的非 error', () => {
    state().notify('e1', 'error');
    for (let i = 0; i < 6; i++) state().notify(`i${i}`, 'info');
    const levels = state().notices.map((n) => n.level);
    expect(state().notices).toHaveLength(5);
    expect(levels[0]).toBe('error'); // error 不被挤出
    expect(state().notices[1].message).toBe('i2'); // 最旧的 info 先丢
  });
});
describe('批次六：通知动作 / bell / disconnectPane / 传输计数', () => {
  it('notify 可携带可序列化动作（不含函数引用）', () => {
    state().notify('已导出: /tmp/x.json', 'success', {
      label: '打开所在目录',
      actionId: 'open-in-explorer',
      arg: '/tmp/x.json',
    });
    const n = state().notices[0];
    expect(n.action).toEqual({
      label: '打开所在目录',
      actionId: 'open-in-explorer',
      arg: '/tmp/x.json',
    });
    // 可序列化契约：结构化克隆不丢字段
    expect(JSON.parse(JSON.stringify(n))).toEqual(n);
  });

  it('markBell 去重；setActive 清除该标签标记；关标签顺带清理', () => {
    const s = state();
    useAppStore.setState({ settings: { 'terminal.confirmCloseTab': false } });
    s.connect(spec);
    s.connect(spec);
    const [t1, t2] = state().tabs.map((t) => t.id);

    s.markBell(t2);
    s.markBell(t2);
    expect(state().bellTabs).toEqual([t2]);

    s.setActive(t2);
    expect(state().bellTabs).toEqual([]);

    s.markBell(t1);
    state().requestCloseTabs([t1]);
    expect(state().bellTabs).toEqual([]);
  });

  it('disconnectPane 只断活跃 pane 并置 closed；closed pane 幂等', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const pid = tab.activePaneId;
    s.setPaneState(tab.id, pid, 'connected');

    s.disconnectPane(tab.id, pid);
    expect(state().tabs[0].panes[pid].state).toBe('closed');
    // 已 closed 再调用不抛不错位
    s.disconnectPane(tab.id, pid);
    expect(state().tabs[0].panes[pid].state).toBe('closed');
  });

  it('setTransferActive 设置与清空', () => {
    expect(state().transferActive).toBeNull();
    state().setTransferActive(3);
    expect(state().transferActive).toBe(3);
    state().setTransferActive(null);
    expect(state().transferActive).toBeNull();
  });
});
describe('分屏保活与同服务器新 pane（UX-6）', () => {
  it('splitActive 不关闭旧 pane 会话，旧 pane 保留且新 pane 复用同一 target', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const oldPane = tab.panes[tab.activePaneId];
    const closeSpy = vi.spyOn(oldPane.session, 'close');

    s.splitActive('row');
    const after = state().tabs[0];
    expect(closeSpy).not.toHaveBeenCalled();
    expect(after.panes[oldPane.id]).toBeDefined(); // 旧 pane 数据仍在
    expect(after.target).toEqual({ kind: 'spec', spec }); // 新 pane 经 tab.target 连同服务器
    const newPane = after.panes[after.activePaneId];
    expect(newPane.id).not.toBe(oldPane.id);
  });

  it('连接成功（首连与重连）均不向远程 shell 写入任何字节（注入已移除）', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const p = tab.panes[tab.activePaneId];
    const writeSpy = vi.spyOn(p.session, 'write').mockImplementation(() => undefined);

    p.session.onEvent({ v: 1, type: 'session_state', tabId: 'x', state: 'connected' });
    expect(writeSpy).not.toHaveBeenCalled();
    p.session.onEvent({
      v: 1,
      type: 'session_state',
      tabId: 'x',
      state: 'connected',
      reconnected: true,
    });
    expect(writeSpy).not.toHaveBeenCalled();
  });
});

describe('广播输入（UX-11）', () => {
  it('开启后扇出到同窗口其它 connected pane；发起 pane 与未连接 pane 不写；关闭后恢复', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    const [t1, t2, t3] = state().tabs;
    const p1 = t1.panes[t1.activePaneId];
    const p2 = t2.panes[t2.activePaneId];
    const p3 = t3.panes[t3.activePaneId];
    s.setPaneState(t1.id, p1.id, 'connected');
    s.setPaneState(t2.id, p2.id, 'connected');
    s.setPaneState(t3.id, p3.id, 'closed'); // 未连接不收广播
    const w1 = vi.spyOn(p1.session, 'write').mockImplementation(() => undefined);
    const w2 = vi.spyOn(p2.session, 'write').mockImplementation(() => undefined);
    const w3 = vi.spyOn(p3.session, 'write').mockImplementation(() => undefined);
    vi.mocked(emit).mockClear();

    s.toggleBroadcast();
    expect(state().broadcastEnabled).toBe(true);
    p1.session.inputHook?.('ls\n');
    expect(w1).not.toHaveBeenCalled(); // 发起 pane 由自身 onData 直发，不经广播
    expect(w2).toHaveBeenCalledWith('ls\n');
    expect(w3).not.toHaveBeenCalled();
    // 跨窗口事件帧带本窗口 label 作回环防护
    expect(vi.mocked(emit)).toHaveBeenCalledWith(BROADCAST_INPUT_EVENT, {
      v: 1,
      source: 'main',
      data: 'ls\n',
    });

    // 关闭开关即恢复单 pane 输入
    s.toggleBroadcast();
    w2.mockClear();
    vi.mocked(emit).mockClear();
    p1.session.inputHook?.('pwd\n');
    expect(w2).not.toHaveBeenCalled();
    expect(vi.mocked(emit)).not.toHaveBeenCalled();
  });

  it('跨窗口接收：写入本窗口 connected pane；本窗口来源帧被回环防护丢弃', async () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const p = tab.panes[tab.activePaneId];
    s.setPaneState(tab.id, p.id, 'connected');
    const w = vi.spyOn(p.session, 'write').mockImplementation(() => undefined);

    await initBroadcastReceiver();
    const handler = vi.mocked(listen).mock.calls[0][1] as (e: {
      payload: { v: 1; source: string; data: string };
    }) => void;
    expect(vi.mocked(listen).mock.calls[0][0]).toBe(BROADCAST_INPUT_EVENT);

    handler({ payload: { v: 1, source: 'det-s1', data: 'top\n' } });
    expect(w).toHaveBeenCalledWith('top\n');

    w.mockClear();
    handler({ payload: { v: 1, source: 'main', data: 'echo x\n' } }); // 本窗口回环
    expect(w).not.toHaveBeenCalled();
  });
});

describe('重开已关闭标签（批次十一）', () => {
  beforeEach(() => {
    // connectBySession → recordRecent → setSetting 会 invoke('settings_set')，需可 thenable
    vi.mocked(invoke).mockResolvedValue([]);
  });

  /** 直接关闭（跳过确认守卫：先把 pane 置 closed） */
  const closeDirect = (id: string) => {
    const t = state().tabs.find((x) => x.id === id);
    if (t) state().setPaneState(t.id, t.activePaneId, 'closed');
    state().closeTab(id);
  };

  it('session 标签关闭后压栈，reopenClosedTab 弹栈按原档案/标题重连', () => {
    const s = state();
    s.connectBySession('s1', 'web-01');
    closeDirect(state().tabs[0].id);
    expect(state().tabs).toHaveLength(0);
    expect(state().closedTabs).toEqual([{ sessionId: 's1', title: 'web-01' }]);

    state().reopenClosedTab();
    expect(state().closedTabs).toEqual([]);
    expect(state().tabs).toHaveLength(1);
    const t = state().tabs[0];
    expect(t.title).toBe('web-01');
    expect(t.target).toEqual({
      kind: 'session',
      sessionId: 's1',
      sessionKind: 'ssh',
      encoding: null,
    });
  });

  it('快速连接（spec）标签不入栈；空栈 reopen 无操作', () => {
    const s = state();
    s.connect(spec);
    closeDirect(state().tabs[0].id);
    expect(state().closedTabs).toEqual([]);

    state().reopenClosedTab();
    expect(state().tabs).toHaveLength(0);
  });

  it('栈上限 10：溢出丢弃最旧；后关的先重开', () => {
    const s = state();
    for (let i = 1; i <= 11; i++) {
      s.connectBySession(`s${i}`, `host-${i}`);
      closeDirect(state().tabs[state().tabs.length - 1].id);
    }
    expect(state().closedTabs).toHaveLength(10);
    expect(state().closedTabs[0].sessionId).toBe('s2'); // s1 被挤出

    state().reopenClosedTab();
    expect(state().tabs[0].title).toBe('host-11'); // 最近关闭的先重开
    expect(state().closedTabs).toHaveLength(9);
  });
});

describe('MCP agent 终端标签', () => {
  type Frame = Record<string, unknown>;
  let push: (f: Frame) => void = () => undefined;

  beforeEach(() => {
    vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === 'mcp_ui_subscribe') {
        const ch = (args as { events: { onmessage: (f: Frame) => void } }).events;
        push = (f) => ch.onmessage(f);
      }
      return Promise.resolve(undefined);
    });
    // 订阅只建一次（模块级标记）：首个用例建立后 push 持续可用
    state().subscribeMcpUi();
    useAppStore.setState({ aiTabs: {} });
  });

  const readyCalls = () =>
    vi.mocked(invoke).mock.calls.filter(([c]) => c === 'mcp_agent_tab_ready');

  it('agent 请求开标签：不抢已有活跃标签，记录请求 id 供首次 attach 回报', () => {
    state().connect(spec);
    const active = state().activeId;
    useAppStore.setState({ sessions: [rec] });
    push({ type: 'agent_terminal_open', requestId: 'r1', sessionId: 's1', sessionName: 'web-01' });
    const tabs = state().tabs;
    expect(tabs).toHaveLength(2);
    expect(state().activeId).toBe(active);
    expect(tabs[1].title).toBe('AI · web-01');
    expect(tabs[1].target).toMatchObject({ kind: 'session', sessionId: 's1' });
    expect(takeAgentRequest(tabs[1].activePaneId)).toBe('r1');
    // 只回报一次
    expect(takeAgentRequest(tabs[1].activePaneId)).toBeUndefined();
  });

  it('新审批请求展开已收起的授权框；ai_activity 提示帧触发取活动快照', async () => {
    useAiActivity.setState({ approvalHidden: true });
    push({
      type: 'approval_request',
      id: 'ap1',
      sessionId: 's1',
      command: 'rm x',
      expiresInMs: 1000,
    });
    expect(useAiActivity.getState().approvalHidden).toBe(false);
    expect(state().pendingApprovals.map((a) => a.id)).toEqual(['ap1']);
    vi.mocked(invoke).mockClear();
    push({ type: 'ai_activity' });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('mcp_activity'));
    useAppStore.setState({ pendingApprovals: [] });
  });

  it('「自动执行」：记住过的命令到达即倒计时；取消勾选停止并忘掉规则；勾选后批准带 always', () => {
    const t0 = Date.now();
    push({
      type: 'approval_request',
      id: 'ap1',
      sessionId: 's1',
      command: 'df -h',
      expiresInMs: 60000,
      remembered: true,
    });
    push({
      type: 'approval_request',
      id: 'ap2',
      sessionId: 's1',
      command: 'ls',
      expiresInMs: 60000,
    });
    const [a1, a2] = state().pendingApprovals;
    expect(a1.autoAt).toBeGreaterThanOrEqual(t0 + APPROVAL_AUTO_RUN_MS);
    expect(a2.autoAt).toBeUndefined();

    vi.mocked(invoke).mockClear();
    state().setApprovalAuto('ap1', false);
    expect(invoke).toHaveBeenCalledWith('mcp_forget_always', { id: 'ap1' });
    expect(state().pendingApprovals[0]).toMatchObject({ autoAt: undefined, remembered: false });
    // 未记住过的项取消勾选不必通知后端
    vi.mocked(invoke).mockClear();
    state().setApprovalAuto('ap2', true);
    state().setApprovalAuto('ap2', false);
    state().setApprovalAuto('ap2', true);
    expect(invoke).not.toHaveBeenCalled();
    expect(state().pendingApprovals[1].autoAt).toBeGreaterThan(0);

    state().answerApproval('ap2', true, true);
    expect(invoke).toHaveBeenCalledWith('mcp_approve', { id: 'ap2', allow: true, always: true });
    expect(state().pendingApprovals.map((a) => a.id)).toEqual(['ap1']);
    useAppStore.setState({ pendingApprovals: [] });
  });

  it('会话不存在或为本地会话：立即回报失败，不开标签', () => {
    useAppStore.setState({ sessions: [{ ...rec, id: 'loc', kind: 'local' }] });
    push({ type: 'agent_terminal_open', requestId: 'r2', sessionId: 'missing' });
    push({ type: 'agent_terminal_open', requestId: 'r3', sessionId: 'loc' });
    expect(state().tabs).toHaveLength(0);
    const calls = readyCalls();
    expect(calls.map(([, a]) => (a as { requestId: string }).requestId)).toEqual(['r2', 'r3']);
    for (const [, a] of calls) expect((a as { error: unknown }).error).toBeTruthy();
  });

  it('开链前用户关掉标签：回报失败，后端不必干等超时', () => {
    useAppStore.setState({ sessions: [rec] });
    push({ type: 'agent_terminal_open', requestId: 'r4', sessionId: 's1' });
    const t = state().tabs[0];
    state().setPaneState(t.id, t.activePaneId, 'closed');
    state().closeTab(t.id);
    const calls = readyCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ requestId: 'r4', tabId: null });
    expect((calls[0][1] as { error: unknown }).error).toBeTruthy();
  });

  describe('AI 命令镜像：会话标签内的只读分屏', () => {
    const start = (execId: string, command: string) =>
      push({
        type: 'exec_start',
        execId,
        sessionId: 's1',
        sessionName: 'web-01',
        tool: 'ssh_exec',
        command,
      });
    const mirrorPanes = () =>
      state().tabs.flatMap((t) => Object.values(t.panes).filter((p) => p.mirror));
    // eslint-disable-next-line no-control-regex
    const plain = (s: string) => s.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, '');

    it('会话标签已打开：不另开标签，镜像挂在该标签下方，输出按序汇入', () => {
      useAppStore.setState({ sessions: [rec] });
      state().connectOrActivate('s1', 'web-01');
      const tab = state().tabs[0];
      const termPane = tab.activePaneId;
      state().connect(spec); // 另一个标签处于活动状态
      start('e1', 'uptime');
      push({
        type: 'exec_output',
        execId: 'e1',
        sessionId: 's1',
        stream: 'stdout',
        data: 'up 3 days\n',
      });
      push({
        type: 'exec_end',
        execId: 'e1',
        sessionId: 's1',
        status: 'ok',
        exitCode: 0,
        durationMs: 5,
        error: null,
        omittedBytes: 0,
      });
      start('e2', 'df -h');

      expect(state().tabs).toHaveLength(2);
      const t = state().tabs[0];
      expect(state().activeId).toBe(t.id); // 切到会话标签
      expect(t.activePaneId).toBe(termPane); // 不抢终端 pane 焦点
      const mirrors = Object.values(t.panes).filter((p) => p.mirror);
      expect(mirrors).toHaveLength(1); // 同会话多条命令共用一个镜像
      expect(mirrors[0].state).toBe('closed');
      expect(t.layout).toMatchObject({
        kind: 'split',
        dir: 'col',
        a: { kind: 'leaf', paneId: termPane },
        b: { kind: 'leaf', paneId: mirrors[0].id },
      });

      const got: string[] = [];
      feedOf(mirrors[0].id).attach((s) => got.push(s));
      expect(plain(got.join(''))).toMatch(
        /\$ uptime\r\nup 3 days\r\n✓ exit 0 · 5 ms\r\n\r\n.*\$ df -h/s,
      );
    });

    it('会话未打开：自动打开会话终端标签并挂镜像；关闭标签镜像随之关闭', () => {
      useAppStore.setState({ sessions: [rec] });
      start('e1', 'id');
      expect(state().tabs).toHaveLength(1);
      const t = state().tabs[0];
      expect(t.target).toMatchObject({ kind: 'session', sessionId: 's1' });
      expect(t.title).toBe('web-01');
      const panes = Object.values(t.panes);
      expect(panes.filter((p) => !p.mirror)).toHaveLength(1); // 真实终端
      expect(panes.filter((p) => p.mirror)).toHaveLength(1);
      expect(t.panes[t.activePaneId].mirror).toBeFalsy();

      state().setPaneState(t.id, t.activePaneId, 'closed');
      state().closeTab(t.id);
      expect(state().tabs).toHaveLength(0);
      // 后续输出丢弃，不重开标签
      push({ type: 'exec_output', execId: 'e1', sessionId: 's1', stream: 'stdout', data: 'x' });
      expect(state().tabs).toHaveLength(0);
      // 重开已关闭标签恢复为普通终端，不带镜像
      state().reopenClosedTab();
      expect(mirrorPanes()).toHaveLength(0);
    });

    it('单独关闭镜像 pane 保留终端；关闭终端只剩镜像则整标签关闭', () => {
      useAppStore.setState({ sessions: [rec] });
      start('e1', 'id');
      const t = state().tabs[0];
      const termPane = t.activePaneId;
      const mirror = mirrorPanes()[0].id;

      state().closePane(t.id, mirror);
      expect(state().tabs).toHaveLength(1);
      expect(Object.keys(state().tabs[0].panes)).toEqual([termPane]);
      expect(state().tabs[0].layout).toEqual({ kind: 'leaf', paneId: termPane });

      // 下一条命令重新挂镜像（同一标签）
      start('e2', 'ls');
      expect(state().tabs).toHaveLength(1);
      expect(mirrorPanes()).toHaveLength(1);

      state().setPaneState(t.id, termPane, 'closed');
      state().closePane(t.id, termPane);
      expect(state().tabs).toHaveLength(0);
    });
  });
  it('attached/closed 帧维护 AI 控制标记', () => {
    push({ type: 'agent_terminal_attached', tabId: 'bt1', terminalId: 'x', kind: 'agent' });
    push({ type: 'agent_terminal_attached', tabId: 'bt2', terminalId: 'y', kind: 'shared' });
    expect(state().aiTabs).toEqual({ bt1: 'agent', bt2: 'shared' });
    push({ type: 'agent_terminal_closed', tabId: 'bt1' });
    expect(state().aiTabs).toEqual({ bt2: 'shared' });
  });

  it('AI terminal_close：AI 开的标签直接关闭（不弹确认），共享标签保留并提示', () => {
    useAppStore.setState({ sessions: [rec] });
    state().connectOrActivate('s1', 'web-01');
    state().connect(spec);
    const [aiTab, userTab] = state().tabs;
    aiTab.panes[aiTab.activePaneId].session.tabId = 'bt-ai';
    userTab.panes[userTab.activePaneId].session.tabId = 'bt-user';
    state().setPaneState(aiTab.id, aiTab.activePaneId, 'connected');
    push({ type: 'agent_terminal_attached', tabId: 'bt-ai', terminalId: 'x', kind: 'agent' });
    push({ type: 'agent_terminal_attached', tabId: 'bt-user', terminalId: 'y', kind: 'shared' });

    push({ type: 'agent_terminal_closed', tabId: 'bt-user', closeTab: false });
    expect(state().tabs.map((t) => t.id)).toEqual([aiTab.id, userTab.id]);
    expect(state().aiTabs).toEqual({ 'bt-ai': 'agent' });
    expect(state().notices.some((n) => n.message.includes('标签保留'))).toBe(true);

    push({ type: 'agent_terminal_closed', tabId: 'bt-ai', closeTab: true });
    expect(state().pendingCloseTabs).toBeNull();
    expect(state().tabs.map((t) => t.id)).toEqual([userTab.id]);
    expect(state().aiTabs).toEqual({});
    expect(state().notices.some((n) => n.message.includes('AI 已关闭终端'))).toBe(true);
  });

  it('MCP 服务停止清场（无 closeTab）：只摘标记，不关标签', () => {
    state().connect(spec);
    const t = state().tabs[0];
    t.panes[t.activePaneId].session.tabId = 'bt-x';
    push({ type: 'agent_terminal_attached', tabId: 'bt-x', terminalId: 'z', kind: 'agent' });
    push({ type: 'agent_terminal_closed', tabId: 'bt-x' });
    expect(state().tabs).toHaveLength(1);
    expect(state().aiTabs).toEqual({});
  });

  describe('AI 客户端断开：退出 AI 托管', () => {
    const counts = { approvals: 0, execs: 0, jobs: 0, terminals: 0, mirrors: 0 };

    it('清理时被拒绝的待确认项关掉授权框，别的保留', () => {
      for (const id of ['ap1', 'ap2'])
        push({ type: 'approval_request', id, sessionId: 's1', command: 'rm x', expiresInMs: 1000 });
      push({ type: 'approval_closed', id: 'ap1' });
      expect(state().pendingApprovals.map((a) => a.id)).toEqual(['ap2']);
      useAppStore.setState({ pendingApprovals: [] });
    });

    it('mirror_close 关闭该会话的镜像分屏，终端保留', () => {
      useAppStore.setState({ sessions: [rec] });
      push({
        type: 'exec_start',
        execId: 'e1',
        sessionId: 's1',
        sessionName: 'web-01',
        tool: 'ssh_exec',
        command: 'id',
      });
      const termPane = state().tabs[0].activePaneId;
      push({ type: 'mirror_close', sessionId: 's1' });
      expect(state().tabs).toHaveLength(1);
      expect(Object.keys(state().tabs[0].panes)).toEqual([termPane]);
    });

    it('断开通知列出清理内容；没有可清理的不打扰', () => {
      push({
        type: 'ai_client_released',
        clientId: 'c1',
        name: 'claude-code',
        reason: 'disconnected',
        ...counts,
        approvals: 2,
        terminals: 1,
      });
      const n = state().notices.at(-1);
      expect(n?.message).toBe(
        '「claude-code」已断开，已自动退出 AI 托管并清理：2 项待确认命令、1 个 AI 终端（标签保留）',
      );
      const before = state().notices.length;
      push({ type: 'ai_client_released', clientId: 'c2', name: '', reason: 'ended', ...counts });
      expect(state().notices).toHaveLength(before);
    });

    it('空闲确认：入队、作答回报后端并出队；客户端又有动作则作废', () => {
      const idle = (clientId: string) =>
        push({
          type: 'ai_client_idle',
          clientId,
          name: 'codex',
          idleMinutes: 30,
          ...counts,
          jobs: 1,
        });
      idle('c1');
      idle('c1'); // 同一客户端不重复入队
      idle('c2');
      expect(state().aiIdlePrompts.map((p) => p.clientId)).toEqual(['c1', 'c2']);
      expect(state().aiIdlePrompts[0]).not.toHaveProperty('type');
      state().answerAiIdle('c1', true);
      expect(invoke).toHaveBeenCalledWith('mcp_client_idle_answer', {
        clientId: 'c1',
        releaseIt: true,
      });
      push({ type: 'ai_client_idle_closed', clientId: 'c2' });
      expect(state().aiIdlePrompts).toEqual([]);
      // 已清理（如用户确认后）的客户端：残留确认框一并移除
      idle('c3');
      push({
        type: 'ai_client_released',
        clientId: 'c3',
        name: 'codex',
        reason: 'idle',
        ...counts,
      });
      expect(state().aiIdlePrompts).toEqual([]);
    });
  });

  it('共享失败（权限未放行）只提示错误，不打标记', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'mcp_share_tab' ? Promise.reject(new Error('denied')) : Promise.resolve(undefined),
    );
    await state().shareTabWithAi('bt9');
    expect(state().aiTabs).toEqual({});
    expect(state().notices.some((n) => n.level === 'error')).toBe(true);
  });

  it('录像：开始成功打标记，停止摘标记；开始失败不打标记', async () => {
    useAppStore.setState({ recTabs: {} });
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === 'term_record_stop' ? Promise.resolve({ title: 'web' }) : Promise.resolve('rid'),
    );
    await state().startRecording('bt1', 'web');
    expect(state().recTabs).toEqual({ bt1: true });
    await state().stopRecording('bt1');
    expect(state().recTabs).toEqual({});
    vi.mocked(invoke).mockImplementation(() => Promise.reject(new Error('gone')));
    await state().startRecording('bt2', 'x');
    expect(state().recTabs).toEqual({});
  });
});
