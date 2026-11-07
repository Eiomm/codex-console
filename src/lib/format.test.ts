import { describe, expect, it } from 'vitest';
import { effortName, formatTokens, isFast, latestStatDate, planName, progressWidth, quotaTone, recentDays, windowName } from './format';
import { emptyDashboard, mergeDashboard, parseDashboard } from './dashboard';

describe('中文格式与日期补齐', () => {
  it.each([
    [0, '0'], [9999, '9,999'], [12345, '1.2 万'], [8650066670, '86.5 亿'],
    [91000000, '9100 万'], [99999999, '1 亿'], [99950000, '9995 万'],
    [10000, '1 万'], [100000000, '1 亿'], [null, '—'],
  ] as const)('格式化 %s', (value, expected) => expect(formatTokens(value)).toBe(expected));
  it('从未排序的记录中找到最新统计日期', () => {
    expect(latestStatDate([
      { startDate: '2026-11-06', tokens: 70 },
      { startDate: '2026-11-04', tokens: 12 },
      { startDate: '2026-11-05', tokens: 34 },
    ])).toBe('2026-11-06');
  });
  it('无统计记录时返回 null', () => {
    expect(latestStatDate([])).toBeNull();
  });
  it('忽略 null 日期，全为 null 时返回 null', () => {
    expect(latestStatDate([{ startDate: null, tokens: 99 }, { startDate: '2026-11-06', tokens: 0 }])).toBe('2026-11-06');
    expect(latestStatDate([{ startDate: null, tokens: 99 }])).toBeNull();
  });
  it('忽略无效日期、错误格式和不存在的日历日期', () => {
    expect(latestStatDate([
      { startDate: '2026-02-29', tokens: 1 },
      { startDate: '2026-13-01', tokens: 1 },
      { startDate: '2026-1-01', tokens: 1 },
      { startDate: 'invalid', tokens: 1 },
      { startDate: '2024-02-29', tokens: null },
    ])).toBe('2024-02-29');
    expect(latestStatDate([{ startDate: '', tokens: 1 }])).toBeNull();
  });
  it('今日补零不会被当作已有官方记录，今日真实零值记录仍有效', () => {
    const daily = [{ startDate: '2026-11-06', tokens: 70 }];
    const days = recentDays(daily, new Date(2026, 10, 7));
    expect(days[29]).toEqual({ startDate: '2026-11-07', tokens: 0 });
    expect(latestStatDate(daily)).toBe('2026-11-06');
    expect(latestStatDate([...daily, { startDate: '2026-11-07', tokens: 0 }])).toBe('2026-11-07');
  });
  it.each([['prolite', 'prolite'], ['pro', 'pro'], ['unknown-plan', 'unknown-plan'], [null, '套餐未知']] as const)('套餐原始值 %s', (value, expected) => expect(planName(value)).toBe(expected));
  it.each([['high', 'high'], ['xhigh', 'xhigh'], ['max', 'max'], ['ultra', 'ultra'], ['unknown-effort', 'unknown-effort'], [null, '未指定']] as const)('推理强度原始值 %s', (value, expected) => expect(effortName(value)).toBe(expected));
  it('缺失日期补零，覆盖跨月和本地日历', () => {
    const days = recentDays([{ startDate: '2026-03-30', tokens: 12 }, { startDate: '2026-04-01', tokens: 34 }], new Date(2026, 3, 1), 3);
    expect(days).toEqual([{ startDate: '2026-03-30', tokens: 12 }, { startDate: '2026-03-31', tokens: 0 }, { startDate: '2026-04-01', tokens: 34 }]);
  });
  it('空数组产生最近 30 天的零值', () => {
    const days = recentDays([], new Date(2026, 3, 16));
    expect(days).toHaveLength(30);
    expect(days.every(d => d.tokens === 0)).toBe(true);
    expect(days[0].startDate).toBe('2026-03-18');
    expect(days[29].startDate).toBe('2026-04-16');
  });
  it.each([[-5, 0], [12.5, 12.5], [150, 100], [null, 0], [NaN, 0]] as const)('限制进度条 %s', (value, expected) => expect(progressWidth(value)).toBe(expected));
  it.each([['fast', true], ['priority', true], [null, false], [undefined, false], ['default', false]] as const)('Fast 状态 %s', (value, expected) => expect(isFast(value)).toBe(expected));
  it.each([[10080, '周额度'], [300, '5 小时额度'], [60, '1 小时额度'], [1440, '1 天额度'], [45, '45 分钟额度'], [12.5, '12.5 分钟额度'], [null, '额度窗口']] as const)('窗口 %s', (value, expected) => expect(windowName(value as number | null)).toBe(expected));
  it('70 和 90 的变色边界', () => {
    expect([70, 70.1, 90, 90.1].map(quotaTone)).toEqual(['normal', 'warning', 'warning', 'danger']);
  });
});
describe('模块解析和缓存', () => {
  it('模块失败保留数据及原更新时间，其他模块继续更新', () => {
    const first = new Date(2026, 0, 1, 10), second = new Date(2026, 0, 1, 11);
    const state = mergeDashboard(emptyDashboard(), parseDashboard({ quota: { ok: true, data: { planType: 'pro' } }, config: { ok: true, data: { model: 'a' } } }), first);
    const next = mergeDashboard(state, parseDashboard({ quota: { ok: false, error: '读取失败' }, config: { ok: true, data: { model: 'b' } } }), second);
    expect(next.quota.data?.planType).toBe('pro');
    expect(next.quota.updatedAt).toBe(first);
    expect(next.quota.error).toBe('读取失败');
    expect(next.config.data?.model).toBe('b');
    expect(next.config.updatedAt).toBe(second);
    expect(next.config.error).toBeNull();
  });
  it('缺少字段或无效结构不抛异常', () => {
    expect(parseDashboard(null).quota.ok).toBe(false);
    const parsed = parseDashboard({ quota: { ok: true, data: { windows: [null, { usedPercent: 12.5 }], resetCredits: { expiresAt: [9, 2] }, accountId: 'secret' } }, models: { ok: true, data: [] } });
    expect(parsed.models).toEqual({ ok: true, data: [] });
    expect(parsed.quota.ok && parsed.quota.data.windows[0].usedPercent).toBe(12.5);
    expect(parsed.quota.ok && parsed.quota.data.resetCredits.expiresAt).toEqual([2, 9]);
    expect(JSON.stringify(parsed)).not.toContain('secret');
  });
});
