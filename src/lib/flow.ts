export type NodeId = 'plan' | 'impl' | 'rev' | 're' | 'verdict' | 'gate';
export type EdgeId = 'task' | 'toRev' | 'toRe' | 'revOut' | 'reOut' | 'pass' | 'back';
export type Who = 'claude' | 'codex' | 'human';
export type LayoutMode = 'wide' | 'tall' | 'narrow';

export interface NodeDef { title: string; who: Who; role: string; what: string }
export const NODES: Record<NodeId, NodeDef> = {
  plan: { title: '规划', who: 'claude', role: '读代码，写任务书和验收标准', what: 'Claude 读需求和相关代码，写出任务书：改哪里、不能动哪里、怎样算完成。' },
  impl: { title: '实现', who: 'codex', role: '按任务书改代码、写测试', what: 'Codex 按任务书改代码、写测试。这里是它真实运行的命令和改动。' },
  rev: { title: '审查', who: 'claude', role: '亲自看 diff、亲自跑测试', what: 'Claude 不采信 Codex 的汇报，自己读 diff、自己跑测试，并逐条核对验收标准。' },
  re: { title: '独立复审', who: 'codex', role: '新线程只读审查，不改代码', what: 'Codex 另开一个只读的新线程审查，不受实现时思路的影响。' },
  verdict: { title: '裁决', who: 'claude', role: '汇总两份意见，决定去向', what: 'Claude 汇总两份审查意见。全部满足就通过；有问题就带着修复清单打回实现，最多 3 轮。' },
  gate: { title: '等你决定', who: 'human', role: '合并或放弃，由你决定', what: '两个 AI 的工作到此结束，是否合并由你决定。' },
};

type Rect = [number, number, number, number];
export interface EdgeDef { pts: [number, number][]; back?: boolean; label: [string, number, number, 'start' | 'middle' | 'end'] }
export interface Layout { box: [number, number]; nodes: Record<NodeId, Rect>; edges: Record<EdgeId, EdgeDef> }

export const LAYOUTS: Record<LayoutMode, Layout> = {
  wide: {
    box: [900, 372],
    nodes: { plan: [8, 128, 186, 122], impl: [240, 118, 196, 142], rev: [486, 28, 186, 118], re: [486, 232, 186, 118], verdict: [716, 128, 176, 104], gate: [716, 276, 176, 74] },
    edges: {
      task: { pts: [[194, 189], [240, 189]], label: ['任务书', 217, 180, 'middle'] },
      toRev: { pts: [[436, 170], [461, 170], [461, 87], [486, 87]], label: ['diff', 466, 80, 'start'] },
      toRe: { pts: [[436, 208], [461, 208], [461, 291], [486, 291]], label: ['diff', 466, 284, 'start'] },
      revOut: { pts: [[672, 87], [694, 87], [694, 165], [716, 165]], label: ['意见', 676, 80, 'start'] },
      reOut: { pts: [[672, 291], [694, 291], [694, 195], [716, 195]], label: ['意见', 676, 306, 'start'] },
      pass: { pts: [[804, 232], [804, 276]], label: ['通过', 812, 258, 'start'] },
      back: { pts: [[804, 128], [804, 12], [338, 12], [338, 118]], back: true, label: ['打回：带修复清单', 571, 8, 'middle'] },
    },
  },
  tall: {
    box: [560, 650],
    nodes: { plan: [150, 6, 260, 96], impl: [150, 134, 260, 112], rev: [8, 286, 250, 104], re: [302, 286, 250, 104], verdict: [150, 430, 260, 96], gate: [150, 566, 260, 72] },
    edges: {
      task: { pts: [[280, 102], [280, 134]], label: ['任务书', 288, 122, 'start'] },
      toRev: { pts: [[240, 246], [240, 266], [133, 266], [133, 286]], label: ['diff', 180, 260, 'middle'] },
      toRe: { pts: [[320, 246], [320, 266], [427, 266], [427, 286]], label: ['diff', 380, 260, 'middle'] },
      revOut: { pts: [[133, 390], [133, 410], [240, 410], [240, 430]], label: ['意见', 180, 404, 'middle'] },
      reOut: { pts: [[427, 390], [427, 410], [320, 410], [320, 430]], label: ['意见', 380, 404, 'middle'] },
      pass: { pts: [[280, 526], [280, 566]], label: ['通过', 288, 550, 'start'] },
      back: { pts: [[410, 478], [556, 478], [556, 190], [410, 190]], back: true, label: ['打回：带修复清单', 483, 184, 'middle'] },
    },
  },
  narrow: {
    box: [340, 690],
    nodes: { plan: [34, 6, 270, 92], impl: [34, 124, 270, 104], rev: [34, 254, 270, 92], re: [34, 372, 270, 92], verdict: [34, 490, 270, 88], gate: [34, 604, 270, 72] },
    edges: {
      task: { pts: [[169, 98], [169, 124]], label: ['任务书', 176, 115, 'start'] },
      toRev: { pts: [[169, 228], [169, 254]], label: ['diff', 176, 245, 'start'] },
      toRe: { pts: [[34, 190], [14, 190], [14, 418], [34, 418]], label: ['diff', 2, 184, 'start'] },
      revOut: { pts: [[304, 300], [316, 300], [316, 520], [304, 520]], label: ['意见', 262, 366, 'start'] },
      reOut: { pts: [[169, 464], [169, 490]], label: ['意见', 176, 481, 'start'] },
      pass: { pts: [[169, 578], [169, 604]], label: ['通过', 176, 595, 'start'] },
      back: { pts: [[304, 550], [332, 550], [332, 176], [304, 176]], back: true, label: ['打回', 338, 170, 'end'] },
    },
  },
};

export function layoutFor(width: number): LayoutMode {
  return width >= 720 ? 'wide' : width >= 480 ? 'tall' : 'narrow';
}

/** Right angles with a small diagonal, like a circuit board trace. */
export function route(pts: [number, number][], r = 7): string {
  let d = `M${pts[0][0]} ${pts[0][1]}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const [px, py] = pts[i - 1], [x, y] = pts[i], [nx, ny] = pts[i + 1];
    const a = Math.min(r, Math.hypot(x - px, y - py) / 2), b = Math.min(r, Math.hypot(nx - x, ny - y) / 2);
    d += ` L${x - Math.sign(x - px) * a} ${y - Math.sign(y - py) * a} L${x + Math.sign(nx - x) * b} ${y + Math.sign(ny - y) * b}`;
  }
  const e = pts[pts.length - 1];
  return `${d} L${e[0]} ${e[1]}`;
}

export function chamfer(x: number, y: number, w: number, h: number, c = 9): string {
  return `${x},${y} ${x + w - c},${y} ${x + w},${y + c} ${x + w},${y + h} ${x + c},${y + h} ${x},${y + h - c}`;
}

/** Truncate to roughly `width` Latin-character cells; CJK glyphs count as nearly two. */
export function clip(text: string, width: number): string {
  let used = 0, out = '';
  for (const c of text) {
    used += c.charCodeAt(0) > 255 ? 1.85 : 1;
    if (used > width) return `${out}…`;
    out += c;
  }
  return out;
}

export function arrowAngle(pts: [number, number][]): number {
  const last = pts[pts.length - 1], prev = pts[pts.length - 2];
  return Math.atan2(last[1] - prev[1], last[0] - prev[0]) * 180 / Math.PI;
}
