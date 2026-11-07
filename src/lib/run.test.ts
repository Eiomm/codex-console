import { describe, expect, test } from 'vitest';
import { applyEvent, edgeInto, emptyRun, isLive, nodeFor, rebuild } from './run';
import type { RunEvent, RunState } from './run';
import { chamfer, clip, layoutFor, LAYOUTS, route } from './flow';

const base = { tsMs: 1000, agent: 'codex' as const, sourceId: 't1' };
function feed(events: RunEvent[], from: RunState = emptyRun()) {
  let run = from; const packets = [];
  for (const e of events) { const r = applyEvent(run, e); run = r.run; if (r.packet) packets.push(r.packet); }
  return { run, packets };
}
const tok = (total: number, fresh: number, extra = {}): RunEvent => ({ ...base, kind: 'tokens', delta: 0, provisional: false, scope: 'main', total, fresh, ...extra });

describe('run reducer', () => {
  test('Codex activity without Claude is implementation, never planning', () => {
    expect(nodeFor(emptyRun(), { ...base, kind: 'turnCompleted', lastMessage: '', truncated: false })).toBe('impl');
  });
  test('read-only Codex threads are the independent review', () => {
    const { run } = feed([{ ...base, kind: 'session', sandbox: 'read-only' }, { ...base, kind: 'command', command: 'git diff', outputTail: '', status: 'completed', exitCode: 0, truncated: false }]);
    expect(run.rows.re).toHaveLength(1);
    expect(run.rows.impl).toHaveLength(0);
  });
  test('cumulative Codex tokens are not double counted when reported twice', () => {
    const { run, packets } = feed([tok(1000, 500), tok(1000, 500), tok(3000, 4500)]);
    expect(run.agents.codex).toEqual({ fresh: 4500, total: 3000, provisional: false });
    expect(run.fresh.impl).toBe(4500);
    expect(packets.map(p => p.count)).toEqual([1, 2]);
  });
  test('Claude usage is deduplicated by message and replaced by the final result', () => {
    const c = { tsMs: 1, agent: 'claude' as const, sourceId: 's' };
    const usage: RunEvent = { ...c, kind: 'tokens', delta: 0, provisional: true, scope: 'main', messageId: 'm1', fresh: 100, total: 200 };
    const { run } = feed([usage, usage, { ...c, kind: 'runResult', status: 'success', costUsd: 0.5, durationMs: 1, numTurns: 2, perModel: { x: { input: 1, output: 6236, cacheRead: 10, cacheCreation: 3, costUsd: .5 } } }]);
    expect(run.agents.claude).toEqual({ fresh: 6240, total: 6250, provisional: false });
    expect(run.gate).toBe(true);
    expect(run.active).toBe('gate');
  });
  test('the Claude forwarder counts toward Claude, not toward the Codex module', () => {
    const c = { tsMs: 1, agent: 'claude' as const, sourceId: 's' };
    const { run } = feed([
      { ...c, kind: 'handoff', mode: 'fresh', description: 'd' },
      { ...c, kind: 'tokens', delta: 0, provisional: true, scope: 'sub', messageId: 'f', fresh: 9000, total: 9000 },
      tok(1000, 500),
    ]);
    expect(run.agents.claude.fresh).toBe(9000);
    expect(run.fresh.impl).toBe(500);
  });
  test('the forwarder plumbing never appears in the implementation record', () => {
    const c = { tsMs: 1, agent: 'claude' as const, sourceId: 's' };
    const { run } = feed([
      { ...c, kind: 'handoff', mode: 'fresh', description: 'd' },
      { ...c, kind: 'tool', toolUseId: 't', name: 'Bash', summary: 'node codex-companion.mjs task', isError: false, truncated: false },
      { ...c, kind: 'toolResult', toolUseId: 't', text: 'boom', isError: true, truncated: false },
    ]);
    expect(run.rows.impl).toHaveLength(0);
    expect(run.now).toBe('Claude 把任务书交给 Codex');
    expect(run.issues).toBe(0);
  });
  test('a final result replaces only its own session and later stream usage cannot add to it', () => {
    const src = (sourceId: string) => ({ tsMs: 1, agent: 'claude' as const, sourceId });
    const use = (sourceId: string, messageId: string, fresh: number): RunEvent => ({ ...src(sourceId), kind: 'tokens', delta: 0, provisional: true, scope: 'main', messageId, fresh, total: fresh });
    const { run } = feed([
      use('A', 'a1', 100), use('B', 'b1', 200),
      { ...src('A'), kind: 'runResult', status: 'success', costUsd: 1, durationMs: 1, numTurns: 1, perModel: { m: { input: 120, output: 0, cacheRead: 0, cacheCreation: 0, costUsd: 1 } } },
      use('A', 'a2', 999),
    ]);
    expect(run.agents.claude).toEqual({ fresh: 320, total: 320, provisional: true });
  });
  test('verdict markers move activity to the verdict module and back on resume', () => {
    const c = { tsMs: 1, agent: 'claude' as const, sourceId: 's' };
    const msg: RunEvent = { ...c, kind: 'message', role: 'assistant', text: '合并意见', truncated: false };
    let { run } = feed([{ ...c, kind: 'handoff', mode: 'fresh', description: 'd' }, { ...base, kind: 'turnCompleted', lastMessage: '', truncated: false }, { ...c, kind: 'verdict', outcome: 'start' }, msg, { ...c, kind: 'verdict', outcome: 'fix' }]);
    expect(run.rows.verdict.map(r => r.kind)).toEqual(['发言', '问题']);
    expect(run.lastVerdict).toBe('fix');
    expect(run.issues).toBe(1);
    run = feed([{ ...c, kind: 'handoff', mode: 'resume', description: 'd' }], run).run;
    expect(run.verdicting).toBe(false);
    expect(run.round).toBe(2);
    expect(nodeFor(run, msg)).toBe('impl');
    run = feed([{ ...base, kind: 'turnCompleted', lastMessage: '', truncated: false }, { ...c, kind: 'verdict', outcome: 'start' }, { ...c, kind: 'verdict', outcome: 'pass' }], run).run;
    expect(run.lastVerdict).toBe('pass');
    expect(run.issues).toBe(1);
  });
  test('a failed run is never shown as ready to merge', () => {
    const r = applyEvent(emptyRun(), { tsMs: 1, agent: 'claude', sourceId: 's', kind: 'runResult', status: 'error', costUsd: 0, durationMs: 1, numTurns: 1, perModel: {} });
    expect(r.run.failed).toBe(true);
    expect(r.packet).toBeNull();
    expect(r.run.now).toContain('运行失败');
    expect(r.run.now).not.toContain('合并');
    expect(r.run.status.gate).toBe('运行失败');
  });
  test('a late older event joins the record without replacing the newer activity', () => {
    const c = (tsMs: number, command: string): RunEvent => ({ ...base, tsMs, kind: 'command', command, exitCode: 0, outputTail: '', status: 'completed', truncated: false });
    const { run, packets } = feed([c(2000, 'new'), c(1000, 'old'), { ...tok(50, 5000), tsMs: 500 }]);
    expect(run.now).toContain('new');
    expect(run.rows.impl.map(r => r.action)).toEqual(['old', 'new']);
    expect(run.agents.codex.fresh).toBe(5000);
    expect(packets).toHaveLength(0);
  });
  test('rebuild gives one answer regardless of arrival order and counts each event once', () => {
    const c = (tsMs: number, command: string): RunEvent => ({ ...base, tsMs, kind: 'command', command, exitCode: 0, outputTail: '', status: 'completed', truncated: false });
    const a = rebuild([c(1, 'a'), c(2, 'b'), c(3, 'c')]), b = rebuild([c(3, 'c'), c(1, 'a'), c(2, 'b')]);
    expect(a.rows.impl.map(r => r.action)).toEqual(['a', 'b', 'c']);
    expect(b.rows.impl.map(r => r.action)).toEqual(['a', 'b', 'c']);
    expect(b.commands).toBe(3);
    expect(b.now).toBe(a.now);
  });
  test('accumulated diffs stay bounded and say so', () => {
    const f = (i: number): RunEvent => ({ ...base, tsMs: i, kind: 'fileChange', itemId: 'i', path: 'a.ts', changeType: 'update', diff: 'x'.repeat(64 * 1024), added: 1, removed: 0, truncated: false });
    const { run } = feed(Array.from({ length: 20 }, (_, i) => f(i)));
    expect(run.files[0].diff.length).toBeLessThanOrEqual(256 * 1024);
    expect(run.files[0].truncated).toBe(true);
    expect(run.files[0].added).toBe(20);
  });
  test('stream usage from Claude stays provisional until the result arrives', () => {
    const { run } = feed([{ tsMs: 1, agent: 'claude', sourceId: 's', kind: 'tokens', delta: 0, provisional: true, scope: 'main', messageId: 'm', fresh: 5, total: 9 }]);
    expect(run.agents.claude.provisional).toBe(true);
  });
  test('handoff, completed turn and resume walk through the phases and rounds', () => {
    const c = { tsMs: 1, agent: 'claude' as const, sourceId: 's' };
    const msg: RunEvent = { ...c, kind: 'message', role: 'assistant', text: '看 diff', truncated: false };
    let { run } = feed([msg]);
    expect(run.rows.plan).toHaveLength(1);
    run = feed([{ ...c, kind: 'handoff', mode: 'fresh', description: 'd' }], run).run;
    expect(run.round).toBe(1);
    run = feed([{ ...base, kind: 'turnCompleted', lastMessage: 'x', truncated: false }, msg], run).run;
    expect(run.rows.rev).toHaveLength(1);
    run = feed([{ ...c, kind: 'handoff', mode: 'resume', description: 'd' }], run).run;
    expect(run.round).toBe(2);
    expect(run.turnDone).toBe(false);
    expect(edgeInto('impl', run.round)).toBe('back');
    expect(edgeInto('impl', 1)).toBe('task');
  });
  test('file changes to one path merge totals and keep each diff', () => {
    const f = (added: number, diff: string): RunEvent => ({ ...base, kind: 'fileChange', itemId: 'i', path: 'src/signature.py', changeType: 'update', diff, added, removed: 1, truncated: false });
    const { run } = feed([f(2, 'a'), f(3, 'b')]);
    expect(run.files).toEqual([{ path: 'src/signature.py', added: 5, removed: 2, diff: 'a\nb', truncated: false }]);
    expect(run.now).toContain('src/signature.py');
  });
  test('failed commands and tool errors are visible, and empty messages add nothing', () => {
    const { run } = feed([
      { ...base, kind: 'command', command: 'pytest\n--x', exitCode: 1, outputTail: 'boom', status: 'failed', truncated: false },
      { ...base, kind: 'message', role: 'assistant', text: '  \n ', truncated: false },
      { tsMs: 1, agent: 'claude', sourceId: 's', kind: 'toolResult', toolUseId: 't', text: '失败原因', isError: true, truncated: false },
    ]);
    expect(run.rows.impl).toHaveLength(1);
    expect(run.rows.impl[0]).toMatchObject({ kind: '命令', action: 'pytest', result: '退出码 1', detail: 'boom' });
    expect(run.issues).toBe(1);
  });
  test('the reducer does not mutate the previous state', () => {
    const first = emptyRun();
    const snapshot = JSON.stringify(first);
    applyEvent(first, { ...base, kind: 'command', command: 'ls', outputTail: '', status: 'completed', exitCode: 0, truncated: false });
    expect(JSON.stringify(first)).toBe(snapshot);
  });
  test('live window', () => {
    const { run } = feed([tok(10, 10)]);
    expect(isLive(run, 1000 + 29_000)).toBe(true);
    expect(isLive(run, 1000 + 31_000)).toBe(false);
    expect(isLive(emptyRun(), 5)).toBe(false);
  });
});

describe('flow geometry', () => {
  test('layout follows the module width', () => {
    expect([layoutFor(1000), layoutFor(720), layoutFor(600), layoutFor(479)]).toEqual(['wide', 'wide', 'tall', 'narrow']);
  });
  test('no two nodes overlap in any layout and every node stays inside the canvas', () => {
    for (const layout of Object.values(LAYOUTS)) {
      const nodes = Object.values(layout.nodes);
      nodes.forEach(([x, y, w, h], i) => {
        expect(x).toBeGreaterThanOrEqual(0); expect(x + w).toBeLessThanOrEqual(layout.box[0]); expect(y + h).toBeLessThanOrEqual(layout.box[1]);
        nodes.slice(i + 1).forEach(([a, b, c, d]) => expect(x < a + c && a < x + w && y < b + d && b < y + h).toBe(false));
      });
    }
  });
  test('routes end on their last point and clip counts wide glyphs', () => {
    expect(route([[0, 0], [10, 0]])).toBe('M0 0 L10 0');
    expect(route([[0, 0], [20, 0], [20, 20]])).toContain('L20 20');
    expect(chamfer(0, 0, 10, 10, 2)).toContain('8,0');
    expect(clip('abc', 5)).toBe('abc');
    expect(clip('中文内容测试', 5)).toBe('中文…');
  });
});
