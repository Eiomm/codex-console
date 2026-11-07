pub mod claude;
pub mod codex;
mod common;
pub mod discover;
pub mod model;
pub mod redact;
pub mod summary;
pub mod tail;
pub(crate) mod time;
pub mod watch;

use codex::CodexParser;
use model::RunEvent;
use serde::Serialize;
use std::path::Path;
use summary::RunSummary;
use tail::Tailer;

pub fn valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub fn validate_thread_id(id: &str) -> Result<(), String> {
    if valid_thread_id(id) { Ok(()) } else { Err("threadId 只能包含字母、数字和连字符".into()) }
}
#[derive(Debug, Serialize)]
pub struct LoadedSession { pub events: Vec<RunEvent>, pub summary: RunSummary, pub skipped: u64 }
/// Injectable root/clock: tests never resolve or read the user's real session directory.
pub fn load_codex_session_at(root: &Path, thread_id: &str, now_ms: u64) -> Result<LoadedSession, String> {
    validate_thread_id(thread_id)?; // Before any filesystem operation.
    let info = discover::find_sessions_at(root, None, 0, now_ms).into_iter()
        .find(|s| s.thread_id == thread_id).ok_or("未找到最近三天内的会话")?;
    let mut parser = CodexParser::new();
    let mut tail = Tailer::new();
    let mut result = LoadedSession { events: vec![], summary: RunSummary::default(), skipped: 0 };
    loop {
        let batch = tail.poll(&info.path).map_err(|_| "无法读取会话记录")?;
        if batch.reset {
            parser = CodexParser::new(); result.events.clear(); result.summary = RunSummary::default(); result.skipped = 0;
        }
        result.skipped += batch.skipped;
        for line in batch.lines {
            for event in parser.feed_line(&line) { result.summary.apply(&event); result.events.push(event); }
        }
        if !batch.has_more { break; }
    }
    if let Some(line) = tail.finish() {
        for event in parser.feed_line(&line) { result.summary.apply(&event); result.events.push(event); }
    }
    result.skipped += parser.skipped;
    Ok(result)
}
pub async fn list_sessions(cwd: Option<String>) -> Result<Vec<discover::SessionInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = discover::default_root().ok_or("无法确定会话目录")?;
        Ok(discover::find_sessions(&root, cwd.as_deref(), 0))
    }).await.map_err(|_| "会话扫描中断")?
}
pub async fn load_session(thread_id: String) -> Result<LoadedSession, String> {
    validate_thread_id(&thread_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let root = discover::default_root().ok_or("无法确定会话目录")?;
        load_codex_session_at(&root, &thread_id, time::now_ms())
    }).await.map_err(|_| "会话读取中断")?
}

#[cfg(test)]
mod tests;
