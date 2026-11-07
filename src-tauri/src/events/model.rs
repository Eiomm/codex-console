use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent { Claude, Codex }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope { Main, Sub }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandoffMode { Fresh, Resume }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerdictOutcome { Start, Pass, Fix, Fail }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub thread_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub sandbox: Option<String>,
    pub cwd: Option<String>,
    pub originator: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub cached_input: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    pub total: u64,
    pub fresh: u64,
}
impl Usage {
    pub fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached_input = self.cached_input.saturating_add(other.cached_input);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
        self.total = self.total.saturating_add(other.total);
        self.fresh = self.fresh.saturating_add(other.fresh);
    }
    pub fn difference(&self, old: &Self) -> Self {
        Self { input: self.input.saturating_sub(old.input), cached_input: self.cached_input.saturating_sub(old.cached_input),
            cache_write: self.cache_write.saturating_sub(old.cache_write), output: self.output.saturating_sub(old.output),
            reasoning: self.reasoning.saturating_sub(old.reasoning), total: self.total.saturating_sub(old.total), fresh: self.fresh.saturating_sub(old.fresh) }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    #[serde(flatten)] pub usage: Usage,
    /// Codex fields are thread cumulative; Claude fields describe one unique message.
    pub delta: u64,
    pub provisional: bool,
    pub model: Option<String>,
    pub scope: Scope,
    pub message_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window { pub name: String, pub used_percent: f64, pub resets_at: Option<f64> }
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_creation: u64, pub cost_usd: f64 }
impl ModelUsage {
    pub fn usage(&self) -> Usage {
        let fresh = self.input.saturating_add(self.cache_creation).saturating_add(self.output);
        Usage { input: self.input, cached_input: self.cache_read, cache_write: self.cache_creation,
            output: self.output, total: fresh.saturating_add(self.cache_read), fresh, reasoning: 0 }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EventKind {
    Verdict { outcome: VerdictOutcome },
    Session { #[serde(flatten)] session: Session },
    Handoff { mode: HandoffMode, description: String },
    Message { role: String, phase: Option<String>, text: String, truncated: bool },
    Command { command: String, exit_code: Option<i64>, duration_ms: Option<u64>, output_tail: String, status: String, truncated: bool },
    Tool { tool_use_id: String, name: String, summary: String, is_error: bool, truncated: bool },
    ToolResult { tool_use_id: String, text: String, is_error: bool, truncated: bool },
    FileChange { item_id: String, path: String, change_type: String, move_path: Option<String>, diff: String, added: u64, removed: u64, truncated: bool },
    Tokens { #[serde(flatten)] tokens: Tokens },
    RateLimit { source: String, windows: Vec<Window> },
    TurnCompleted { duration_ms: Option<u64>, last_message: String, truncated: bool },
    RunResult { status: String, cost_usd: f64, duration_ms: u64, num_turns: u64, per_model: BTreeMap<String, ModelUsage> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub ts_ms: u64,
    pub agent: Agent,
    /// Thread/session identity, never an account or user identity.
    pub source_id: String,
    #[serde(flatten)] pub kind: EventKind,
}
