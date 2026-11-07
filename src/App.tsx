import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { openUrl } from '@tauri-apps/plugin-opener';
import { FlowCanvas } from './components/FlowCanvas';
import { IconSprite, Mark } from './components/Icons';
import { Inspector } from './components/Inspector';
import { emptyDashboard, failedDashboard, mergeDashboard, parseDashboard } from './lib/dashboard';
import { formatTokens, latestStatDate, localDateKey, planName, progressWidth, recentDays, resetTime, windowName } from './lib/format';
import type { EdgeId, NodeId, Who } from './lib/flow';
import { applyEvent, basename, edgeInto, emptyRun, isLive, rebuild, sortEvents } from './lib/run';
import type { Packet, RunEvent, RunState } from './lib/run';
import { beginSettingsWrite, canStartDashboard, confirmedSettings, parseSettingsResult, settingsControls, settingsMessage, shouldAcceptDashboard, SETTINGS_SCOPE } from './lib/settings';
import type { RefreshGate, SettingsUpdate } from './lib/settings';
import './App.css';

interface SessionInfo { threadId: string; cwd: string; originator?: string | null; startedAtMs: number }
interface CwdChoice { cwd: string; label: string; latest: number; count: number }
interface RunInfo { runId: string; repo: string; worktree: string; branch: string; base: string; status: string; startedAtMs: number; goal: string }
interface LoadedRun { info: RunInfo; events: RunEvent[] }
interface MergeResult { outcome: string; branch: string }

const STATUS_TEXT: Record<string, string> = { running: '进行中', passed: '审查通过，等你决定', needsAttention: '需要你查看，没有通过审查', failed: '运行失败', stopped: '已停止', merged: '已合并', discarded: '已放弃', interrupted: '被中断' };
const stored = (key: string) => { try { return localStorage.getItem(key) ?? ''; } catch { return ''; } };
const remember = (key: string, value: string) => { try { localStorage.setItem(key, value); } catch { /* optional convenience */ } };

const CLAUDE_WINDOW: Record<string, string> = { five_hour: 'Claude 5 时', seven_day: 'Claude 7 天' };
const REPLAY_MAX_MS = 45_000;

function Meter({ who, label, percent, value }: { who: 'claude' | 'codex'; label: React.ReactNode; percent: number; value: string }) {
  return <div className={`meter c-${who}`}>
    <span className="who">{label}</span>
    <div className="track"><i style={{ width: `${progressWidth(percent)}%` }} /></div>
    <span className="num">{value}</span>
  </div>;
}

function chooseCwds(sessions: SessionInfo[]): CwdChoice[] {
  const map = new Map<string, CwdChoice>();
  for (const s of sessions) {
    const c = map.get(s.cwd);
    if (c) { c.latest = Math.max(c.latest, s.startedAtMs); c.count += 1; }
    else map.set(s.cwd, { cwd: s.cwd, label: basename(s.cwd), latest: s.startedAtMs, count: 1 });
  }
  return [...map.values()].sort((a, b) => b.latest - a.latest);
}

export default function App() {
  const [dash, setDash] = useState(emptyDashboard);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<{ text: string; transient: boolean } | null>(null);
  const gate = useRef<RefreshGate>({ generation: 0, saving: false, inFlightGeneration: null });
  const mounted = useRef(false);

  const [run, setRun] = useState<RunState>(emptyRun);
  const runRef = useRef(run);
  const allEvents = useRef<RunEvent[]>([]);
  const packets = useRef<Packet[]>([]);
  const [selected, setSelected] = useState<NodeId>('impl');
  const pinned = useRef(false);
  const [choices, setChoices] = useState<CwdChoice[]>([]);
  const [cwd, setCwd] = useState('');
  const [listError, setListError] = useState<string | null>(null);
  const [replaying, setReplaying] = useState(false);
  const replayTimer = useRef<number | null>(null);
  const rebuildTimer = useRef<number | null>(null);
  const [resetKey, setResetKey] = useState(0);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [repo, setRepo] = useState(() => stored('repo'));
  const [goal, setGoal] = useState('');
  const [verify, setVerify] = useState(() => stored('verify'));
  const [activeRun, setActiveRun] = useState<RunInfo | null>(null);
  const [runs, setRuns] = useState<RunInfo[]>([]);
  const [taskMsg, setTaskMsg] = useState<{ text: string; bad: boolean } | null>(null);
  const [armed, setArmed] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const activeRunId = useRef<string | null>(null);
  const starting = useRef(false);
  const earlyRun = useRef<{ watchId: string; event: RunEvent }[]>([]);
  const currentWatch = useRef<string | null>(null);
  const openedAt = useRef(Date.now());
  const flowMod = useRef<HTMLElement | null>(null);

  const refresh = useCallback(async () => {
    if (!canStartDashboard(gate.current)) return;
    const generation = gate.current.generation;
    gate.current = { ...gate.current, inFlightGeneration: generation };
    try {
      const result = parseDashboard(await invoke<unknown>('get_dashboard'));
      if (shouldAcceptDashboard(generation, gate.current, mounted.current)) setDash(p => mergeDashboard(p, result, new Date()));
    } catch {
      if (shouldAcceptDashboard(generation, gate.current, mounted.current)) setDash(p => mergeDashboard(p, failedDashboard(), new Date()));
    } finally {
      if (gate.current.inFlightGeneration === generation) gate.current = { ...gate.current, inFlightGeneration: null };
    }
  }, []);
  const loadChoices = useCallback(async () => {
    try {
      const next = chooseCwds(await invoke<SessionInfo[]>('list_codex_sessions', { cwd: null }));
      if (!mounted.current) return;
      setChoices(next); setListError(null);
      if (!activeRunId.current && !starting.current) setCwd(current => current || next[0]?.cwd || '');
    } catch {
      if (mounted.current) setListError('无法读取会话列表');
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void refresh(); void loadChoices();
    const dashTimer = window.setInterval(() => void refresh(), 60_000);
    const listTimer = window.setInterval(() => void loadChoices(), 15_000);
    const clockTimer = window.setInterval(() => setNowMs(Date.now()), 2_000);
    const onFocus = () => { void refresh(); void loadChoices(); };
    window.addEventListener('focus', onFocus);
    return () => {
      mounted.current = false;
      gate.current = { ...gate.current, generation: gate.current.generation + 1 };
      [dashTimer, listTimer, clockTimer].forEach(t => window.clearInterval(t));
      window.removeEventListener('focus', onFocus);
    };
  }, [refresh, loadChoices]);

  const show = useCallback((next: RunState) => {
    runRef.current = next;
    setRun(next);
    if (!pinned.current && next.active) setSelected(next.active);
  }, []);
  const ingest = useCallback((e: RunEvent, animate: boolean) => {
    const { run: next, packet } = applyEvent(runRef.current, e);
    if (packet && animate) packets.current.push(packet);
    show(next);
  }, [show]);
  const clearPackets = useCallback(() => {
    packets.current.length = 0;
    setResetKey(k => k + 1);
  }, []);
  const stopReplay = useCallback(() => {
    const wasReplaying = replayTimer.current !== null;
    if (wasReplaying) window.clearTimeout(replayTimer.current!);
    replayTimer.current = null;
    setReplaying(false);
    // Everything received so far, including what arrived during the replay, counted exactly once.
    if (wasReplaying) { clearPackets(); show(rebuild(allEvents.current)); }
  }, [clearPackets, show]);

  const accept = useCallback((event: RunEvent) => {
    allEvents.current.push(event);
    if (replayTimer.current !== null) return;
    // Sessions are read file by file, so history can arrive out of order. Rebuild once it settles.
    if (event.tsMs < runRef.current.lastTs) {
      if (rebuildTimer.current === null) {
        rebuildTimer.current = window.setTimeout(() => {
          rebuildTimer.current = null;
          if (replayTimer.current === null) show(rebuild(allEvents.current));
        }, 120);
      }
      return;
    }
    ingest(event, event.tsMs >= openedAt.current - 5_000);
  }, [ingest, show]);

  const loadRuns = useCallback(async () => {
    try { const next = await invoke<RunInfo[]>('list_runs'); if (mounted.current) setRuns(next); } catch { /* the form still works */ }
  }, []);

  useEffect(() => {
    void loadRuns();
    const offs = [
      listen<{ watchId: string; event: RunEvent }>('run-event', m => {
        if (!m.payload.watchId.startsWith('run-')) return;
        if (starting.current) { earlyRun.current.push(m.payload); return; }
        if (m.payload.watchId === activeRunId.current) accept(m.payload.event);
      }),
      listen<{ runId: string; status: string; detail?: string }>('run-status', m => {
        if (m.payload.runId === activeRunId.current) {
          setActiveRun(current => current && current.runId === m.payload.runId ? { ...current, status: m.payload.status } : current);
          if (m.payload.detail) setTaskMsg({ text: m.payload.detail, bad: m.payload.status === 'failed' });
        }
        void loadRuns();
      }),
      // A watched log was truncated or replaced: drop what it contributed and recount.
      listen<{ watchId: string; sourceId: string }>('run-reset', m => {
        if (m.payload.watchId !== activeRunId.current && m.payload.watchId !== currentWatch.current) return;
        allEvents.current = allEvents.current.filter(e => e.sourceId !== m.payload.sourceId);
        show(rebuild(allEvents.current));
      }),
    ];
    return () => { offs.forEach(off => void off.then(stop => stop())); };
  }, [accept, loadRuns, show]);

  useEffect(() => {
    if (!cwd) return;
    stopReplay();
    clearPackets();
    allEvents.current = []; pinned.current = false; show(emptyRun());
    let watchId: string | null = null, cancelled = false;
    openedAt.current = Date.now();
    // The worker may emit before watch_codex returns its id, so hold those events until it is known.
    const early: { watchId: string; event: RunEvent }[] = [];
    const off = listen<{ watchId: string; event: RunEvent }>('run-event', message => {
      if (cancelled) return;
      if (watchId === null) { early.push(message.payload); return; }
      if (message.payload.watchId === watchId) accept(message.payload.event);
    });
    void (async () => {
      try {
        await off;
        const id = await invoke<string>('watch_codex', { cwd });
        if (cancelled) { void invoke('unwatch_codex', { watchId: id }); return; }
        watchId = id; currentWatch.current = id;
        for (const m of early.splice(0)) if (m.watchId === id) accept(m.event);
      } catch {
        if (!cancelled) setListError('无法跟踪这个目录的会话');
      }
    })();
    return () => {
      cancelled = true;
      if (rebuildTimer.current !== null) { window.clearTimeout(rebuildTimer.current); rebuildTimer.current = null; }
      void off.then(stop => stop());
      if (watchId) void invoke('unwatch_codex', { watchId });
    };
  }, [cwd, accept, stopReplay, clearPackets, show]);

  useEffect(() => {
    if (!notice?.transient) return;
    const timer = window.setTimeout(() => setNotice(null), 3500);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const verifyCommands = verify.split('\n').map(s => s.trim()).filter(Boolean);
  const resetView = () => {
    stopReplay(); clearPackets();
    allEvents.current = []; pinned.current = false; show(emptyRun());
    openedAt.current = Date.now();
  };
  const startRun = async () => {
    setTaskMsg(null); setArmed(null); setBusy(true);
    const previousCwd = cwd;
    starting.current = true; earlyRun.current = []; activeRunId.current = null;
    setActiveRun(null); setCwd(''); resetView();
    remember('repo', repo.trim()); remember('verify', verify);
    try {
      const info = await invoke<RunInfo>('start_run', { repo: repo.trim(), goal, verifyCommands, model: null });
      activeRunId.current = info.runId;
      setActiveRun(info);
      starting.current = false;
      for (const m of earlyRun.current.splice(0)) if (m.watchId === info.runId) accept(m.event);
      void loadRuns();
    } catch (error) {
      starting.current = false;
      setCwd(previousCwd);
      setTaskMsg({ text: String(error), bad: true });
    } finally { setBusy(false); }
  };
  const openRun = async (runId: string) => {
    setTaskMsg(null); setArmed(null);
    if (!runId) { leaveRun(); return; }
    try {
      const loaded = await invoke<LoadedRun>('load_run', { runId });
      setCwd(''); resetView();
      activeRunId.current = runId;
      setActiveRun(loaded.info);
      allEvents.current = [...loaded.events];
      show(rebuild(allEvents.current));
    } catch (error) { setTaskMsg({ text: String(error), bad: true }); }
  };
  const leaveRun = () => {
    activeRunId.current = null; setActiveRun(null); setTaskMsg(null); setArmed(null);
    setCwd(choices[0]?.cwd ?? '');
  };
  const runAction = async (name: 'stop' | 'merge' | 'discard') => {
    if (!activeRun) return;
    // No native confirm dialog in this webview: a destructive action takes a second deliberate click.
    if (name !== 'stop' && armed !== name) { setArmed(name); return; }
    setArmed(null); setBusy(true); setTaskMsg(null);
    try {
      if (name === 'stop') await invoke('stop_run', { runId: activeRun.runId });
      else if (name === 'discard') { await invoke('discard_run', { runId: activeRun.runId }); setTaskMsg({ text: '已放弃：工作树和分支都已删除。', bad: false }); }
      else {
        const result = await invoke<MergeResult>('merge_run', { runId: activeRun.runId });
        const text: Record<string, string> = {
          merged: `已合并到主分支，分支 ${result.branch} 保留。没有推送。`,
          committedOnly: `已提交到分支 ${result.branch}。主仓库有未提交的改动，所以没有合并，也没有动它们。`,
          conflict: `已提交到分支 ${result.branch}，但合并有冲突，已撤销合并。需要你手动处理。`,
          nothing: 'Codex 没有留下任何改动，没有可合并的内容。',
        };
        setTaskMsg({ text: text[result.outcome] ?? result.outcome, bad: result.outcome === 'conflict' });
      }
      await loadRuns();
      const latest = (await invoke<RunInfo[]>('list_runs')).find(r => r.runId === activeRun.runId);
      if (latest) setActiveRun(latest);
    } catch (error) { setTaskMsg({ text: String(error), bad: true }); }
    finally { setBusy(false); }
  };

  const replay = () => {
    stopReplay();
    const events = sortEvents(allEvents.current);
    if (!events.length) return;
    clearPackets(); pinned.current = false; show(emptyRun());
    const span = Math.max(events[events.length - 1].tsMs - events[0].tsMs, 1);
    const speed = Math.max(span / REPLAY_MAX_MS, 1);
    setReplaying(true);
    let i = 0;
    const step = () => {
      ingest(events[i], true);
      i += 1;
      if (i >= events.length) { stopReplay(); return; }
      replayTimer.current = window.setTimeout(step, Math.min((events[i].tsMs - events[i - 1].tsMs) / speed, 1_500));
    };
    replayTimer.current = window.setTimeout(step, 200);
  };

  const save = async (update: SettingsUpdate) => {
    const version = dash.config.data?.version;
    if (gate.current.saving || !version) return;
    gate.current = beginSettingsWrite(gate.current);
    setSaving(true); setNotice(null);
    try {
      const result = parseSettingsResult(await invoke<unknown>('update_settings', { expectedVersion: version, ...update }));
      if (mounted.current) {
        setDash(p => ({ ...p, config: confirmedSettings(p.config, result, new Date()) }));
        setNotice({ text: settingsMessage(result), transient: result.outcome === 'applied' });
      }
    } catch {
      if (mounted.current) setNotice({ text: settingsMessage(null), transient: false });
    } finally {
      gate.current = { ...gate.current, saving: false };
      if (mounted.current) { setSaving(false); void refresh(); }
    }
  };

  const running = activeRun?.status === 'running';
  const live = replaying || running || (!activeRun && isLive(run, nowMs));
  const active = live ? run.active : null;
  const hotEdges = useMemo(() => {
    const edges: Partial<Record<EdgeId, Who>> = {};
    if (active) {
      const edge = edgeInto(active, run.round);
      // A wire takes the colour of whoever sent what travels on it.
      const sender: Partial<Record<NodeId, Who>> = { impl: 'claude', rev: 'codex', re: 'codex', verdict: 'claude', gate: 'human' };
      if (edge && sender[active]) edges[edge] = sender[active];
    }
    return edges;
  }, [active, run.round]);

  const quota = dash.quota.data;
  const config = dash.config.data;
  const controls = settingsControls(config, dash.models.data, saving, !!dash.config.error || !!dash.models.error);
  const usage = dash.usage.data;
  const totalFresh = Math.max(run.agents.claude.fresh + run.agents.codex.fresh, 1);
  const days = usage ? recentDays(usage.daily, new Date()) : [];
  const peak = Math.max(1, ...days.map(d => d.tokens));
  const sparkPath = days.map((d, i) => `${i ? 'L' : 'M'}${i * 200 / Math.max(days.length - 1, 1)} ${27 - d.tokens / peak * 25}`).join(' ');
  const hasToday = usage?.daily.some(d => d.startDate === localDateKey(new Date()));
  const latest = usage ? latestStatDate(usage.daily) : null;
  const nowWho: Who = run.now ? run.nowWho : 'codex';
  const nowText = busy && !run.now ? '正在准备工作树并启动 Claude…' : run.now ? (live ? run.now : `最后记录：${run.now}`) : running ? '正在启动 Claude…' : cwd ? '这个目录还没有读到活动记录。' : activeRun ? '这个任务没有活动记录。' : '在上面填写项目和需求，然后开始任务。';
  const selectNode = (node: NodeId) => { pinned.current = true; setSelected(node); };

  return <div className="app">
    <IconSprite />
    <header className="mod"><div className="in" /><div className="body"><div className="top">
      <span className="brand">CODEX<i>//</i>控制台</span>
      <div className="picker">
        <label htmlFor="cwd">目录</label>
        <select id="cwd" value={cwd} onChange={event => { activeRunId.current = null; setActiveRun(null); setCwd(event.target.value); }} disabled={!choices.length || running || busy}>
          {!cwd && choices.length > 0 && <option value="">查看任务时不选目录</option>}
          {!choices.length && <option value="">没有会话</option>}
          {choices.map(c => <option key={c.cwd} value={c.cwd}>{c.label}（{c.count} 个会话）</option>)}
        </select>
      </div>
      {listError && <span className="chip err" role="status">{listError}</span>}
      <div className="rounds" aria-label="审查轮次"><span>{run.round ? `第 ${run.round} 轮` : '未开始'}</span>{[1, 2, 3].map(n => <i key={n} className={run.round >= n ? 'on' : undefined} />)}</div>
      <span className={`chip state-chip${run.failed ? ' err' : ''}`}>{run.failed ? '运行失败' : live ? (replaying ? '回放中' : '进行中') : '空闲'}</span>
      <button className="btn replay-btn" onClick={replaying ? stopReplay : replay} disabled={!allEvents.current.length}>{replaying ? '停止回放' : '回放'}</button>
    </div></div></header>

    <section className="mod" aria-label="任务"><div className="in" /><div className="body">
      <div className="mod-h"><span className="tag">M00</span><h2>任务</h2>
        {runs.length > 0 && <select aria-label="历史任务" style={{ width: 'auto', maxWidth: 300 }} value={activeRun?.runId ?? ''} onChange={event => void openRun(event.target.value)}>
          <option value="">不看历史任务</option>
          {runs.map(r => <option key={r.runId} value={r.runId}>{r.goal.split('\n')[0].slice(0, 24)}（{STATUS_TEXT[r.status] ?? r.status}）</option>)}
        </select>}
      </div>
      <div className="task-body">
        {activeRun ? <>
          <div className="run-meta" style={{ gridColumn: '1 / -1' }}>{activeRun.goal.split('\n')[0]}<br />{activeRun.repo} 分支 {activeRun.branch}<br />工作树 {activeRun.worktree}</div>
        </> : <>
          <div><label htmlFor="repo">项目路径（git 仓库顶层）</label><input id="repo" value={repo} onChange={event => setRepo(event.target.value)} placeholder="/Users/你/项目" spellCheck={false} /></div>
          <div><label htmlFor="goal">需求</label><textarea id="goal" value={goal} onChange={event => setGoal(event.target.value)} placeholder="说明要实现什么、怎样算完成" /></div>
          <div><label htmlFor="verify">验证命令（一行一条，Claude 只能运行这些）</label><textarea id="verify" value={verify} onChange={event => setVerify(event.target.value)} placeholder="pytest tests/test_loader.py" spellCheck={false} /></div>
        </>}
        <div className="task-actions">
          {!activeRun && <button className="btn go" disabled={busy || !repo.trim() || !goal.trim()} onClick={() => void startRun()}>{busy ? '正在启动…' : '开始任务'}</button>}
          {running && <button className="btn danger" disabled={busy} onClick={() => void runAction('stop')}>停止</button>}
          {activeRun?.status === 'passed' && <button className="btn go" disabled={busy} onClick={() => void runAction('merge')}>{armed === 'merge' ? '再点一次确认合并' : '合并到主分支'}</button>}
          {activeRun && !running && activeRun.status !== 'merged' && activeRun.status !== 'discarded' && <button className="btn danger" disabled={busy} onClick={() => void runAction('discard')}>{armed === 'discard' ? '再点一次确认放弃，会删除改动' : '放弃这次改动'}</button>}
          {activeRun && !running && <button className="btn" onClick={leaveRun}>新任务</button>}
          {activeRun && <span className="chip">{STATUS_TEXT[activeRun.status] ?? activeRun.status}</span>}
          {taskMsg && <span className={`msg${taskMsg.bad ? ' err' : ''}`} role="status">{taskMsg.text}</span>}
        </div>
      </div>
    </div></section>

    <div className="grid">
      <section className="mod flow-m" ref={flowMod} aria-label="协作流程"><div className="in" /><div className="body">
        <div className="mod-h"><span className="tag">M01</span><h2>协作流程</h2></div>
        <div className="now" style={{ '--who': `var(--${nowWho})` } as React.CSSProperties}><span className="lbl">NOW</span><span className={`txt${live ? ' live' : ''}`} role="status" title={nowText}>{nowText}</span></div>
        <FlowCanvas containerRef={flowMod} selected={selected} active={active} hotEdges={hotEdges} status={run.status} fresh={run.fresh} onSelect={selectNode} packets={packets} resetKey={resetKey} />
        <div className="legend">
          <span><i style={{ background: 'var(--claude)' }} />Claude 工作中</span>
          <span><i style={{ background: 'var(--codex)' }} />Codex 工作中</span>
          <span><i style={{ background: 'var(--human)' }} />需要你决定</span>
          <span>一个数据包约 2 千 token（新消耗，不含缓存），在模型调用结束时批量到达</span>
        </div>
      </div></section>

      <Inspector node={selected} rows={run.rows[selected]} files={run.files} tasks={run.tasks} />

      <div className="tele">
        <section className="mod" aria-label="token 消耗"><div className="in" /><div className="body">
          <div className="mod-h"><span className="tag">M03</span><h2>token</h2></div>
          <div className="t-body">
            <Meter who="claude" label={<><Mark who="claude" />Claude</>} percent={run.agents.claude.fresh / totalFresh * 100} value={`${run.agents.claude.provisional ? '约 ' : ''}${formatTokens(run.agents.claude.fresh)}`} />
            <Meter who="codex" label={<><Mark who="codex" />Codex</>} percent={run.agents.codex.fresh / totalFresh * 100} value={formatTokens(run.agents.codex.fresh)} />
            <p className="sub one">新消耗。含缓存合计 {formatTokens(run.agents.claude.total + run.agents.codex.total)}{run.costUsd !== null ? `，Claude 约 $${run.costUsd.toFixed(2)}` : ''}</p>
            <svg id="spark" viewBox="0 0 200 28" preserveAspectRatio="none" role="img" aria-label="Codex 最近 30 天每日 token">{days.length > 0 && <path d={sparkPath} fill="none" stroke="var(--codex)" strokeWidth="1" vectorEffect="non-scaling-stroke" />}</svg>
            <p className="sub one">{usage ? <>Codex 近 30 天。累计 {formatTokens(usage.lifetimeTokens)}{!hasToday && latest ? `，官方统计到 ${latest.slice(5).replace('-', '/')}` : ''}</> : 'Codex 近 30 天'}</p>
          </div>
        </div></section>

        <section className="mod" aria-label="额度"><div className="in" /><div className="body">
          <div className="mod-h"><span className="tag">M04</span><h2>额度</h2>{quota && <span className="chip">{planName(quota.planType)}</span>}</div>
          <div className="t-body">
            {dash.quota.error && <p className="sub err" role="status">{dash.quota.error}</p>}
            {quota?.ordinaryUsageAllowed === false && <p className="sub err">当前常规用量不可用</p>}
            {quota?.windows.map((w, i) => <Meter key={i} who="codex" label={`Codex ${windowName(w.windowDurationMins).replace('额度', '')}`} percent={w.usedPercent ?? 0} value={w.usedPercent == null ? '未知' : `${w.usedPercent}%`} />)}
            {Object.keys(CLAUDE_WINDOW).map(name => {
              const w = run.limits.claude?.find(x => x.name === name);
              return <Meter key={name} who="claude" label={CLAUDE_WINDOW[name]} percent={w?.usedPercent ?? 0} value={w ? `${Math.round(w.usedPercent)}%` : '—'} />;
            })}
            <p className="sub one">{quota ? <>{quota.windows[0] ? `Codex ${resetTime(quota.windows[0].resetsAt)} 重置。` : ''}免费重置 {quota.resetCredits.availableCount ?? '—'} 次</> : '正在读取额度'}</p>
            <button className="link" onClick={() => void openUrl('https://chatgpt.com/codex/settings/usage').catch(() => setNotice({ text: '无法打开浏览器', transient: true }))}>查看官方用量页</button>
          </div>
        </div></section>

        <section className="mod" aria-label="Codex 引擎"><div className="in" /><div className="body">
          <div className="mod-h"><span className="tag">M05</span><h2>Codex 引擎</h2></div>
          <div className="t-body">
            <div className="field">
              <div><label htmlFor="m">模型</label>
                <select id="m" value={controls.model} disabled={controls.modelDisabled} onChange={event => void save({ model: event.target.value })}>
                  {config?.model == null && <option value="" disabled>未指定</option>}
                  {controls.currentModelMissing && <option value={controls.model} disabled>{controls.model}（当前）</option>}
                  {controls.choices.map(m => <option key={m.model} value={m.model!}>{m.model}</option>)}
                </select></div>
              <div><label htmlFor="e">推理强度</label>
                <select id="e" value={controls.effort} disabled={controls.effortDisabled} onChange={event => void save({ effort: event.target.value })}>
                  {!controls.efforts.includes(controls.effort) && <option value={controls.effort} disabled>{config?.effort == null ? '未指定' : `${config.effort}（当前）`}</option>}
                  {controls.efforts.map(e => <option key={e} value={e}>{e}</option>)}
                </select></div>
            </div>
            <button className="sw" role="switch" aria-checked={controls.checked} disabled={controls.fastDisabled} onClick={() => void save({ fast: !controls.checked })}><span className="bar" /><span>{controls.checked ? 'Fast 开，订阅额度消耗约 ×2.5' : 'Fast 关'}</span></button>
            {controls.note && <p className="sub">{controls.note}</p>}
            {(controls.versionMessage || dash.config.error) && <p className="sub err" role="status">{dash.config.error ?? controls.versionMessage}</p>}
            <p className="sub status" role="status">{saving ? '正在保存…' : notice?.text ?? SETTINGS_SCOPE}</p>
          </div>
        </div></section>

        <section className="mod" aria-label="本次活动"><div className="in" /><div className="body">
          <div className="mod-h"><span className="tag">M06</span><h2>本次活动</h2></div>
          <div className="t-body stats">
            <div><b>{run.commands}</b><span>条命令</span></div>
            <div><b>{run.files.length}</b><span>个文件</span></div>
            <div><b>{run.issues}</b><span>个问题</span></div>
          </div>
          <div className="t-body"><p className="sub one">{run.fast ? '这次运行用了 Fast' : run.lastTs ? '这次运行没有用 Fast' : '暂无运行记录'}</p></div>
        </div></section>
      </div>
    </div>
  </div>;
}
