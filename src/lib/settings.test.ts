import { describe, expect, it } from 'vitest';
import type { CardState, Config, Model } from './dashboard';
import { parseDashboard } from './dashboard';
import { beginSettingsWrite, canStartDashboard, confirmedSettings, parseSettingsResult, serviceTierState, settingsControls, settingsMessage, shouldAcceptDashboard, SETTINGS_ERROR } from './settings';
import type { RefreshGate, SettingsResult } from './settings';

const config: Config = { model: 'fast-model', effort: 'high', serviceTier: 'fast', version: 'sha256:v1' };
const models: Model[] = [
  { model: 'fast-model', displayName: 'Fast display name', isDefault: true, hidden: false, efforts: ['low', 'high'], defaultEffort: 'high', fastSupported: true },
  { model: 'slow-model', displayName: null, isDefault: false, hidden: null, efforts: ['low'], defaultEffort: 'low', fastSupported: false },
  { model: 'hidden-model', displayName: null, isDefault: false, hidden: true, efforts: ['high'], defaultEffort: 'high', fastSupported: true },
];
const result = (outcome: SettingsResult['outcome']): SettingsResult => ({ outcome, config: { ...config, effort: 'low', version: 'sha256:v2' } });

describe('刷新代次与写入竞态', () => {
  it('写入期间禁止刷新，丢弃此前请求；写入完成后不受旧请求阻挡', () => {
    let gate: RefreshGate = { generation: 0, saving: false, inFlightGeneration: null };
    expect(canStartDashboard(gate)).toBe(true);
    gate = { ...gate, inFlightGeneration: 0 };
    expect(canStartDashboard(gate)).toBe(false);
    expect(shouldAcceptDashboard(0, gate, true)).toBe(true);
    gate = beginSettingsWrite(gate);
    expect(gate.generation).toBe(1);
    expect(canStartDashboard(gate)).toBe(false);
    expect(shouldAcceptDashboard(0, gate, true)).toBe(false);
    expect(shouldAcceptDashboard(1, gate, true)).toBe(false);
    gate = { ...gate, saving: false };
    expect(canStartDashboard(gate)).toBe(true);
    expect(shouldAcceptDashboard(0, gate, true)).toBe(false);
    gate = { ...gate, inFlightGeneration: 1 };
    expect(shouldAcceptDashboard(1, gate, true)).toBe(true);
    expect(shouldAcceptDashboard(1, gate, false)).toBe(false);
    gate = beginSettingsWrite(gate);
    expect(gate.generation).toBe(2);
    expect(shouldAcceptDashboard(1, { ...gate, saving: false }, true)).toBe(false);
  });
});
describe('只显示后端确认值与控件可用性', () => {
  it('保存时禁用三个控件，显示值不变；成功才采用后端结果，失败保留实际值', () => {
    const controls = settingsControls(config, models, true);
    expect([controls.modelDisabled, controls.effortDisabled, controls.fastDisabled]).toEqual([true, true, true]);
    expect([controls.model, controls.effort, controls.checked]).toEqual(['fast-model', 'high', true]);
    const previous: CardState<Config> = { data: config, updatedAt: new Date(0), error: null };
    expect(confirmedSettings(previous, null, new Date(1))).toBe(previous);
    for (const outcome of ['applied', 'unchanged', 'conflict', 'overridden'] as const) {
      const response = result(outcome);
      const next = confirmedSettings(previous, response, new Date(1));
      expect(next.data).toBe(response.config);
      expect(next.updatedAt).toEqual(new Date(1));
    }
  });
  it('没有版本时全部禁用并提示', () => {
    for (const version of [null, '']) {
      const controls = settingsControls({ ...config, version }, models, false);
      expect([controls.modelDisabled, controls.effortDisabled, controls.fastDisabled]).toEqual([true, true, true]);
      expect(controls.versionMessage).toBe('无法读取配置版本');
    }
    expect(settingsControls(null, models, false).modelDisabled).toBe(true);
  });
  it('模型不支持 Fast 时禁用，推理强度来自当前模型，隐藏模型不进入列表', () => {
    const normal = settingsControls(config, models, false);
    expect([normal.modelDisabled, normal.effortDisabled, normal.fastDisabled]).toEqual([false, false, false]);
    expect(normal.choices.map(m => m.model)).toEqual(['fast-model', 'slow-model']);
    expect(normal.efforts).toEqual(['low', 'high']);
    expect(normal.versionMessage).toBeNull();
    const slow = settingsControls({ ...config, model: 'slow-model' }, models, false);
    expect(slow.fastDisabled).toBe(true);
    expect(slow.efforts).toEqual(['low']);
    for (const model of ['unknown-model', 'hidden-model']) {
      const current = settingsControls({ ...config, model }, models, false);
      expect(current.currentModelMissing).toBe(true);
      expect(current.model).toBe(model);
      expect(current.modelDisabled).toBe(false);
      expect(current.effortDisabled).toBe(true);
      expect(current.fastDisabled).toBe(true);
    }
  });
  it('模型列表不可用或数据失效时禁用', () => {
    for (const controls of [settingsControls(config, null, false), settingsControls(config, [], false), settingsControls(config, models, false, true)]) {
      expect([controls.modelDisabled, controls.effortDisabled, controls.fastDisabled]).toEqual([true, true, true]);
    }
  });
});
describe('档位与结果提示', () => {
  it.each(['fast', 'priority'])('%s 显示开启', tier => expect(serviceTierState(tier)).toEqual({ checked: true, note: null }));
  it.each([null, undefined])('缺失档位 %s 显示关闭', tier => expect(serviceTierState(tier)).toEqual({ checked: false, note: null }));
  it.each(['flex', 'unknown', ''])('未知档位 %s 显示关闭与原始值', tier => {
    expect(serviceTierState(tier)).toEqual({ checked: false, note: `当前为其他档位：${tier}` });
    expect(settingsControls({ ...config, serviceTier: tier }, models, false).checked).toBe(false);
  });
  it('成功、未变、冲突、覆盖与错误提示', () => {
    expect(settingsMessage(result('applied'))).toBe('已保存');
    expect(settingsMessage(result('unchanged'))).toBe('配置未变化');
    expect(settingsMessage(result('conflict'))).toBe('配置已被其他程序修改，已刷新为最新值，请确认后重试');
    expect(settingsMessage({ ...result('overridden'), overriddenBy: 'system', effectiveValue: 'high' })).toBe('已写入，但被 system 覆盖，当前生效值：high');
    expect(settingsMessage({ ...result('overridden'), overriddenBy: 'project', effectiveValue: null })).toBe('已写入，但被 project 覆盖，当前生效值：null');
    expect(settingsMessage({ ...result('overridden'), effectiveValue: { model: 'a' } })).toBe('已写入，但被 未知层 覆盖，当前生效值：{"model":"a"}');
    expect(settingsMessage(null)).toBe(SETTINGS_ERROR);
  });
  it('解析设置响应和新增 DTO 字段；丢弃多余原文', () => {
    const parsed = parseSettingsResult({ ...result('overridden'), message: 'PRIVATE', filePath: 'PRIVATE', overriddenBy: 'system', effectiveValue: 'high' });
    expect(parsed.config.version).toBe('sha256:v2');
    expect(JSON.stringify(parsed)).not.toContain('PRIVATE');
    for (const value of [null, [], { outcome: 'PRIVATE', config }, { outcome: 'applied' }, { outcome: 'applied', config: [] }]) {
      expect(() => parseSettingsResult(value)).toThrow(SETTINGS_ERROR);
    }
    const dashboard = parseDashboard({ config: { ok: true, data: config }, models: { ok: true, data: models } });
    expect(dashboard.config.ok && dashboard.config.data.version).toBe('sha256:v1');
    expect(dashboard.models.ok && dashboard.models.data[0].defaultEffort).toBe('high');
    expect(parseSettingsResult({ outcome: 'unchanged', config: {} }).config.version).toBeNull();
  });
});
