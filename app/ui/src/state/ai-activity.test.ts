import { describe, expect, it } from 'vitest';
import {
  activeSessionIds,
  fmtElapsed,
  groupApprovals,
  aiStatusBySession,
  type AiActivitySnapshot,
} from './ai-activity';
import type { McpApprovalRequest } from './app-store';

const ap = (id: string, sessionId: string): McpApprovalRequest => ({
  type: 'approval_request',
  id,
  sessionId,
  command: `cmd-${id}`,
  expiresInMs: 300_000,
  receivedAt: 0,
});

const activity: AiActivitySnapshot = {
  execs: [
    { execId: 'e1', sessionId: 'web', tool: 'ssh_exec', command: 'ls', startedAt: 1 },
    { execId: 'e2', sessionId: 'db', tool: 'ssh_exec', command: 'df', startedAt: 2 },
  ],
  jobs: [{ jobId: 'j1', sessionId: 'web', command: 'tail -f x', startedAt: 3, status: 'running' }],
  terminals: [
    { terminalId: 't1', sessionId: 'web', kind: 'agent', tabId: 'b1' },
    { terminalId: 't2', sessionId: null, kind: 'shared', tabId: 'b2' },
  ],
};

describe('ai-activity', () => {
  it('按会话统计执行中与待确认：空闲的 AI 终端不算执行中', () => {
    const map = aiStatusBySession(activity, [ap('a1', 'db'), ap('a2', 'db'), ap('a3', 'web')]);
    expect(map.get('web')).toEqual({ running: 2, pending: 1 });
    expect(map.get('db')).toEqual({ running: 1, pending: 2 });
    expect(map.has('other')).toBe(false);
  });

  it('待确认按会话分组，保持到达顺序', () => {
    const groups = groupApprovals([ap('a1', 'db'), ap('a2', 'web'), ap('a3', 'db')]);
    expect(groups.map((g) => g.sessionId)).toEqual(['db', 'web']);
    expect(groups[0].items.map((a) => a.id)).toEqual(['a1', 'a3']);
  });

  it('活动面板的会话：待确认在前、去重、跳过无档案的临时连接', () => {
    expect(activeSessionIds(activity, [ap('a1', 'db')])).toEqual(['db', 'web']);
  });

  it('时长格式', () => {
    expect(fmtElapsed(5_400)).toBe('5s');
    expect(fmtElapsed(125_000)).toBe('2m05s');
    expect(fmtElapsed(3_900_000)).toBe('1h05m');
    expect(fmtElapsed(-1)).toBe('0s');
  });
});
