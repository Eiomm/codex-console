use super::{common::s, tail::MAX_LINE, time};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fs::{self, File}, io::{BufRead, BufReader, Read}, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub thread_id: String,
    pub cwd: String,
    pub originator: Option<String>,
    pub started_at_ms: u64,
    pub path: PathBuf,
}
pub fn default_root() -> Option<PathBuf> { dirs::home_dir().map(|p| p.join(".codex/sessions")) }
pub fn find_sessions(root: &Path, cwd: Option<&str>, since_ms: u64) -> Vec<SessionInfo> {
    find_sessions_at(root, cwd, since_ms, time::now_ms())
}
/// Explicit clock makes all discovery tests independent of the machine's real sessions and date.
pub fn find_sessions_at(root: &Path, cwd: Option<&str>, since_ms: u64, now_ms: u64) -> Vec<SessionInfo> {
    DiscoveryCache::new(cwd).find_new_at(root, since_ms, now_ms)
}

/// Per-watch cache. Incomplete/invalid headers remain retryable; confirmed cwd mismatches
/// and already discovered sessions never have their first line reopened by discovery.
pub(super) struct DiscoveryCache {
    wanted: Option<PathBuf>,
    discovered: BTreeSet<PathBuf>,
    mismatched: BTreeSet<PathBuf>,
}
impl DiscoveryCache {
    pub(super) fn new(cwd: Option<&str>) -> Self {
        Self { wanted: cwd.map(normalize), discovered: BTreeSet::new(), mismatched: BTreeSet::new() }
    }
    pub(super) fn find_new_at(&mut self, root: &Path, since_ms: u64, now_ms: u64) -> Vec<SessionInfo> {
        let mut result = vec![];
        let earliest = since_ms.max(now_ms.saturating_sub(3 * 86_400_000));
        let latest = now_ms.saturating_add(60_000);
        for path in session_paths_at(root, now_ms) {
            if self.discovered.contains(&path) || self.mismatched.contains(&path) { continue; }
            let Some(info) = read_meta(&path) else { continue; };
            if self.wanted.as_ref().is_some_and(|cwd| *cwd != normalize(&info.cwd)) {
                self.mismatched.insert(path);
                continue;
            }
            if info.started_at_ms < earliest || info.started_at_ms > latest { continue; }
            self.discovered.insert(path);
            result.push(info);
        }
        result.sort_by(|a,b| a.started_at_ms.cmp(&b.started_at_ms).then(a.path.cmp(&b.path)));
        result
    }
}
fn session_paths_at(root: &Path, now_ms: u64) -> Vec<PathBuf> {
    let mut paths = vec![];
    let today = (now_ms / 86_400_000) as i64;
    // Directory dates are local, timestamps are UTC. Cover both sides of the UTC day,
    // then enforce the actual rolling three-day interval using session timestamps.
    for day in (today - 3)..=(today + 1) {
        let dir = root.join(time::date_path(day));
        // Do not follow directory or file symlinks outside the supplied session tree.
        let mut component = root.to_path_buf();
        let safe = Path::new(&time::date_path(day)).components().all(|c| {
            component.push(c);
            fs::symlink_metadata(&component).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        });
        if !safe { continue; }
        let Ok(entries) = fs::read_dir(dir) else { continue; };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_file()) { continue; }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") { paths.push(entry.path()); }
        }
    }
    paths
}
fn normalize(path: &str) -> PathBuf {
    // canonicalize resolves /tmp -> /private/tmp on macOS. Nonexistent paths retain their spelling.
    fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}
fn read_meta(path: &Path) -> Option<SessionInfo> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take((MAX_LINE + 1) as u64));
    let mut line = vec![];
    reader.read_until(b'\n', &mut line).ok()?;
    if line.last() == Some(&b'\n') { line.pop(); }
    if line.len() > MAX_LINE { return None; }
    let value: Value = serde_json::from_slice(&line).ok()?;
    if value["type"] != "session_meta" { return None; }
    let p = &value["payload"];
    let id = s(&p["id"]);
    if !super::valid_thread_id(id) { return None; }
    let cwd = p["cwd"].as_str()?.to_owned();
    let started_at_ms = time::parse_ms(s(&p["timestamp"])).or_else(|| time::parse_ms(s(&value["timestamp"])))?;
    Some(SessionInfo { thread_id: id.into(), cwd, originator: p["originator"].as_str().map(str::to_owned), started_at_ms, path: path.to_owned() })
}
