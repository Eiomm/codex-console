export type Nullable<T> = T | null;
export interface QuotaWindow { usedPercent: Nullable<number>; windowDurationMins: Nullable<number>; resetsAt: Nullable<number> }
export interface Quota {
  planType: Nullable<string>;
  ordinaryUsageAllowed: Nullable<boolean>;
  windows: QuotaWindow[];
  credits: { hasCredits: Nullable<boolean>; balance: Nullable<string> };
  resetCredits: { availableCount: Nullable<number>; expiresAt: number[] };
}
export interface DailyUsage { startDate: Nullable<string>; tokens: Nullable<number> }
export interface Usage { lifetimeTokens: Nullable<number>; currentStreakDays: Nullable<number>; longestStreakDays: Nullable<number>; daily: DailyUsage[] }
export interface Config { model: Nullable<string>; effort: Nullable<string>; serviceTier: Nullable<string>; version: Nullable<string> }
export interface Model { model: Nullable<string>; displayName: Nullable<string>; isDefault: Nullable<boolean>; hidden: Nullable<boolean>; efforts: string[]; defaultEffort: Nullable<string>; fastSupported: boolean }
export type ModuleResult<T> = { ok: true; data: T } | { ok: false; error: string };
export interface Dashboard { quota: ModuleResult<Quota>; usage: ModuleResult<Usage>; config: ModuleResult<Config>; models: ModuleResult<Model[]> }
export interface CardState<T> { data: T | null; updatedAt: Date | null; error: string | null }
export interface DashboardState { quota: CardState<Quota>; usage: CardState<Usage>; config: CardState<Config>; models: CardState<Model[]> }

const record = (v: unknown): Record<string, unknown> => v !== null && typeof v === 'object' && !Array.isArray(v) ? v as Record<string, unknown> : {};
const isRecord = (v: unknown): boolean => v !== null && typeof v === 'object' && !Array.isArray(v);
const array = (v: unknown): unknown[] => Array.isArray(v) ? v : [];
const number = (v: unknown): Nullable<number> => typeof v === 'number' && Number.isFinite(v) ? v : null;
const string = (v: unknown): Nullable<string> => typeof v === 'string' ? v : null;
const boolean = (v: unknown): Nullable<boolean> => typeof v === 'boolean' ? v : null;

function parseQuota(value: unknown): Quota {
  const v = record(value), credits = record(v.credits), resets = record(v.resetCredits);
  return {
    planType: string(v.planType), ordinaryUsageAllowed: boolean(v.ordinaryUsageAllowed),
    windows: array(v.windows).filter(isRecord).map(item => { const w = record(item); return { usedPercent: number(w.usedPercent), windowDurationMins: number(w.windowDurationMins), resetsAt: number(w.resetsAt) }; }),
    credits: { hasCredits: boolean(credits.hasCredits), balance: string(credits.balance) },
    resetCredits: { availableCount: number(resets.availableCount), expiresAt: array(resets.expiresAt).map(number).filter((v): v is number => v !== null).sort((a, b) => a - b) },
  };
}
function parseUsage(value: unknown): Usage {
  const v = record(value);
  return { lifetimeTokens: number(v.lifetimeTokens), currentStreakDays: number(v.currentStreakDays), longestStreakDays: number(v.longestStreakDays), daily: array(v.daily).filter(isRecord).map(item => { const d = record(item); return { startDate: string(d.startDate), tokens: number(d.tokens) }; }) };
}
export function parseConfig(value: unknown): Config {
  const v = record(value);
  return { model: string(v.model), effort: string(v.effort), serviceTier: string(v.serviceTier), version: string(v.version) };
}
function parseModels(value: unknown): Model[] {
  return array(value).filter(isRecord).map(item => {
    const v = record(item);
    return { model: string(v.model), displayName: string(v.displayName), isDefault: boolean(v.isDefault), hidden: boolean(v.hidden), efforts: array(v.efforts).filter((e): e is string => typeof e === 'string'), defaultEffort: string(v.defaultEffort), fastSupported: v.fastSupported === true };
  });
}
function parseModule<T>(value: unknown, parse: (value: unknown) => T, list = false): ModuleResult<T> {
  const v = record(value);
  if (v.ok === false) return { ok: false, error: string(v.error) || '读取失败，请稍后刷新' };
  if (v.ok !== true || !(list ? Array.isArray(v.data) : isRecord(v.data))) return { ok: false, error: '返回的数据结构无法解析' };
  return { ok: true, data: parse(v.data) };
}
export function parseDashboard(value: unknown): Dashboard {
  const v = record(value);
  return { quota: parseModule(v.quota, parseQuota), usage: parseModule(v.usage, parseUsage), config: parseModule(v.config, parseConfig), models: parseModule(v.models, parseModels, true) };
}
export function emptyDashboard(): DashboardState {
  const empty = <T>(): CardState<T> => ({ data: null, updatedAt: null, error: null });
  return { quota: empty(), usage: empty(), config: empty(), models: empty() };
}
export function mergeModule<T>(previous: CardState<T>, result: ModuleResult<T>, now: Date): CardState<T> {
  return result.ok ? { data: result.data, updatedAt: now, error: null } : { ...previous, error: result.error };
}
export function mergeDashboard(previous: DashboardState, next: Dashboard, now: Date): DashboardState {
  return { quota: mergeModule(previous.quota, next.quota, now), usage: mergeModule(previous.usage, next.usage, now), config: mergeModule(previous.config, next.config, now), models: mergeModule(previous.models, next.models, now) };
}
export function failedDashboard(): Dashboard {
  const error = { ok: false as const, error: '无法连接本地服务，请稍后刷新' };
  return { quota: error, usage: error, config: error, models: error };
}
