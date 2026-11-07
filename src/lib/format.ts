import type { DailyUsage } from './dashboard';

export function formatTokens(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  if (Math.abs(value) >= 100_000_000) return `${Number((value / 100_000_000).toFixed(1))} 亿`;
  if (Math.abs(value) >= 10_000) {
    const roundedWan = Number((value / 10_000).toFixed(1));
    if (Math.abs(roundedWan) >= 10_000) return `${Number((value / 100_000_000).toFixed(1))} 亿`;
    return `${roundedWan} 万`;
  }
  return value.toLocaleString('zh-CN', { maximumFractionDigits: 2 });
}
export function progressWidth(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
}
export function quotaTone(value: number | null): 'normal' | 'warning' | 'danger' {
  return value != null && value > 90 ? 'danger' : value != null && value > 70 ? 'warning' : 'normal';
}
export function isFast(tier: string | null | undefined): boolean { return tier === 'fast' || tier === 'priority'; }
export function windowName(minutes: number | null | undefined): string {
  if (minutes == null || !Number.isFinite(minutes)) return '额度窗口';
  if (minutes === 10080) return '周额度';
  if (minutes === 300) return '5 小时额度';
  if (minutes > 0 && minutes % 1440 === 0) return `${minutes / 1440} 天额度`;
  if (minutes > 0 && minutes % 60 === 0) return `${minutes / 60} 小时额度`;
  return `${minutes.toLocaleString('zh-CN')} 分钟额度`;
}
export function localDateKey(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}
function isStatDate(value: string | null): value is string {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const date = new Date(`${value}T00:00:00Z`);
  return Number.isFinite(date.getTime()) && date.toISOString().slice(0, 10) === value;
}
export function latestStatDate(daily: DailyUsage[]): string | null {
  let latest: string | null = null;
  for (const { startDate } of daily) {
    if (isStatDate(startDate) && (latest === null || startDate > latest)) latest = startDate;
  }
  return latest;
}
export function recentDays(daily: DailyUsage[], today: Date, count = 30): { startDate: string; tokens: number }[] {
  const values = new Map<string, number>();
  for (const day of daily) {
    if (isStatDate(day.startDate) && day.tokens != null && Number.isFinite(day.tokens)) {
      values.set(day.startDate, (values.get(day.startDate) ?? 0) + Math.max(0, day.tokens));
    }
  }
  return Array.from({ length: Math.max(0, count) }, (_, index) => {
    // Local calendar arithmetic avoids UTC shifts and daylight-saving gaps.
    const date = new Date(today.getFullYear(), today.getMonth(), today.getDate() - count + index + 1, 12);
    const startDate = localDateKey(date);
    return { startDate, tokens: values.get(startDate) ?? 0 };
  });
}
export function localTime(date: Date): string {
  return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
}
export function resetTime(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds)) return '未知';
  const date = new Date(seconds * 1000);
  if (Number.isNaN(date.getTime())) return '未知';
  return date.toLocaleString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false });
}
export function effortName(effort: string | null): string {
  return effort ?? '未指定';
}
export function planName(plan: string | null): string {
  return plan ?? '套餐未知';
}
