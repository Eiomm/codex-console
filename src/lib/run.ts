import type { EdgeId, NodeId, Who } from './flow';

export const TOKENS_PER_PACKET = 2000;
const MAX_ROWS = 500;
const MAX_FILE_DIFF = 256 * 1024;

export interface Usage { input?: number; cachedInput?: number; cacheWrite?: number; output?: number; reasoning?: number; total?: number; fresh?: number }
export interface Window { name: string; usedPercent: number; resetsAt?: number | null }
export interface ModelUsage { input: number; output: number; cacheRead: number; cacheCreation: number; costUsd: number }
interface Base { tsMs: number; agent: 'claude' | 'codex'; sourceId: string }
export type RunEvent = Base & (
  | { kind: 'session'; threadId?: string; model?: string | null; effort?: string | null; serviceTier?: string | null; sandbox?: string | null; cwd?: string | null; originator?: string | null }
  | { kind: 'handoff'; mode: 'fresh' | 'resume'; description: string }
  | { kind: 'message'; role: string; phase?: string | null; text: string; truncated: boolean }
  | { kind: 'command'; command: string; exitCode?: number | null; durationMs?: number | null; outputTail: string; status: string; truncated: boolean }
  | { kind: 'tool'; toolUseId: string; name: string; summary: string; isError: boolean; truncated: boolean }
  | { kind: 'toolResult'; toolUseId: string; text: string; isError: boolean; truncated: boolean }
  | { kind: 'fileChange'; itemId: string; path: string; changeType: string; movePath?: string | null; diff: string; added: number; removed: number; truncated: boolean }
  | ({ kind: 'tokens'; delta: number; provisional: boolean; model?: string | null; scope: 'main' | 'sub'; messageId?: string | null } & Usage)
  | { kind: 'rateLimit'; source: string; windows: Window[] }
  | { kind: 'turnCompleted'; durationMs?: number | null; lastMessage: string; truncated: boolean }
  | { kind: 'verdict'; outcome: 'start' | 'pass' | 'fix' | 'fail' }
  | { kind: 'runResult'; status: string; costUsd: number; durationMs: number; numTurns: number; perModel: Record<string, ModelUsage> }
);

export type RowKind = '读取' | '命令' | '改文件' | '问题' | '发言' | '记录';
export interface Row { id: number; tsMs: number; kind: RowKind; action: string; result?: string; detail?: string; isDiff?: boolean }
export interface FileRow { path: string; added: number; removed: number; diff: string; truncated: boolean }
export interface AgentTotals { fresh: number; total: number; provisional: boolean }
export interface Packet { edge: EdgeId; who: Who; count: number }

export interface RunState {
  rows: Record<NodeId, Row[]>;
  status: Partial<Record<NodeId, string>>;
  fresh: Partial<Record<NodeId, number>>;
  active: NodeId | null;
  lastTs: number;
  now: string;
  nowWho: Who;
  round: number;
  gate: boolean;
  failed: boolean;
  files: FileRow[];
  tasks: string[];
  commands: number;
  issues: number;
  agents: { claude: AgentTotals; codex: AgentTotals };
  limits: Record<string, Window[]>;
  fast: boolean;
  costUsd: number | null;
  sandboxes: Record<string, string>;
  codexSeen: Record<string, { fresh: number; total: number }>;
  claudeSeen: Record<string, true>;
  claudeSources: Record<string, { fresh: number; total: number; final: boolean }>;
  turnDone: boolean;
  verdicting: boolean;
  lastVerdict: 'pass' | 'fix' | 'fail' | null;
  nextId: number;
}

const emptyRows = (): Record<NodeId, Row[]> => ({ plan: [], impl: [], rev: [], re: [], verdict: [], gate: [] });
export function emptyRun(): RunState {
  return {
    rows: emptyRows(), status: {}, fresh: {}, active: null, lastTs: 0, now: '', nowWho: 'codex', round: 0, gate: false, failed: false,
    files: [], tasks: [], commands: 0, issues: 0,
    agents: { claude: { fresh: 0, total: 0, provisional: false }, codex: { fresh: 0, total: 0, provisional: false } },
    limits: {}, fast: false, costUsd: null, sandboxes: {}, codexSeen: {}, claudeSeen: {}, claudeSources: {}, turnDone: false, verdicting: false, lastVerdict: null, nextId: 1,
  };
}

/** Which module an event belongs to. Codex never plans: without a read-only sandbox it is implementing. */
export function nodeFor(run: RunState, e: RunEvent): NodeId {
  if (e.agent === 'codex') return run.sandboxes[e.sourceId] === 'read-only' ? 're' : 'impl';
  if (run.gate || e.kind === 'runResult') return 'gate';
  if (e.kind === 'handoff') return 'plan';
  if (e.kind === 'verdict' || run.verdicting) return 'verdict';
  if (run.turnDone) return 'rev';
  return run.round > 0 ? 'impl' : 'plan';
}

export function edgeInto(node: NodeId, round: number): EdgeId | null {
  switch (node) {
    case 'impl': return round > 1 ? 'back' : 'task';
    case 'rev': return 'toRev';
    case 're': return 'toRe';
    case 'verdict': return 'revOut';
    case 'gate': return 'pass';
    default: return null;
  }
}

function firstLine(text: string): string {
  const line = text.trim().split('\n').find(l => l.trim()) ?? '';
  return line.trim();
}
function toolKind(name: string): RowKind {
  if (['Read', 'Grep', 'Glob', 'WebFetch', 'WebSearch'].includes(name)) return '读取';
  if (['Edit', 'Write', 'NotebookEdit'].includes(name)) return '改文件';
  if (name === 'Bash') return '命令';
  return '记录';
}
const AGENT_NAME: Record<'claude' | 'codex', string> = { claude: 'Claude', codex: 'Codex' };

function claudeTotals(sources: RunState['claudeSources']): AgentTotals {
  let fresh = 0, total = 0, provisional = false;
  for (const s of Object.values(sources)) { fresh += s.fresh; total += s.total; provisional = provisional || !s.final; }
  return { fresh, total, provisional };
}
function insertRow(list: Row[], row: Row): Row[] {
  let at = list.length;
  while (at > 0 && list[at - 1].tsMs > row.tsMs) at -= 1;
  const next = [...list.slice(0, at), row, ...list.slice(at)];
  return next.length > MAX_ROWS ? next.slice(-MAX_ROWS) : next;
}
function joinDiff(a: string, b: string): { diff: string; cut: boolean } {
  const joined = `${a}\n${b}`;
  return joined.length > MAX_FILE_DIFF ? { diff: joined.slice(-MAX_FILE_DIFF), cut: true } : { diff: joined, cut: false };
}

/** Pure reducer: returns the next state plus any packets to animate. */
export function applyEvent(prev: RunState, e: RunEvent): { run: RunState; packet: Packet | null } {
  const run: RunState = { ...prev, lastTs: Math.max(prev.lastTs, e.tsMs) };
  // A late historical event still enters the record, but it must not replace newer live status.
  const stale = e.tsMs < prev.lastTs;
  let packet: Packet | null = null;
  const name = AGENT_NAME[e.agent];
  if (e.kind === 'session') {
    if (e.sandbox) run.sandboxes = { ...run.sandboxes, [e.sourceId]: e.sandbox };
    if (e.serviceTier === 'priority' || e.serviceTier === 'fast') run.fast = true;
    return { run, packet };
  }
  if (e.kind === 'rateLimit') {
    run.limits = { ...run.limits, [e.source]: e.windows };
    return { run, packet };
  }
  const node = nodeFor(run, e);
  // While a task is with Codex, Claude only runs a forwarder that relays it. That plumbing is not
  // Codex's work, so it must not appear in the implementation record.
  if (e.agent === 'claude' && node === 'impl' && (e.kind === 'tool' || e.kind === 'toolResult' || e.kind === 'message')) return { run, packet };
  const add = (kind: RowKind, action: string, result?: string, detail?: string, isDiff?: boolean) => {
    run.rows = { ...run.rows, [node]: insertRow(run.rows[node], { id: run.nextId, tsMs: e.tsMs, kind, action, result, detail, isDiff }) };
    run.nextId += 1;
  };
  const say = (text: string, status: string) => {
    if (stale) return;
    run.now = text; run.nowWho = e.agent; run.active = node;
    run.status = { ...run.status, [node]: status };
  };
  switch (e.kind) {
    case 'handoff':
      run.round = run.round === 0 ? 1 : e.mode === 'resume' ? run.round + 1 : run.round;
      run.turnDone = false; run.verdicting = false;
      add('记录', e.mode === 'resume' ? '把修复清单交回 Codex' : '把任务书交给 Codex', e.description);
      if (!stale) {
        run.now = e.mode === 'resume' ? `Claude 把修复清单交回 Codex（第 ${run.round} 轮）` : 'Claude 把任务书交给 Codex';
        run.nowWho = 'claude'; run.active = 'impl';
        run.status = { ...run.status, plan: '已交付', impl: '接收任务' };
      }
      break;
    case 'message': {
      const line = firstLine(e.text);
      if (!line) break;
      if (e.role === 'user') {
        if (e.agent === 'codex') run.tasks = [...run.tasks, e.text];
        add('记录', '收到任务', line, e.text);
        say(`${name} 收到任务：${line}`, '收到任务');
      } else {
        add('发言', line, undefined, e.text);
        say(`${name}：${line}`, '发言');
      }
      break;
    }
    case 'command': {
      const line = firstLine(e.command);
      const failed = typeof e.exitCode === 'number' && e.exitCode !== 0;
      run.commands += 1;
      add('命令', line, failed ? `退出码 ${e.exitCode}` : e.status === 'completed' || e.exitCode === 0 ? '完成' : e.status, e.outputTail);
      say(`${name} 刚运行了 ${line}`, failed ? `退出码 ${e.exitCode}` : '运行命令');
      break;
    }
    case 'tool': {
      const line = firstLine(e.summary) || e.name;
      add(toolKind(e.name), e.name === 'Bash' ? line : `${e.name} ${line}`);
      say(`${name} 刚调用 ${e.name}：${line}`, `调用 ${e.name}`);
      break;
    }
    case 'toolResult':
      if (e.isError) { run.issues += 1; add('问题', '工具调用失败', firstLine(e.text), e.text); }
      break;
    case 'fileChange': {
      const i = run.files.findIndex(f => f.path === e.path);
      let merged: FileRow;
      if (i < 0) merged = { path: e.path, added: e.added, removed: e.removed, diff: e.diff, truncated: e.truncated };
      else {
        const { diff, cut } = joinDiff(run.files[i].diff, e.diff);
        merged = { ...run.files[i], added: run.files[i].added + e.added, removed: run.files[i].removed + e.removed, diff, truncated: run.files[i].truncated || e.truncated || cut };
      }
      run.files = i < 0 ? [...run.files, merged] : run.files.map((f, n) => n === i ? merged : f);
      add('改文件', e.path, `+${e.added} −${e.removed}`, e.diff, true);
      say(`${name} 刚修改了 ${e.path}`, `改 ${e.path.split('/').pop()}`);
      break;
    }
    case 'tokens': {
      const fresh = e.fresh ?? 0, total = e.total ?? 0;
      let freshDelta: number;
      if (e.agent === 'codex') {
        const seen = run.codexSeen[e.sourceId] ?? { fresh: 0, total: 0 };
        if (total <= seen.total) break;
        freshDelta = Math.max(0, fresh - seen.fresh);
        run.codexSeen = { ...run.codexSeen, [e.sourceId]: { fresh, total } };
        const a = run.agents.codex;
        run.agents = { ...run.agents, codex: { fresh: a.fresh + freshDelta, total: a.total + total - seen.total, provisional: a.provisional || e.provisional } };
      } else {
        const key = `${e.sourceId}:${e.messageId ?? run.nextId}`;
        if (run.claudeSeen[key]) break;
        run.claudeSeen = { ...run.claudeSeen, [key]: true };
        freshDelta = fresh;
        // The final result is authoritative for its session: later stream usage must not add to it.
        const src = run.claudeSources[e.sourceId] ?? { fresh: 0, total: 0, final: false };
        if (!src.final) {
          run.claudeSources = { ...run.claudeSources, [e.sourceId]: { ...src, fresh: src.fresh + fresh, total: src.total + total } };
          run.agents = { ...run.agents, claude: claudeTotals(run.claudeSources) };
        }
      }
      // The Claude forwarder that relays a task to Codex counts toward Claude's total, but it must
      // not inflate a module labelled Codex, and it is not Codex doing the work.
      if (e.agent === 'claude' && node === 'impl') break;
      run.fresh = { ...run.fresh, [node]: (run.fresh[node] ?? 0) + freshDelta };
      const edge = edgeInto(node, run.round);
      if (edge && freshDelta > 0 && !stale) packet = { edge, who: e.agent, count: Math.max(1, Math.round(freshDelta / TOKENS_PER_PACKET)) };
      if (!stale) run.active = node;
      break;
    }
    case 'turnCompleted':
      if (e.agent === 'codex') run.turnDone = true;
      add('记录', `${name} 这一轮结束`, e.durationMs ? `${Math.round(e.durationMs / 1000)} 秒` : undefined, e.lastMessage);
      say(`${name} 这一轮结束`, '这一轮结束');
      break;
    case 'verdict':
      if (e.outcome === 'start') {
        run.verdicting = true;
        say('Claude 正在汇总两份审查意见', '汇总意见');
      } else {
        run.lastVerdict = e.outcome;
        if (e.outcome !== 'pass') run.issues += 1;
        const text = { pass: '裁决：通过', fix: '裁决：打回，带修复清单回到 Codex', fail: '裁决：失败，3 轮后仍有问题或任务无法完成' }[e.outcome];
        add(e.outcome === 'pass' ? '记录' : '问题', text);
        say(text, { pass: '通过', fix: '打回', fail: '失败' }[e.outcome]);
      }
      break;
    case 'runResult': {
      let fresh = 0, total = 0;
      for (const m of Object.values(e.perModel)) {
        const f = m.input + m.cacheCreation + m.output;
        fresh += f; total += f + m.cacheRead;
      }
      run.claudeSources = { ...run.claudeSources, [e.sourceId]: { fresh, total, final: true } };
      run.agents = { ...run.agents, claude: claudeTotals(run.claudeSources) };
      run.costUsd = (prev.costUsd ?? 0) + e.costUsd;
      run.gate = true; run.verdicting = false;
      // A failed run has not passed review: it must never read as ready to merge.
      if (e.status !== 'success') {
        run.failed = true; run.issues += 1;
        add('问题', '运行失败，没有通过审查', e.status);
        if (!stale) {
          run.now = `运行失败（${e.status}），改动没有通过审查`; run.nowWho = 'human'; run.active = 'gate';
          run.status = { ...run.status, gate: '运行失败' };
        }
        break;
      }
      add('记录', '两个 AI 的工作结束', `${e.numTurns} 轮对话，约 $${e.costUsd.toFixed(2)}`);
      if (!stale) {
        run.now = '等你决定：合并，或者放弃这次改动'; run.nowWho = 'human'; run.active = 'gate';
        run.status = { ...run.status, gate: '等待你的决定' };
        packet = { edge: 'pass', who: 'claude', count: 3 };
      }
      break;
    }
  }
  return { run, packet };
}

/** The state for a set of events regardless of arrival order, so live and replayed views agree. */
export function rebuild(events: RunEvent[]): RunState {
  let run = emptyRun();
  for (const e of sortEvents(events)) run = applyEvent(run, e).run;
  return run;
}
export function sortEvents(events: RunEvent[]): RunEvent[] {
  return events.map((e, i) => [e, i] as const).sort((a, b) => a[0].tsMs - b[0].tsMs || a[1] - b[1]).map(p => p[0]);
}

export function isLive(run: RunState, nowMs: number, windowMs = 30_000): boolean {
  return run.lastTs > 0 && nowMs - run.lastTs < windowMs;
}

export function basename(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() || path;
}
