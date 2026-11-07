use super::model::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase { #[default] Plan, Impl, Review, ReReview, Verdict, Gate }
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseSummary {
    pub fresh: u64, pub total: u64, pub commands: u64, pub tools: u64, pub files: u64,
    pub status: String,
    /// Phase-level Claude usage remains an estimate: result supplies no phase breakdown.
    pub provisional: bool,
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary { #[serde(flatten)] pub usage: Usage, pub provisional: bool, pub commands: u64, pub tools: u64, pub files: u64 }
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary { pub path: String, pub added: u64, pub removed: u64 }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestLimit { pub ts_ms: u64, pub windows: Vec<Window> }
#[derive(Debug, Clone)]
enum FactKind {
    Session { sandbox: Option<String>, fast: bool }, Handoff(HandoffMode),
    Activity(&'static str), Command, Tool, File(String, u64, u64), Tokens(Tokens),
    Rate(String, Vec<Window>), Completed, Result(Usage), Verdict(VerdictOutcome),
}
#[derive(Debug, Clone)]
struct Fact { ts: u64, agent: Agent, source: String, kind: FactKind }
#[derive(Debug, Default, Clone)]
struct SourceUsage { stream: Usage, final_usage: Option<Usage>, provisional: bool, messages: BTreeSet<String> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub phase: Phase,
    pub round: u64,
    pub last_verdict: Option<VerdictOutcome>,
    pub phases: BTreeMap<Phase, PhaseSummary>,
    pub agents: BTreeMap<Agent, AgentSummary>,
    pub rate_limits: BTreeMap<String, LatestLimit>,
    pub fast: bool,
    pub files: BTreeMap<String, FileSummary>,
    #[serde(skip)] history: Vec<Fact>,
    #[serde(skip)] sandboxes: BTreeMap<String, String>,
    #[serde(skip)] sources: BTreeMap<(Agent, String), SourceUsage>,
    #[serde(skip)] completed: bool,
    #[serde(skip)] gate: bool,
    #[serde(skip)] verdict: bool,
}
impl Default for RunSummary {
    fn default() -> Self {
        Self { phase: Phase::Plan, round: 0, last_verdict: None,
            phases: [Phase::Plan, Phase::Impl, Phase::Review, Phase::ReReview, Phase::Verdict, Phase::Gate].into_iter().map(|p| (p, PhaseSummary::default())).collect(),
            agents: [Agent::Claude, Agent::Codex].into_iter().map(|a| (a, AgentSummary::default())).collect(),
            rate_limits: BTreeMap::new(), fast: false, files: BTreeMap::new(), history: vec![], sandboxes: BTreeMap::new(), sources: BTreeMap::new(), completed: false, gate: false, verdict: false }
    }
}
impl RunSummary {
    pub fn apply(&mut self, event: &RunEvent) {
        let kind = match &event.kind {
            EventKind::Session { session } => FactKind::Session { sandbox: session.sandbox.clone(), fast: matches!(session.service_tier.as_deref(), Some("priority" | "fast")) },
            EventKind::Verdict { outcome } => FactKind::Verdict(*outcome),
            EventKind::Handoff { mode, .. } => FactKind::Handoff(*mode),
            EventKind::Message { .. } => FactKind::Activity("消息"),
            EventKind::ToolResult { .. } => FactKind::Activity("工具结果"),
            EventKind::Command { .. } => FactKind::Command,
            EventKind::Tool { .. } => FactKind::Tool,
            EventKind::FileChange { path, added, removed, .. } => FactKind::File(path.clone(), *added, *removed),
            EventKind::Tokens { tokens } => FactKind::Tokens(tokens.clone()),
            EventKind::RateLimit { source, windows } => FactKind::Rate(source.clone(), windows.clone()),
            EventKind::TurnCompleted { .. } => FactKind::Completed,
            EventKind::RunResult { per_model, .. } => {
                let mut usage = Usage::default();
                for model in per_model.values() { usage.add(&model.usage()); }
                FactKind::Result(usage)
            }
        };
        let fact = Fact { ts: event.ts_ms, agent: event.agent, source: event.source_id.clone(), kind };
        // Keep compact accounting facts, not command outputs, prompts, or diffs. Normal ordered
        // streams reduce incrementally; late arrivals replay in stable timestamp order.
        let late = self.history.last().is_some_and(|last| last.ts > fact.ts);
        let new_sandbox = if let FactKind::Session { sandbox: Some(sandbox), .. } = &fact.kind {
            self.sandboxes.get(&fact.source) != Some(sandbox)
        } else { false };
        if late || new_sandbox {
            let at = self.history.partition_point(|f| f.ts <= fact.ts);
            self.history.insert(at, fact);
            self.rebuild();
        } else {
            self.reduce(&fact);
            self.history.push(fact);
        }
        self.refresh_totals();
    }
    /// A truncated/replaced rollout is replayed from zero; remove its old contribution first.
    pub(crate) fn remove_source(&mut self, agent: Agent, source: &str) {
        self.history.retain(|fact| fact.agent != agent || fact.source != source);
        self.rebuild();
        self.refresh_totals();
    }
    fn rebuild(&mut self) {
        let history = std::mem::take(&mut self.history);
        *self = Self::default();
        // Session context may arrive after activity. Classify read-only threads consistently.
        for fact in &history {
            if let FactKind::Session { sandbox: Some(sandbox), .. } = &fact.kind { self.sandboxes.insert(fact.source.clone(), sandbox.clone()); }
        }
        for fact in &history { self.reduce(fact); }
        self.history = history;
    }
    fn refresh_totals(&mut self) {
        // Final per-model accounting replaces the stream for that Claude session; it is never added to it.
        for (agent, summary) in &mut self.agents {
            summary.usage = Usage::default(); summary.provisional = false;
            for ((a, _), source) in &self.sources {
                if a == agent {
                    summary.usage.add(source.final_usage.as_ref().unwrap_or(&source.stream));
                    summary.provisional |= source.final_usage.is_none() && source.provisional;
                }
            }
        }
    }
    fn reduce(&mut self, fact: &Fact) {
        if let FactKind::Session { fast, .. } = &fact.kind { self.fast |= *fast; }
        if let FactKind::Handoff(mode) = fact.kind {
            if self.round == 0 { self.round = 1; } else if mode == HandoffMode::Resume || (self.verdict && self.last_verdict == Some(VerdictOutcome::Fix)) { self.round += 1; }
            self.completed = false;
            self.verdict = false;
        }
        if let FactKind::Verdict(outcome) = fact.kind {
            self.last_verdict = Some(outcome);
            self.verdict = true;
        }
        if matches!(fact.kind, FactKind::Result(_)) { self.gate = true; }
        let phase = if fact.agent == Agent::Codex && self.sandboxes.get(&fact.source).is_some_and(|s| s == "read-only") { Phase::ReReview }
            else if self.gate { Phase::Gate }
            else if matches!(fact.kind, FactKind::Handoff(_)) || fact.agent == Agent::Codex { Phase::Impl }
            else if self.verdict { Phase::Verdict }
            else if fact.agent == Agent::Claude && self.completed { Phase::Review }
            else if self.round > 0 { Phase::Impl } else { Phase::Plan };
        self.phase = phase;
        let stage = self.phases.entry(phase).or_default();
        let agent = self.agents.entry(fact.agent).or_default();
        match &fact.kind {
            FactKind::Verdict(_) => stage.status = "裁决".into(),
            FactKind::Session { .. } => stage.status = "会话就绪".into(),
            FactKind::Handoff(_) => stage.status = "已交给 Codex".into(),
            FactKind::Activity(status) => stage.status = (*status).into(),
            FactKind::Command => { stage.commands += 1; agent.commands += 1; stage.status = "命令完成".into(); }
            FactKind::Tool => { stage.tools += 1; agent.tools += 1; stage.status = "调用工具".into(); }
            FactKind::File(path, added, removed) => {
                stage.files += 1; agent.files += 1; stage.status = "文件改动".into();
                let file = self.files.entry(path.clone()).or_insert_with(|| FileSummary { path: path.clone(), ..FileSummary::default() });
                file.added = file.added.saturating_add(*added); file.removed = file.removed.saturating_add(*removed);
            }
            FactKind::Tokens(tokens) => {
                let source = self.sources.entry((fact.agent, fact.source.clone())).or_default();
                let delta = if fact.agent == Agent::Codex {
                    if tokens.usage.total <= source.stream.total { return; }
                    let delta = tokens.usage.difference(&source.stream); source.stream = tokens.usage.clone(); delta
                } else {
                    if let Some(id) = &tokens.message_id { if !source.messages.insert(id.clone()) { return; } }
                    source.stream.add(&tokens.usage); tokens.usage.clone()
                };
                source.provisional |= tokens.provisional;
                stage.fresh = stage.fresh.saturating_add(delta.fresh); stage.total = stage.total.saturating_add(delta.total);
                stage.provisional |= tokens.provisional;
            }
            FactKind::Rate(source, windows) => { self.rate_limits.insert(source.clone(), LatestLimit { ts_ms: fact.ts, windows: windows.clone() }); }
            FactKind::Completed => { if fact.agent == Agent::Codex { self.completed = true; } stage.status = "本轮完成".into(); }
            FactKind::Result(usage) => {
                self.sources.entry((fact.agent, fact.source.clone())).or_default().final_usage = Some(usage.clone());
                stage.status = "运行结束".into();
            }
        }
    }
}
