use super::{common::*, model::*, redact::Redactor, time};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct CodexParser {
    pub skipped: u64,
    session: Session,
    last_session: Option<Session>,
    total: u64,
    cwd: String,
    redactor: Redactor,
}
impl CodexParser {
    pub fn new() -> Self { Self::default() }
    pub fn with_home(home: Option<String>) -> Self { Self { redactor: Redactor::new(home), ..Self::default() } }
    pub fn feed_line(&mut self, line: &str) -> Vec<RunEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { self.skipped += 1; return vec![]; };
        let ts = time::parse_ms(s(&v["timestamp"])).unwrap_or(0);
        let p = &v["payload"];
        let mut kinds = vec![];
        match s(&v["type"]) {
            "session_meta" if p.is_object() => {
                self.session.thread_id = self.redactor.clean(s(&p["id"]));
                self.cwd = s(&p["cwd"]).to_owned();
                self.session.cwd = self.opt(&p["cwd"]);
                self.session.originator = self.opt(&p["originator"]);
                self.session_event(&mut kinds);
            }
            "turn_context" if p.is_object() => {
                self.session.model = self.opt(&p["model"]);
                self.session.effort = self.opt(&p["collaboration_mode"]["settings"]["reasoning_effort"]).or_else(|| self.opt(&p["effort"]));
                self.session.sandbox = self.opt(&p["sandbox_policy"]["type"]);
                if p["cwd"].is_string() { self.cwd = s(&p["cwd"]).into(); self.session.cwd = self.opt(&p["cwd"]); }
                self.session_event(&mut kinds);
            }
            "token_usage_record" if p["thread_token_usage"].is_object() => self.tokens(&p["thread_token_usage"], &mut kinds),
            "event_msg" if p.is_object() => match s(&p["type"]) {
                "thread_settings_applied" => {
                    self.session.service_tier = self.opt(&p["thread_settings"]["service_tier"]);
                    self.session_event(&mut kinds);
                }
                "item_completed" => self.item(&p["item"], &mut kinds),
                "token_count" => {
                    self.tokens(&p["info"]["total_token_usage"], &mut kinds);
                    if p["rate_limits"].is_object() {
                        let r = &p["rate_limits"];
                        let windows = ["primary", "secondary"].into_iter().filter_map(|name| {
                            let w = &r[name];
                            Some(Window { name: name.into(), used_percent: w["used_percent"].as_f64()?, resets_at: w["resets_at"].as_f64() })
                        }).collect();
                        kinds.push(EventKind::RateLimit { source: "codex".into(), windows });
                    }
                }
                "task_complete" => {
                    let (last_message, truncated) = self.redactor.text(s(&p["last_agent_message"]));
                    kinds.push(EventKind::TurnCompleted { duration_ms: p["duration_ms"].as_u64(), last_message, truncated });
                }
                _ => self.skipped += 1,
            },
            _ => self.skipped += 1,
        }
        kinds.into_iter().map(|kind| RunEvent { ts_ms: ts, agent: Agent::Codex, source_id: self.session.thread_id.clone(), kind }).collect()
    }
    fn opt(&self, v: &Value) -> Option<String> { v.as_str().map(|s| self.redactor.clean(s)) }
    fn session_event(&mut self, kinds: &mut Vec<EventKind>) {
        if self.last_session.as_ref() != Some(&self.session) {
            kinds.push(EventKind::Session { session: self.session.clone() });
            self.last_session = Some(self.session.clone());
        }
    }
    fn tokens(&mut self, v: &Value, kinds: &mut Vec<EventKind>) {
        let total = n(&v["total_tokens"]);
        if total <= self.total { return; }
        let input = n(&v["input_tokens"]);
        let cached_input = n(&v["cached_input_tokens"]);
        let output = n(&v["output_tokens"]);
        kinds.push(EventKind::Tokens { tokens: Tokens {
            usage: Usage { input, cached_input, cache_write: n(&v["cache_write_input_tokens"]), output,
                reasoning: n(&v["reasoning_output_tokens"]), total, fresh: input.saturating_sub(cached_input).saturating_add(output) },
            delta: total - self.total, provisional: false, model: self.session.model.clone(), scope: Scope::Main, message_id: None,
        }});
        self.total = total;
    }
    fn relative(&self, path: &str) -> String {
        let path = Path::new(path);
        let cwd = Path::new(&self.cwd);
        if let Ok(relative) = path.strip_prefix(cwd) { return self.redactor.clean(&relative.to_string_lossy()); }
        if path.is_absolute() && cwd.is_absolute() {
            let target: Vec<_> = path.components().collect();
            let base: Vec<_> = cwd.components().collect();
            let common = target.iter().zip(&base).take_while(|(a,b)| a == b).count();
            if common > 0 {
                let mut relative = PathBuf::new();
                for _ in common..base.len() { relative.push(".."); }
                for part in &target[common..] { relative.push(part); }
                return self.redactor.clean(&relative.to_string_lossy());
            }
        }
        self.redactor.clean(&path.to_string_lossy())
    }
    fn message(&self, text: &str, role: &str, phase: Option<String>, kinds: &mut Vec<EventKind>) {
        let (text, truncated) = self.redactor.text(text);
        kinds.push(EventKind::Message { role: role.into(), phase, text, truncated });
    }
    fn item(&mut self, item: &Value, kinds: &mut Vec<EventKind>) {
        match s(&item["type"]) {
            "UserMessage" => self.message(&content(&item["content"]), "user", None, kinds),
            "AgentMessage" => self.message(&content(&item["content"]), "assistant", self.opt(&item["phase"]), kinds),
            "Reasoning" => {
                let text = content(&item["summary_text"]);
                if !text.trim().is_empty() { self.message(&text, "assistant", Some("reasoning".into()), kinds); }
            }
            "CommandExecution" => {
                let parsed = array(&item["parsed_cmd"]).filter_map(|v| v["cmd"].as_str()).collect::<Vec<_>>().join("\n");
                let command = if parsed.is_empty() { array(&item["command"]).last().map(s).unwrap_or("") } else { &parsed };
                let (output_tail, truncated) = self.redactor.tail(s(&item["aggregated_output"]));
                let duration_ms = item["duration"].as_object().map(|_| n(&item["duration"]["secs"]).saturating_mul(1000).saturating_add(n(&item["duration"]["nanos"])/1_000_000));
                kinds.push(EventKind::Command { command: self.redactor.clean(command), exit_code: item["exit_code"].as_i64(), duration_ms,
                    output_tail, status: self.redactor.clean(s(&item["status"])), truncated });
            }
            "FileChange" => {
                if let Some(changes) = item["changes"].as_object() {
                    for (path, change) in changes {
                        let raw = s(&change["unified_diff"]);
                        let added = raw.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count() as u64;
                        let removed = raw.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count() as u64;
                        let (diff, truncated) = self.redactor.diff(raw);
                        kinds.push(EventKind::FileChange { item_id: self.redactor.clean(s(&item["id"])), path: self.relative(path),
                            change_type: self.redactor.clean(s(&change["type"])), move_path: change["move_path"].as_str().map(|p| self.relative(p)), diff, added, removed, truncated });
                    }
                } else { self.skipped += 1; }
            }
            _ => self.skipped += 1,
        }
    }
}
