use super::{common::*, model::*, redact::Redactor, time};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
pub struct ClaudeParser {
    pub skipped: u64,
    source_id: String,
    seen_usage: HashSet<String>,
    last_ts: u64,
    redactor: Redactor,
}
impl ClaudeParser {
    pub fn new() -> Self { Self::default() }
    pub fn with_home(home: Option<String>) -> Self { Self { redactor: Redactor::new(home), ..Self::default() } }
    /// Live streams often omit timestamps; use receipt time without changing fixture replay.
    pub fn feed_line_at(&mut self, line: &str, now_ms: u64) -> Vec<RunEvent> {
        self.last_ts = now_ms;
        self.feed_line(line)
    }
    pub fn feed_line(&mut self, line: &str) -> Vec<RunEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { self.skipped += 1; return vec![]; };
        // Undated control frames inherit the preceding timestamp, preserving stream order.
        self.last_ts = time::parse_ms(s(&v["timestamp"])).unwrap_or(self.last_ts);
        if let Some(id) = v["session_id"].as_str() { self.source_id = self.redactor.clean(id); }
        let mut kinds = vec![];
        match s(&v["type"]) {
            "system" if v["subtype"] == "init" => kinds.push(EventKind::Session { session: Session {
                thread_id: self.source_id.clone(), model: self.opt(&v["model"]), cwd: self.opt(&v["cwd"]), ..Session::default()
            }}),
            "system" if v["subtype"] == "task_started" && v["subagent_type"] == "codex:codex-rescue" => {
                let first = s(&v["prompt"]).lines().next().unwrap_or("");
                let mode = if first.split_whitespace().any(|s| s == "--resume") { HandoffMode::Resume } else { HandoffMode::Fresh };
                kinds.push(EventKind::Handoff { mode, description: self.redactor.text(s(&v["description"])).0 });
            }
            "assistant" if v["message"].is_object() => {
                let m = &v["message"];
                for block in array(&m["content"]) {
                    match s(&block["type"]) {
                        "text" => {
                            let raw = s(&block["text"]);
                            let mut pending = Vec::new();
                            // Preserve order within a text block: review text before a verdict must
                            // not be classified as verdict activity, and following text must be.
                            for line in raw.split('\n') {
                                let outcome = if s(&v["parent_tool_use_id"]).is_empty() {
                                    match line.trim() {
                                        "[[verdict:start]]" => Some(VerdictOutcome::Start),
                                        "[[verdict:pass]]" => Some(VerdictOutcome::Pass),
                                        "[[verdict:fix]]" => Some(VerdictOutcome::Fix),
                                        "[[verdict:fail]]" => Some(VerdictOutcome::Fail),
                                        _ => None,
                                    }
                                } else { None };
                                if let Some(outcome) = outcome {
                                    self.flush_text(&mut pending, &mut kinds);
                                    kinds.push(EventKind::Verdict { outcome });
                                } else { pending.push(line); }
                            }
                            self.flush_text(&mut pending, &mut kinds);
                        }
                        "tool_use" => {
                            let input = &block["input"];
                            let summary = ["command", "file_path", "description", "pattern"].into_iter().find_map(|k| input[k].as_str()).unwrap_or("");
                            let (summary, truncated) = self.redactor.text(summary);
                            kinds.push(EventKind::Tool { tool_use_id: self.redactor.clean(s(&block["id"])), name: self.redactor.clean(s(&block["name"])), summary, is_error: false, truncated });
                        }
                        "thinking" => {},
                        _ => self.skipped += 1,
                    }
                }
                let id = s(&m["id"]);
                let dedup = format!("{}:{id}", self.source_id);
                if !id.is_empty() && m["usage"].is_object() && self.seen_usage.insert(dedup) {
                    let u = &m["usage"];
                    let input = n(&u["input_tokens"]);
                    let cached_input = n(&u["cache_read_input_tokens"]);
                    let cache_write = n(&u["cache_creation_input_tokens"]);
                    let output = n(&u["output_tokens"]);
                    let fresh = input.saturating_add(cache_write).saturating_add(output);
                    let total = fresh.saturating_add(cached_input);
                    kinds.push(EventKind::Tokens { tokens: Tokens {
                        usage: Usage { input, cached_input, cache_write, output, total, fresh, reasoning: 0 }, delta: total, provisional: true,
                        model: self.opt(&m["model"]), scope: if s(&v["parent_tool_use_id"]).is_empty() { Scope::Main } else { Scope::Sub },
                        message_id: Some(self.redactor.clean(id)),
                    }});
                }
            }
            "user" => {
                for block in array(&v["message"]["content"]) {
                    if block["type"] == "tool_result" {
                        let (text, truncated) = self.redactor.text(&content(&block["content"]));
                        kinds.push(EventKind::ToolResult { tool_use_id: self.redactor.clean(s(&block["tool_use_id"])), text, is_error: block["is_error"].as_bool().unwrap_or(false), truncated });
                    } else { self.skipped += 1; }
                }
            }
            "rate_limit_event" => {
                let w = &v["rate_limit_info"]["unifiedWindows"];
                let windows = ["five_hour", "seven_day"].into_iter().filter_map(|name| Some(Window {
                    name: name.into(), used_percent: w[name]["utilization"].as_f64()? * 100.0, resets_at: w[name]["resetsAt"].as_f64(),
                })).collect();
                kinds.push(EventKind::RateLimit { source: "claude".into(), windows });
            }
            "result" => {
                let mut per_model = BTreeMap::new();
                if let Some(models) = v["modelUsage"].as_object() {
                    for (name, u) in models {
                        per_model.insert(self.redactor.clean(name), ModelUsage { input: n(&u["inputTokens"]), output: n(&u["outputTokens"]),
                            cache_read: n(&u["cacheReadInputTokens"]), cache_creation: n(&u["cacheCreationInputTokens"]), cost_usd: u["costUSD"].as_f64().unwrap_or(0.0) });
                    }
                }
                kinds.push(EventKind::RunResult { status: if v["is_error"] == true { "error".into() } else { self.redactor.clean(s(&v["subtype"])) },
                    cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0), duration_ms: n(&v["duration_ms"]), num_turns: n(&v["num_turns"]), per_model });
            }
            _ => self.skipped += 1,
        }
        kinds.into_iter().map(|kind| RunEvent { ts_ms: self.last_ts, agent: Agent::Claude, source_id: self.source_id.clone(), kind }).collect()
    }
    fn flush_text(&self, lines: &mut Vec<&str>, kinds: &mut Vec<EventKind>) {
        if lines.is_empty() { return; }
        let (text, truncated) = self.redactor.text(&lines.join("\n"));
        lines.clear();
        if !text.is_empty() { kinds.push(EventKind::Message { role: "assistant".into(), phase: None, text, truncated }); }
    }
    fn opt(&self, v: &Value) -> Option<String> { v.as_str().map(|s| self.redactor.clean(s)) }
}
