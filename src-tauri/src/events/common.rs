use serde_json::Value;
pub fn s(v: &Value) -> &str { v.as_str().unwrap_or("") }
pub fn n(v: &Value) -> u64 { v.as_u64().unwrap_or(0) }
pub fn array(v: &Value) -> impl Iterator<Item = &Value> { v.as_array().into_iter().flatten() }
pub fn content(v: &Value) -> String {
    if let Some(s) = v.as_str() { return s.to_owned(); }
    array(v).filter_map(|b| b.as_str().or_else(|| b.get("text").and_then(Value::as_str))).collect::<Vec<_>>().join("\n")
}
