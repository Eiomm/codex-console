use crate::dto::{Config, Model};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsRequest {
    pub expected_version: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResult {
    pub outcome: &'static str,
    pub config: Config,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overridden_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_value: Option<Value>,
}
impl SettingsResult {
    pub fn new(outcome: &'static str, config: Config) -> Self {
        Self { outcome, config, overridden_by: None, effective_value: None }
    }
}

// An enum makes it impossible for even a future planner branch to supply arbitrary key paths.
#[derive(Debug, Serialize)]
pub enum SettingKey {
    #[serde(rename = "model")]
    Model,
    #[serde(rename = "model_reasoning_effort")]
    Effort,
    #[serde(rename = "service_tier")]
    ServiceTier,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit { key_path: SettingKey, value: Value, merge_strategy: &'static str }
impl Edit {
    fn new(key_path: SettingKey, value: Option<&str>) -> Self {
        Self { key_path, value: value.map_or(Value::Null, |s| Value::String(s.into())), merge_strategy: "replace" }
    }
}
fn fast_enabled(tier: Option<&str>) -> bool { matches!(tier, Some("fast" | "priority")) }

pub fn plan_edits(current: &Config, request: &SettingsRequest, models: &[Model]) -> Result<Vec<Edit>, String> {
    let target_id = request.model.as_deref().or(current.model.as_deref());
    let target = models.iter().find(|m| m.model.as_deref() == target_id && m.model.is_some() && m.hidden != Some(true));
    let mut edits = Vec::new();
    if let Some(model) = &request.model {
        if target.is_none() { return Err("模型不可用，请刷新模型列表".into()); }
        if current.model.as_ref() != Some(model) { edits.push(Edit::new(SettingKey::Model, Some(model))); }
    }
    if let Some(effort) = &request.effort {
        let target = target.ok_or("模型不可用，请刷新模型列表")?;
        if !target.efforts.contains(effort) { return Err("目标模型不支持此推理强度".into()); }
        if current.effort.as_ref() != Some(effort) { edits.push(Edit::new(SettingKey::Effort, Some(effort))); }
    } else if request.model.is_some() {
        let target = target.ok_or("模型不可用，请刷新模型列表")?;
        if !current.effort.as_ref().is_some_and(|effort| target.efforts.contains(effort)) {
            let default = target.default_effort.as_ref().filter(|effort| target.efforts.contains(effort))
                .ok_or("目标模型没有可用的默认推理强度")?;
            edits.push(Edit::new(SettingKey::Effort, Some(default)));
        }
    }
    match request.fast {
        Some(true) => {
            if !target.is_some_and(|m| m.fast_supported) { return Err("目标模型不支持 Fast".into()); }
            if !fast_enabled(current.service_tier.as_deref()) { edits.push(Edit::new(SettingKey::ServiceTier, Some("fast"))); }
        }
        Some(false) => {
            if current.service_tier.is_some() { edits.push(Edit::new(SettingKey::ServiceTier, None)); }
        }
        None if request.model.is_some() && fast_enabled(current.service_tier.as_deref()) && target.is_some_and(|m| !m.fast_supported) => {
            edits.push(Edit::new(SettingKey::ServiceTier, None));
        }
        None => {}
    }
    Ok(edits)
}

#[cfg(test)]
mod tests;
