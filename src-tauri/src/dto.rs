use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Module<T> {
    Success { ok: bool, data: T },
    Failure { ok: bool, error: String },
}
impl<T> Module<T> {
    pub fn from_result(result: Result<T, String>) -> Self {
        match result {
            Ok(data) => Self::Success { ok: true, data },
            Err(error) => Self::Failure { ok: false, error },
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Dashboard { pub quota: Module<Quota>, pub usage: Module<Usage>, pub config: Module<Config>, pub models: Module<Vec<Model>> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    plan_type: Option<String>, ordinary_usage_allowed: Option<bool>, windows: Vec<Window>,
    credits: Credits, reset_credits: ResetCredits,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Window { used_percent: Option<f64>, window_duration_mins: Option<f64>, resets_at: Option<f64> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Credits { has_credits: Option<bool>, balance: Option<String> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResetCredits { available_count: Option<f64>, expires_at: Vec<f64> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage { lifetime_tokens: Option<f64>, current_streak_days: Option<f64>, longest_streak_days: Option<f64>, daily: Vec<Daily> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Daily { start_date: Option<String>, tokens: Option<f64> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config { pub model: Option<String>, pub effort: Option<String>, pub service_tier: Option<String>, pub version: Option<String> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model { pub model: Option<String>, display_name: Option<String>, is_default: Option<bool>, pub hidden: Option<bool>, pub efforts: Vec<String>, pub default_effort: Option<String>, pub fast_supported: bool }

fn object(value: &Value) -> Result<(), String> {
    if value.is_object() { Ok(()) } else { Err("接口返回的结构无法解析".into()) }
}
fn string(value: &Value) -> Option<String> { value.as_str().map(str::to_owned) }
fn array(value: &Value) -> impl Iterator<Item = &Value> { value.as_array().into_iter().flatten() }

pub fn quota(value: Value) -> Result<Quota, String> {
    object(&value)?;
    let limits = &value["rateLimits"];
    let windows = ["primary", "secondary"].into_iter().filter_map(|key| {
        let window = &limits[key];
        window.is_object().then(|| Window { used_percent: window["usedPercent"].as_f64(), window_duration_mins: window["windowDurationMins"].as_f64(), resets_at: window["resetsAt"].as_f64() })
    }).collect();
    let reset = &value["rateLimitResetCredits"];
    let mut expires_at: Vec<_> = array(&reset["credits"])
        .filter(|c| c["status"].as_str().is_none_or(|s| s == "available"))
        .filter_map(|c| c["expiresAt"].as_f64()).collect();
    expires_at.sort_by(f64::total_cmp);
    let balance = &limits["credits"]["balance"];
    Ok(Quota {
        plan_type: string(&limits["planType"]), ordinary_usage_allowed: value["ordinaryUsageAllowed"].as_bool(), windows,
        credits: Credits { has_credits: limits["credits"]["hasCredits"].as_bool(), balance: string(balance).or_else(|| balance.as_f64().map(|n| n.to_string())) },
        reset_credits: ResetCredits { available_count: reset["availableCount"].as_f64(), expires_at },
    })
}
pub fn usage(value: Value) -> Result<Usage, String> {
    object(&value)?;
    let summary = &value["summary"];
    Ok(Usage { lifetime_tokens: summary["lifetimeTokens"].as_f64(), current_streak_days: summary["currentStreakDays"].as_f64(), longest_streak_days: summary["longestStreakDays"].as_f64(), daily: array(&value["dailyUsageBuckets"]).filter(|v| v.is_object()).map(|v| Daily { start_date: string(&v["startDate"]), tokens: v["tokens"].as_f64() }).collect() })
}
pub fn config(value: Value) -> Result<Config, String> {
    object(&value)?;
    let config = &value["config"];
    Ok(Config { model: string(&config["model"]), effort: string(&config["model_reasoning_effort"]), service_tier: string(&config["service_tier"]),
        version: array(&value["layers"]).find(|layer| layer["name"]["type"] == "user").and_then(|layer| string(&layer["version"])),
    })
}
pub fn models(value: Value) -> Result<Vec<Model>, String> {
    object(&value)?;
    Ok(array(&value["data"]).filter(|v| v.is_object()).map(|v| Model {
        model: string(&v["model"]), display_name: string(&v["displayName"]), is_default: v["isDefault"].as_bool(), hidden: v["hidden"].as_bool(),
        efforts: array(&v["supportedReasoningEfforts"]).filter_map(|e| string(&e["reasoningEffort"])).collect(),
        default_effort: string(&v["defaultReasoningEffort"]),
        fast_supported: array(&v["serviceTiers"]).any(|t| t["id"] == "priority"),
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn sanitized_sample_and_optional_fields() {
        let mut sample = json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{"usedPercent":1,"windowDurationMins":10080,"resetsAt":1794379710},"secondary":null,"credits":{"hasCredits":false,"unlimited":false,"balance":"0"},"planType":"prolite"},"rateLimitResetCredits":{"availableCount":2,"credits":[{"id":"secret-reset-id","status":"available","expiresAt":1796069948,"title":"Full reset"},{"expiresAt":1796069900}]},"accountId":"secret-account","email":"secret-email","userId":"secret-user"});
        let dto = serde_json::to_value(quota(sample.clone()).unwrap()).unwrap();
        assert_eq!(dto["windows"].as_array().unwrap().len(), 1);
        assert_eq!(dto["planType"], "prolite");
        assert_eq!(dto["resetCredits"]["expiresAt"], json!([1796069900.0,1796069948.0]));
        let serialized = dto.to_string();
        for forbidden in ["accountId", "email", "userId", "secret-", "\"id\""] { assert!(!serialized.contains(forbidden)); }
        sample.as_object_mut().unwrap().remove("rateLimitResetCredits");
        sample["rateLimits"]["primary"]["usedPercent"] = json!(12.5);
        let dto = serde_json::to_value(quota(sample).unwrap()).unwrap();
        assert_eq!(dto["windows"][0]["usedPercent"], 12.5);
        assert!(dto["resetCredits"]["availableCount"].is_null());
        assert_eq!(dto["resetCredits"]["expiresAt"], json!([]));
    }
    #[test]
    fn config_exposes_only_user_version_and_models_expose_default_effort() {
        let c = config(serde_json::json!({"config":{"model":"m","model_reasoning_effort":"high","service_tier":"priority","secret":"PRIVATE"}, "origins":{"private":"PRIVATE"}, "layers":[
            {"name":{"type":"system"},"version":"system-version","config":{"secret":"PRIVATE"}},
            {"name":{"type":"user","private":"PRIVATE"},"version":"sha256:user","config":{"secret":"PRIVATE"}},
            {"name":{"type":"project"},"version":"project-version"}
        ]})).unwrap();
        let value = serde_json::to_value(c).unwrap();
        assert_eq!(value, serde_json::json!({"model":"m","effort":"high","serviceTier":"priority","version":"sha256:user"}));
        for layers in [serde_json::json!([]), serde_json::json!([{"name":{"type":"system"},"version":"system"}]), serde_json::json!([{"name":{"type":"user"}}])] {
            assert!(config(serde_json::json!({"config":{},"layers":layers})).unwrap().version.is_none());
        }
        let m = models(serde_json::json!({"data":[{"model":"m","defaultReasoningEffort":"high"}, {"model":"other"}]})).unwrap();
        assert_eq!(m[0].default_effort.as_deref(), Some("high"));
        assert!(m[1].default_effort.is_none());
    }
    #[test]
    fn other_samples_and_malformed_structures() {
        let u = usage(json!({"summary":{"lifetimeTokens":8650066670u64,"currentStreakDays":3,"longestStreakDays":24},"dailyUsageBuckets":[{"startDate":"2026-04-16","tokens":59254446}]})).unwrap();
        assert_eq!(u.lifetime_tokens, Some(8650066670.0));
        assert_eq!(u.daily[0].tokens, Some(59254446.0));
        let c = config(json!({"config":{"model":"gpt-6-astra","model_reasoning_effort":"high","service_tier":"fast"}})).unwrap();
        assert_eq!(c.effort.as_deref(), Some("high"));
        let m = models(json!({"data":[{"model":"gpt-6-astra","displayName":"测试","isDefault":false,"hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high"}],"serviceTiers":[{"id":"priority","name":"Fast"}]}]})).unwrap();
        assert!(m[0].fast_supported);
        assert_eq!(m[0].efforts, ["high"]);
        for v in [json!({}), json!({"rateLimits":{"primary":{},"secondary":null}})] { assert!(quota(v).is_ok()); }
        assert!(usage(json!({})).is_ok());
        assert!(config(json!({})).is_ok());
        assert!(models(json!({})).unwrap().is_empty());
        assert!(quota(json!([])).is_err());
        assert!(usage(Value::Null).is_err());
        assert!(config(json!("private response")).unwrap_err().find("private response").is_none());
        assert!(models(json!(42)).is_err());
    }
}
