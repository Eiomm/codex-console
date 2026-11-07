import { useEffect, useRef, useState } from 'react';
import { NODES } from '../lib/flow';
import type { NodeId } from '../lib/flow';
import type { FileRow, Row } from '../lib/run';

type Tab = 'act' | 'diff' | 'task';
const clock = (ms: number) => new Date(ms).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });

const MAX_DIFF_LINES = 1500;
function DiffText({ text }: { text: string }) {
  const lines = text.split('\n');
  const shown = lines.slice(0, MAX_DIFF_LINES);
  return <>
    <pre className="diff">{shown.map((line, i) => <span key={i} className={line.startsWith('+') ? 'plus' : line.startsWith('-') ? 'minus' : undefined}>{line}{'\n'}</span>)}</pre>
    {lines.length > MAX_DIFF_LINES && <p className="what">只显示前 {MAX_DIFF_LINES} 行，共 {lines.length} 行。</p>}
  </>;
}

function Activity({ rows, emptyText }: { rows: Row[]; emptyText: string }) {
  const [open, setOpen] = useState<number | null>(null);
  const box = useRef<HTMLUListElement | null>(null);
  const newest = rows[rows.length - 1]?.id;
  useEffect(() => { const el = box.current?.parentElement; if (el) el.scrollTop = el.scrollHeight; }, [newest]);
  if (!rows.length) return <div className="empty">{emptyText}</div>;
  return <ul className="rows" ref={box}>
    {rows.map(r => {
      const canOpen = !!r.detail;
      return <li key={r.id}>
        <time>{clock(r.tsMs)}</time>
        <span className={`kind${r.kind === '问题' ? ' issue' : ''}`}>{r.kind}</span>
        <div className="row-body">
          {canOpen
            ? <button className="row-btn" aria-expanded={open === r.id} onClick={() => setOpen(open === r.id ? null : r.id)}>{r.action}</button>
            : r.action}
          {r.result && <div className={`res${r.kind === '问题' ? ' bad' : ''}`}>{r.result}</div>}
          {open === r.id && r.detail && (r.isDiff ? <DiffText text={r.detail} /> : <pre className="detail">{r.detail}</pre>)}
        </div>
      </li>;
    })}
  </ul>;
}

function Files({ files }: { files: FileRow[] }) {
  const [open, setOpen] = useState<string | null>(null);
  if (!files.length) return <div className="empty">还没有文件改动。</div>;
  // A choice from another directory is no longer valid; fall back to the newest file.
  const shown = files.some(f => f.path === open) ? open! : files[files.length - 1].path;
  const current = files.find(f => f.path === shown);
  return <>
    <ul className="files">
      {files.map(f => <li key={f.path}>
        <button className="row-btn" aria-pressed={shown === f.path} onClick={() => setOpen(f.path)}>{f.path}</button>
        <span><span className="plus">+{f.added}</span> <span className="minus">−{f.removed}</span></span>
      </li>)}
    </ul>
    {current && <DiffText text={current.diff} />}
    {current?.truncated && <p className="what">diff 太长，这里显示的内容不完整。</p>}
  </>;
}

interface Props { node: NodeId; rows: Row[]; files: FileRow[]; tasks: string[] }
export function Inspector({ node, rows, files, tasks }: Props) {
  const [tab, setTab] = useState<Tab>('act');
  const def = NODES[node];
  const tabs: [Tab, string][] = node === 'impl' ? [['act', '活动'], ['diff', '改动'], ['task', '任务书']] : [['act', '活动']];
  const current = tabs.some(t => t[0] === tab) ? tab : 'act';
  return <section className="mod insp-m" aria-label="节点详情" style={{ '--c': `var(--${def.who})` } as React.CSSProperties}><div className="in" /><div className="body">
    <div className="mod-h"><span className="tag">M02</span><h2>{def.title}</h2></div>
    <p className="what">{def.what}</p>
    <div className="tabs" role="tablist">
      {tabs.map(([id, label]) => <button key={id} role="tab" aria-selected={current === id} onClick={() => setTab(id)}>{label}</button>)}
    </div>
    <div className="panes">
      {current === 'act' && <Activity rows={rows} emptyText="这一步还没有记录。" />}
      {current === 'diff' && <Files files={files} />}
      {current === 'task' && (tasks.length
        ? tasks.map((t, i) => <pre key={i} className="prompt">{tasks.length > 1 ? `第 ${i + 1} 次\n` : ''}{t}</pre>)
        : <div className="empty">还没有收到任务书。</div>)}
    </div>
  </div></section>;
}
