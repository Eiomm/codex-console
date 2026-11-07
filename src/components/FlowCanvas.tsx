import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { arrowAngle, chamfer, clip, layoutFor, LAYOUTS, NODES, route } from '../lib/flow';
import type { EdgeId, LayoutMode, NodeId, Who } from '../lib/flow';
import { formatTokens } from '../lib/format';
import type { Packet } from '../lib/run';

const SVG_NS = 'http://www.w3.org/2000/svg';
const PACKET_GAP_MS = 170;
const PACKET_SPEED = 0.14; // viewBox units per millisecond
const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
const color = (w: Who) => `var(--${w})`;

interface Props {
  containerRef: React.RefObject<HTMLElement | null>;
  selected: NodeId;
  active: NodeId | null;
  hotEdges: Partial<Record<EdgeId, Who>>;
  status: Partial<Record<NodeId, string>>;
  fresh: Partial<Record<NodeId, number>>;
  onSelect: (node: NodeId) => void;
  packets: React.RefObject<Packet[]>;
  /** Changing this discards queued and in-flight packets, so old data never plays in a new view. */
  resetKey: number;
}

export function FlowCanvas({ containerRef, selected, active, hotEdges, status, fresh, onSelect, packets, resetKey }: Props) {
  const [mode, setMode] = useState<LayoutMode>('wide');
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => setMode(layoutFor(el.clientWidth));
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => observer.disconnect();
  }, [containerRef]);
  const layout = LAYOUTS[mode];

  const hotRefs = useRef<Partial<Record<EdgeId, SVGPathElement | null>>>({});
  const layer = useRef<SVGGElement | null>(null);
  useEffect(() => {
    if (reducedMotion()) return;
    type Flying = { el: SVGRectElement; path: SVGPathElement; at: number; len: number };
    let flying: Flying[] = [];
    const queue: { edge: EdgeId; who: Who; left: number; wait: number }[] = [];
    let last = performance.now(), frame = 0;
    const tick = (now: number) => {
      const dt = Math.min(now - last, 60); last = now;
      for (const p of packets.current ?? []) queue.push({ edge: p.edge, who: p.who, left: Math.min(p.count, 40), wait: 0 });
      if (packets.current) packets.current.length = 0;
      for (const q of queue) {
        q.wait -= dt;
        const path = hotRefs.current[q.edge];
        if (q.left > 0 && q.wait <= 0 && path && layer.current) {
          const rect = document.createElementNS(SVG_NS, 'rect');
          rect.setAttribute('width', '7'); rect.setAttribute('height', '2.5'); rect.setAttribute('x', '-3.5'); rect.setAttribute('y', '-1.25');
          rect.setAttribute('fill', color(q.who));
          layer.current.appendChild(rect);
          flying.push({ el: rect, path, at: 0, len: path.getTotalLength() });
          q.left -= 1; q.wait = PACKET_GAP_MS;
        }
      }
      for (let i = queue.length - 1; i >= 0; i--) if (queue[i].left <= 0) queue.splice(i, 1);
      flying = flying.filter(f => {
        f.at += dt * PACKET_SPEED;
        if (f.at >= f.len || !f.path.isConnected) { f.el.remove(); return false; }
        const a = f.path.getPointAtLength(f.at), b = f.path.getPointAtLength(Math.min(f.at + 1, f.len));
        f.el.setAttribute('transform', `translate(${a.x} ${a.y}) rotate(${Math.atan2(b.y - a.y, b.x - a.x) * 180 / Math.PI})`);
        return true;
      });
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => { cancelAnimationFrame(frame); flying.forEach(f => f.el.remove()); };
  }, [packets, resetKey]);

  return <svg id="flow" viewBox={`0 0 ${layout.box[0]} ${layout.box[1]}`} role="group" aria-label="协作流程图">
    {(Object.entries(layout.edges) as [EdgeId, typeof layout.edges[EdgeId]][]).map(([id, edge]) => {
      const d = route(edge.pts);
      const hot = hotEdges[id];
      const end = edge.pts[edge.pts.length - 1];
      return <g key={id} className={hot ? 'edge-on' : undefined}>
        <path d={d} className="glow" stroke={hot ? color(hot) : undefined} />
        <path d={d} className={`trace${edge.back ? ' back' : ''}`} />
        <path d={d} className="hot" stroke={hot ? color(hot) : undefined} ref={el => { hotRefs.current[id] = el; }} />
        <path d="M-6 -4 L0 0 L-6 4" fill="none" stroke="var(--hot)" strokeWidth="1.5" transform={`translate(${end[0]} ${end[1]}) rotate(${arrowAngle(edge.pts)})`} />
        <text x={edge.label[1]} y={edge.label[2]} textAnchor={edge.label[3]} className="edge-l">{edge.label[0]}</text>
      </g>;
    })}
    {(Object.entries(layout.nodes) as [NodeId, [number, number, number, number]][]).map(([id, [x, y, w, h]]) => {
      const def = NODES[id];
      const value = fresh[id] ?? 0;
      const state = status[id] ?? '等待';
      const cls = ['node', active === id ? 'active' : '', selected === id ? 'sel' : ''].filter(Boolean).join(' ');
      return <g key={id} className={cls} tabIndex={0} role="button" aria-pressed={selected === id} aria-label={`${def.title}：${def.role}。状态：${state}`} style={{ '--c': color(def.who) } as React.CSSProperties}
        onClick={() => onSelect(id)} onKeyDown={event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); onSelect(id); } }}>
        <polygon points={chamfer(x, y, w, h)} className="halo" />
        <polygon points={chamfer(x, y, w, h)} className="frame" />
        <rect x={x} y={y + 12} width="2.5" height={h - 24} className="mark" />
        <text x={x + 14} y={y + 22} className="n-t">{def.title}</text>
        {def.who !== 'human' && <>
          <use href={`#i-${def.who}`} x={x + w - (def.who === 'claude' ? 76 : 70)} y={y + 11} width="13" height="13" style={{ color: color(def.who), fill: 'currentColor' }} />
          <text x={x + w - (def.who === 'claude' ? 60 : 54)} y={y + 22} className="n-w">{def.who === 'claude' ? 'Claude' : 'Codex'}</text>
        </>}
        <text x={x + 14} y={y + 40} className="n-r">{clip(def.role, (w - 24) / 6.1)}</text>
        {id !== 'gate' && <text x={x + 14} y={y + h - 26}><tspan className="n-v">{formatTokens(value)}</tspan><tspan className="n-u" dx="6">token</tspan></text>}
        <text x={x + 14} y={y + h - 9} className="n-s">{clip(state, (w - 24) / 6.6)}</text>
      </g>;
    })}
    <g ref={layer} />
  </svg>;
}
