mod config;
mod git;
mod worker;
#[cfg(test)]
mod tests;

pub use config::{ClaudeOptions, build_claude_args};
use crate::events::{model::RunEvent, summary::RunSummary, time};
use config::Config;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread::JoinHandle};

pub(super) type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus { Running, Passed, NeedsAttention, Failed, Stopped, Merged, Discarded, Interrupted }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub run_id: String, pub repo: PathBuf, pub worktree: PathBuf, pub branch: String, pub base: String,
    pub status: RunStatus, pub started_at_ms: u64, pub goal: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest { pub repo: String, pub goal: String, #[serde(default)] pub verify_commands: Vec<String>, pub model: Option<String> }
#[derive(Debug, Serialize)]
pub struct LoadedRun { pub info: RunInfo, pub events: Vec<RunEvent>, pub summary: RunSummary }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult { pub outcome: String, pub branch: String }
struct Active { id: String, stop: Arc<AtomicBool>, handle: JoinHandle<()> }
#[derive(Default)]
struct State { active: Option<Active>, next: u64, shutdown: bool }
pub struct RunManager { config: Config, emit: Emit, state: Mutex<State> }

impl RunManager {
    pub fn current(emit: impl Fn(&str, serde_json::Value) + Send + Sync + 'static) -> Result<Self, String> {
        Self::new(Config::current()?, Arc::new(emit))
    }
    fn new(mut config: Config, emit: Emit) -> Result<Self, String> {
        fs::create_dir_all(&config.data).map_err(|_| "无法创建任务数据目录")?;
        config.data = config.data.canonicalize().map_err(|_| "无法定位任务数据目录")?;
        for name in ["runs", "worktrees"] {
            let dir = config.data.join(name);
            fs::create_dir_all(&dir).map_err(|_| "无法创建任务目录")?;
            if dir.canonicalize().map_err(|_| "无法定位任务目录")? != dir { return Err("任务目录不能是符号链接".into()); }
        }
        let manager = Self { config, emit, state: Mutex::new(State::default()) };
        for mut info in manager.list_runs()? {
            if info.status == RunStatus::Running {
                info.status = RunStatus::Interrupted;
                manager.save_status(&info, Some("应用上次退出时任务尚未结束"))?;
            }
        }
        Ok(manager)
    }
    pub fn start_run(&self, request: StartRequest) -> Result<RunInfo, String> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.shutdown { return Err("应用正在退出".into()); }
        if state.active.as_ref().is_some_and(|a| !a.handle.is_finished()) { return Err("已有任务正在进行，请先停止或等待结束".into()); }
        if let Some(active) = state.active.take() { let _ = active.handle.join(); }
        let goal = validate_request(&request)?;
        let (repo, base) = git::validate_repo(&request.repo)?;
        let launch = self.config.launch()?;
        let started_at_ms = time::now_ms();
        let run_id = loop {
            state.next += 1;
            let seconds = started_at_ms / 1000 % 86400;
            let date = time::date_path((started_at_ms / 86400000) as i64).replace('/', "");
            let id = format!("run-{date}-{:02}{:02}{:02}-{}", seconds/3600, seconds/60%60, seconds%60, state.next);
            if !self.config.data.join("runs").join(&id).exists() && !self.config.data.join("worktrees").join(&id).exists() { break id; }
        };
        let info = RunInfo { worktree: self.config.data.join("worktrees").join(&run_id), branch: format!("console/{run_id}"),
            run_id, repo, base, status: RunStatus::Running, started_at_ms, goal };
        let dir = self.run_dir(&info.run_id)?;
        let task_file = dir.join("task.md");
        let task_file = validate_task_file(&task_file)?;
        fs::create_dir(&dir).map_err(|_| "无法创建运行记录目录")?;
        if let Err(error) = git::run(&info.repo, &["worktree", "add", "-b", &info.branch, path_str(&info.worktree)?, "HEAD"]) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        let prompt = config::prompt(&info.goal, &info.branch, &info.base, path_str(&launch.companion)?, task_file, &request.verify_commands);
        let args = build_claude_args(&prompt, path_str(&launch.companion)?, task_file, &request.verify_commands, &ClaudeOptions { model: request.model, ..Default::default() });
        let stop = Arc::new(AtomicBool::new(false));
        let start = worker::start(&self.config, &info, launch, args, stop.clone(), self.emit.clone());
        match start {
            Ok(handle) => {
                state.active = Some(Active { id: info.run_id.clone(), stop, handle });
                Ok(info)
            }
            Err(error) => {
                let _ = git::run(&info.repo, &["worktree", "remove", "--force", path_str(&info.worktree)?]);
                let _ = git::run(&info.repo, &["branch", "-D", &info.branch]);
                let _ = fs::remove_dir_all(dir);
                Err(error)
            }
        }
    }
    pub fn stop_run(&self, run_id: &str) -> Result<(), String> {
        validate_id(run_id)?;
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.active.as_ref().is_some_and(|a| a.id == run_id) {
            let active = state.active.take().expect("matched active run");
            active.stop.store(true, Ordering::Release);
            active.handle.join().map_err(|_| "任务停止线程异常")?;
            return Ok(());
        }
        self.read_info(run_id)?;
        Ok(())
    }
    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.shutdown = true;
        if let Some(active) = state.active.take() { active.stop.store(true, Ordering::Release); let _ = active.handle.join(); }
    }
    pub fn list_runs(&self) -> Result<Vec<RunInfo>, String> {
        let mut runs = Vec::new();
        for entry in fs::read_dir(self.config.data.join("runs")).map_err(|_| "无法读取任务列表")?.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) { continue; }
            if let Some(id) = entry.file_name().to_str() { if let Ok(info) = self.read_info(id) { runs.push(info); } }
        }
        runs.sort_by(|a,b| b.started_at_ms.cmp(&a.started_at_ms).then(b.run_id.cmp(&a.run_id)));
        Ok(runs)
    }
    pub fn load_run(&self, run_id: &str) -> Result<LoadedRun, String> {
        let info = self.read_info(run_id)?;
        let events = worker::load_events(&self.run_dir(run_id)?.join("events.jsonl"))?;
        let mut summary = RunSummary::default();
        for event in &events { summary.apply(event); }
        Ok(LoadedRun { info, events, summary })
    }
    pub fn merge_run(&self, run_id: &str) -> Result<MergeResult, String> {
        let _state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut info = self.read_info(run_id)?;
        if info.status != RunStatus::Passed { return Err("只有审查通过的任务可以合并".into()); }
        self.validate_owned(&info)?;
        git::run(&info.worktree, &["add", "-A"])?;
        let diff = git::output(&info.worktree, &["diff", "--cached", "--quiet"])?;
        match diff.status.code() {
            Some(0) => {
                if git::run(&info.worktree, &["rev-parse", "HEAD"])? == info.base { return Ok(merge_result("nothing", &info)); }
            }
            Some(1) => {
                let subject: String = info.goal.lines().next().unwrap_or("").chars().take(72).collect();
                let message = format!("console: {subject}\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>");
                git::run(&info.worktree, &["commit", "-m", &message])?;
            }
            _ => return Err("无法检查待提交的改动".into()),
        }
        if !git::run(&info.repo, &["status", "--porcelain"])?.is_empty() { return Ok(merge_result("committedOnly", &info)); }
        let merged = git::output(&info.repo, &["merge", "--no-ff", "--no-edit", &info.branch])?;
        if !merged.status.success() {
            if git::output(&info.repo, &["rev-parse", "--verify", "MERGE_HEAD"])?.status.success() {
                git::run(&info.repo, &["merge", "--abort"]).map_err(|_| "合并失败，无法自动撤销，请检查仓库")?;
                return Ok(merge_result("conflict", &info));
            }
            return Err("Git 合并失败，分支和工作树已保留".into());
        }
        // A successful merge must never be reported as retryable just because cleanup fails.
        info.status = RunStatus::Merged;
        let cleanup = git::run(&info.repo, &["worktree", "remove", path_str(&info.worktree)?]);
        self.save_status(&info, cleanup.as_ref().err().map(|_| "合并成功，但工作树清理失败"))?;
        Ok(merge_result("merged", &info))
    }
    pub fn discard_run(&self, run_id: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut info = self.read_info(run_id)?;
        if info.status == RunStatus::Running || state.active.as_ref().is_some_and(|a| a.id == run_id && !a.handle.is_finished()) { return Err("进行中的任务不能放弃，请先停止".into()); }
        if info.status == RunStatus::Merged { return Err("已经合并的任务不能放弃".into()); }
        if info.status == RunStatus::Discarded { return Ok(()); }
        self.validate_owned(&info)?;
        git::run(&info.repo, &["worktree", "remove", "--force", path_str(&info.worktree)?])?;
        git::run(&info.repo, &["branch", "-D", &info.branch])?;
        info.status = RunStatus::Discarded;
        self.save_status(&info, None)
    }
    fn validate_owned(&self, info: &RunInfo) -> Result<(), String> {
        let expected = self.config.data.join("worktrees").join(&info.run_id);
        if info.worktree != expected || !info.branch.starts_with("console/") || info.branch != format!("console/{}", info.run_id) {
            return Err("任务工作树或分支记录不安全，拒绝操作".into());
        }
        if expected.canonicalize().map_err(|_| "任务工作树不存在")? != expected {
            return Err("任务工作树不在数据目录内，拒绝操作".into());
        }
        let (repo, _) = git::validate_repo(path_str(&info.repo)?)?;
        if repo != info.repo { return Err("仓库记录已变化，拒绝操作".into()); }
        if git::run(&info.worktree, &["symbolic-ref", "--short", "HEAD"])? != info.branch { return Err("工作树当前分支与任务记录不符".into()); }
        let list = git::run(&info.repo, &["worktree", "list", "--porcelain"])?;
        let found = list.split("\n\n").any(|record| record.lines().any(|line| line == format!("worktree {}", info.worktree.display())) && record.lines().any(|line| line == format!("branch refs/heads/{}", info.branch)));
        if !found { return Err("仓库未登记该任务工作树，拒绝操作".into()); }
        Ok(())
    }
    fn run_dir(&self, id: &str) -> Result<PathBuf, String> { validate_id(id)?; Ok(self.config.data.join("runs").join(id)) }
    fn read_info(&self, id: &str) -> Result<RunInfo, String> {
        let dir = self.run_dir(id)?;
        if dir.canonicalize().map_err(|_| "任务不存在")? != dir { return Err("任务记录路径不安全".into()); }
        let meta = dir.join("meta.json");
        if fs::symlink_metadata(&meta).map_err(|_| "无法读取任务记录")?.file_type().is_symlink() { return Err("任务记录路径不安全".into()); }
        let info: RunInfo = serde_json::from_slice(&fs::read(meta).map_err(|_| "无法读取任务记录")?).map_err(|_| "任务记录格式错误")?;
        if info.run_id != id { return Err("任务编号与记录不一致".into()); }
        Ok(info)
    }
    fn save_status(&self, info: &RunInfo, detail: Option<&str>) -> Result<(), String> {
        write_info(&self.run_dir(&info.run_id)?, info)?;
        emit_status(&self.emit, info, detail);
        Ok(())
    }
}
impl Drop for RunManager { fn drop(&mut self) { self.shutdown(); } }
fn merge_result(outcome: &str, info: &RunInfo) -> MergeResult { MergeResult { outcome: outcome.into(), branch: info.branch.clone() } }
fn path_str(path: &Path) -> Result<&str, String> { path.to_str().ok_or_else(|| "路径必须是有效文本".into()) }
fn validate_task_file(path: &Path) -> Result<&str, String> {
    let text = path_str(path)?;
    if !path.is_absolute() || text.chars().any(|c| "\"'`;|&$<>\\\n\r*?()".contains(c)) {
        return Err("任务书路径必须是绝对路径且不能含权限规则不支持的字符".into());
    }
    Ok(text)
}
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 100 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { Err("runId 只能包含字母、数字和连字符".into()) } else { Ok(()) }
}
fn validate_request(request: &StartRequest) -> Result<String, String> {
    let goal = request.goal.trim();
    if goal.is_empty() || goal.chars().count() > 20000 { return Err("任务目标不能为空且不能超过 20000 字符".into()); }
    for command in &request.verify_commands {
        if command.trim().is_empty() || command.chars().count() > 200 || command.chars().any(|c| ";|&`$<>\n\r\\".contains(c)) { return Err("验证命令不能为空、超过 200 字符或包含禁止的控制字符".into()); }
    }
    Ok(goal.into())
}
pub(super) fn write_info(dir: &Path, info: &RunInfo) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(info).map_err(|_| "无法编码任务记录")?;
    fs::write(dir.join("meta.json.tmp"), bytes).map_err(|_| "无法保存任务记录")?;
    fs::rename(dir.join("meta.json.tmp"), dir.join("meta.json")).map_err(|_| "无法更新任务记录".into())
}
pub(super) fn emit_status(emit: &Emit, info: &RunInfo, detail: Option<&str>) {
    let mut payload = serde_json::json!({"runId": info.run_id, "status": info.status});
    if let Some(detail) = detail { payload["detail"] = detail.into(); }
    emit("run-status", payload);
}
