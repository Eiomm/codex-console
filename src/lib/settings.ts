import { parseConfig } from './dashboard';
import type { CardState, Config, Model } from './dashboard';
import { isFast } from './format';

export interface SettingsUpdate { model?: string; effort?: string; fast?: boolean }
export interface SettingsResult {
  outcome: 'applied' | 'unchanged' | 'overridden' | 'conflict';
  config: Config;
  overriddenBy?: string;
  effectiveValue?: unknown;
}
export const SETTINGS_ERROR = '无法保存配置，请刷新后重试';
export const SETTINGS_SCOPE = '对之后新开的任务生效，进行中的任务不变。这是全局设置，也会影响终端和 ChatGPT 桌面版的 Codex。';

export interface RefreshGate { generation: number; saving: boolean; inFlightGeneration: number | null }
export function canStartDashboard(gate: RefreshGate): boolean {
  return !gate.saving && gate.inFlightGeneration !== gate.generation;
}
export function shouldAcceptDashboard(startGeneration: number, gate: RefreshGate, mounted: boolean): boolean {
  return mounted && !gate.saving && startGeneration === gate.generation;
}
export function beginSettingsWrite(gate: RefreshGate): RefreshGate {
  return { ...gate, generation: gate.generation + 1, saving: true };
}

export function serviceTierState(tier: string | null | undefined): { checked: boolean; note: string | null } {
  return { checked: isFast(tier), note: tier != null && !isFast(tier) ? `当前为其他档位：${tier}` : null };
}
export function settingsControls(config: Config | null, models: Model[] | null, saving: boolean, unavailable = false) {
  const choices = (models ?? []).filter(m => m.model !== null && m.hidden !== true);
  const selected = choices.find(m => m.model === config?.model);
  const disabled = saving || unavailable || !config?.version || models === null;
  return {
    // These values come only from the last confirmed config, never from a pending edit.
    model: config?.model ?? '', effort: config?.effort ?? '', ...serviceTierState(config?.serviceTier),
    choices, efforts: selected?.efforts ?? [],
    currentModelMissing: config?.model != null && !selected,
    modelDisabled: disabled || choices.length === 0,
    effortDisabled: disabled || !selected || selected.efforts.length === 0,
    fastDisabled: disabled || !selected?.fastSupported,
    versionMessage: !config?.version ? '无法读取配置版本' : null,
  };
}
export function settingsMessage(result: SettingsResult | null): string {
  if (!result) return SETTINGS_ERROR;
  switch (result.outcome) {
    case 'applied': return '已保存';
    case 'unchanged': return '配置未变化';
    case 'conflict': return '配置已被其他程序修改，已刷新为最新值，请确认后重试';
    case 'overridden': {
      const value = typeof result.effectiveValue === 'string' ? result.effectiveValue : JSON.stringify(result.effectiveValue ?? null);
      return `已写入，但被 ${result.overriddenBy ?? '未知层'} 覆盖，当前生效值：${value}`;
    }
  }
}
export function confirmedSettings(previous: CardState<Config>, result: SettingsResult | null, now: Date): CardState<Config> {
  return result ? { data: result.config, error: null, updatedAt: now } : previous;
}
export function parseSettingsResult(value: unknown): SettingsResult {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error(SETTINGS_ERROR);
  const v = value as Record<string, unknown>;
  if (!['applied', 'unchanged', 'overridden', 'conflict'].includes(String(v.outcome)) || v.config === null || typeof v.config !== 'object' || Array.isArray(v.config)) throw new Error(SETTINGS_ERROR);
  return {
    outcome: v.outcome as SettingsResult['outcome'], config: parseConfig(v.config),
    overriddenBy: typeof v.overriddenBy === 'string' ? v.overriddenBy : undefined,
    effectiveValue: v.effectiveValue,
  };
}
