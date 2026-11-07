use super::*;
use crate::dto;
use serde_json::json;

fn models() -> Vec<Model> {
    dto::models(json!({"data":[
        {"model":"fast-model","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}],"defaultReasoningEffort":"high","serviceTiers":[{"id":"priority"}]},
        {"model":"slow-model","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"low"}],"defaultReasoningEffort":"low"},
        {"model":"hidden-model","hidden":true,"supportedReasoningEfforts":[{"reasoningEffort":"low"}]},
        {"model":"bad-default","supportedReasoningEfforts":[{"reasoningEffort":"low"}],"defaultReasoningEffort":"invalid"},
        {"model":"no-default","supportedReasoningEfforts":[{"reasoningEffort":"low"}]}
    ]})).unwrap()
}
fn current(effort: Option<&str>, tier: Option<&str>) -> Config {
    dto::config(json!({"config":{"model":"fast-model","model_reasoning_effort":effort,"service_tier":tier}})).unwrap()
}
fn request(value: Value) -> SettingsRequest {
    let mut value = value;
    value["expectedVersion"] = json!("sha256:front-end-version");
    serde_json::from_value(value).unwrap()
}
fn planned(current: &Config, value: Value) -> Value {
    let edits = plan_edits(current, &request(value), &models()).unwrap();
    let value = serde_json::to_value(edits).unwrap();
    for edit in value.as_array().unwrap() {
        assert!(["model", "model_reasoning_effort", "service_tier"].contains(&edit["keyPath"].as_str().unwrap()));
        assert_eq!(edit["mergeStrategy"], "replace");
    }
    value
}
fn edit(key: &str, value: Value) -> Value { json!({"keyPath":key,"value":value,"mergeStrategy":"replace"}) }

#[test]
fn models_are_validated_including_hidden_unknown_and_missing_target() {
    for id in ["hidden-model", "unknown-model", ""] {
        assert!(plan_edits(&current(Some("high"), None), &request(json!({"model":id})), &models()).is_err());
    }
    assert_eq!(planned(&current(Some("low"), None), json!({"model":"slow-model"})), json!([edit("model", json!("slow-model"))]));
    let mut absent = current(Some("high"), None);
    for model in [None, Some("unknown-model".into()), Some("hidden-model".into())] {
        absent.model = model;
        for update in [json!({"effort":"low"}), json!({"fast":true})] {
            assert!(plan_edits(&absent, &request(update), &models()).is_err());
        }
        assert_eq!(planned(&absent, json!({"fast":false})), json!([]));
    }
}
#[test]
fn effort_checks_target_model_and_explicit_effort_takes_precedence() {
    let c = current(Some("high"), None);
    assert_eq!(planned(&c, json!({"effort":"low"})), json!([edit("model_reasoning_effort", json!("low"))]));
    assert_eq!(planned(&c, json!({"model":"slow-model","effort":"low"})), json!([edit("model", json!("slow-model")),edit("model_reasoning_effort", json!("low"))]));
    for update in [json!({"effort":"ultra"}), json!({"model":"slow-model","effort":"high"})] {
        assert!(plan_edits(&c, &request(update), &models()).is_err());
    }
    // A bad default is irrelevant when a supported effort was supplied explicitly.
    assert!(plan_edits(&c, &request(json!({"model":"bad-default","effort":"low"})), &models()).is_ok());
}
#[test]
fn model_changes_fall_back_to_valid_default_only() {
    for effort in [None, Some("high"), Some("unknown")] {
        let c = current(effort, None);
        assert_eq!(planned(&c, json!({"model":"slow-model"})), json!([edit("model", json!("slow-model")),edit("model_reasoning_effort", json!("low"))]));
        for model in ["bad-default", "no-default"] {
            assert!(plan_edits(&c, &request(json!({"model":model})), &models()).is_err());
        }
    }
    assert_eq!(planned(&current(Some("low"), None), json!({"model":"bad-default"})), json!([edit("model", json!("bad-default"))]));
}
#[test]
fn fast_validates_support_and_clears_the_key_including_unknown_tiers() {
    assert_eq!(planned(&current(Some("high"), None), json!({"fast":true})), json!([edit("service_tier", json!("fast"))]));
    for tier in ["fast", "priority", "other"] {
        assert_eq!(planned(&current(Some("high"), Some(tier)), json!({"fast":false})), json!([edit("service_tier", Value::Null)]));
    }
    assert_eq!(planned(&current(Some("high"), Some("other")), json!({"fast":true})), json!([edit("service_tier", json!("fast"))]));
    assert!(plan_edits(&current(Some("low"), None), &request(json!({"model":"slow-model","fast":true})), &models()).is_err());
    let mut slow = current(Some("low"), None);
    slow.model = Some("slow-model".into());
    assert!(plan_edits(&slow, &request(json!({"fast":true})), &models()).is_err());
}
#[test]
fn changing_model_disables_fast_only_when_necessary() {
    for tier in ["fast", "priority"] {
        assert_eq!(planned(&current(Some("high"), Some(tier)), json!({"model":"slow-model"})), json!([
            edit("model", json!("slow-model")),edit("model_reasoning_effort", json!("low")),edit("service_tier", Value::Null)
        ]));
    }
    assert_eq!(planned(&current(Some("low"), Some("other")), json!({"model":"slow-model"})), json!([edit("model", json!("slow-model"))]));
    assert_eq!(planned(&current(Some("high"), Some("fast")), json!({"effort":"low"})), json!([edit("model_reasoning_effort", json!("low"))]));
}
#[test]
fn unchanged_returns_no_edits_and_no_unknown_keys_can_be_planned() {
    for update in [json!({}), json!({"model":"fast-model"}), json!({"effort":"high"}), json!({"fast":true}), json!({"model":"fast-model","effort":"high","fast":true})] {
        for tier in ["fast", "priority"] { assert_eq!(planned(&current(Some("high"), Some(tier)), update.clone()), json!([])); }
    }
    assert_eq!(planned(&current(Some("high"), None), json!({"fast":false})), json!([]));
    // Enumerate successes and failures and assert the key boundary on every successful plan.
    for model in [None, Some("fast-model"), Some("slow-model"), Some("hidden-model"), Some("unknown-model"), Some("bad-default")] {
        for effort in [None, Some("low"), Some("high"), Some("invalid")] {
            for fast in [None, Some(true), Some(false)] {
                for tier in [None, Some("fast"), Some("priority"), Some("other")] {
                    let req = request(json!({"model":model,"effort":effort,"fast":fast}));
                    if let Ok(edits) = plan_edits(&current(Some("high"), tier), &req, &models()) {
                        for edit in serde_json::to_value(edits).unwrap().as_array().unwrap() {
                            assert!(["model", "model_reasoning_effort", "service_tier"].contains(&edit["keyPath"].as_str().unwrap()));
                        }
                    }
                }
            }
        }
    }
    assert!(serde_json::from_value::<SettingsRequest>(json!({"expectedVersion":"v", "filePath":"arbitrary"})).is_err());
}
