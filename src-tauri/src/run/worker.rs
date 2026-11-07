use super::{Config, Emit, RunInfo, RunStatus, config::Launch, emit_status, write_info};
use crate::events::{claude::ClaudeParser, model::{Agent, EventKind, RunEvent, VerdictOutcome}, summary::RunSummary,
    tail::MAX_LINE, time, watch::{WatchPoller, WatchUpdate}};
use serde::{Deserialize, Serialize};
use std::{fs::{File, OpenOptions}, io::{BufRead, BufReader, Read, Write}, os::unix::process::CommandExt,
    path::Path, process::{Child, Command, Stdio}, sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc},
    thread::{self, JoinHandle}, time::{Duration, Instant}};

const MAX_STDERR: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(90 * 60);
const GRACE: Duration = Duration::from_secs(3);

/// Reset records are sanitized control events. Keeping them in the journal makes live/replay
/// agree after a rollout is replaced, without storing any raw stream or rewriting past events.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Record { Event(RunEvent), Reset { reset_source_id: String } }
struct Journal { file: File, summary: RunSummary, result: Option<bool> }
impl Journal {
    fn update(&mut self, update: WatchUpdate, id: &str, emit: &Emit) -> Result<(), String> {
        let (record, name, payload) = match update {
            WatchUpdate::Event(event) => {
                self.summary.apply(&event);
                if let EventKind::RunResult { status, .. } = &event.kind { self.result = Some(status == "success"); }
                let payload = serde_json::json!({"watchId": id, "event": event});
                (Record::Event(event), "run-event", payload)
            }
            WatchUpdate::Reset(source) => {
                self.summary.remove_source(Agent::Codex, &source);
                let payload = serde_json::json!({"watchId": id, "sourceId": source});
                (Record::Reset { reset_source_id: source }, "run-reset", payload)
            }
        };
        let mut bytes = serde_json::to_vec(&record).map_err(|_| "无法编码任务事件")?;
        bytes.push(b'\n');
        self.file.write_all(&bytes).map_err(|_| "无法保存任务事件")?;
        emit(name, payload);
        Ok(())
    }
}
pub(super) fn load_events(path: &Path) -> Result<Vec<RunEvent>, String> {
    let file = File::open(path).map_err(|_| "无法读取任务事件")?;
    let mut events: Vec<RunEvent> = Vec::new();
    // A crash can leave a partial final record; ignore only that unfinished line.
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes).map_err(|_| "无法读取任务事件")? == 0 { break; }
        if bytes.last() != Some(&b'\n') { break; }
        match serde_json::from_slice::<Record>(&bytes).map_err(|_| "任务事件格式错误")? {
            Record::Event(event) => events.push(event),
            Record::Reset { reset_source_id } => events.retain(|e| e.agent != Agent::Codex || e.source_id != reset_source_id),
        }
    }
    Ok(events)
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        signal(self.0.id(), "-KILL");
        let _ = self.0.wait();
    }
}
enum Frame { Line(String, u64), Closed(bool) }
pub(super) fn start(config: &Config, info: &RunInfo, launch: Launch, args: Vec<String>, stop: Arc<AtomicBool>, emit: Emit) -> Result<JoinHandle<()>, String> {
    start_with_timeout(config, info, launch, args, stop, emit, TIMEOUT)
}
pub(super) fn start_with_timeout(config: &Config, info: &RunInfo, launch: Launch, args: Vec<String>, stop: Arc<AtomicBool>, emit: Emit, max_runtime: Duration) -> Result<JoinHandle<()>, String> {
    let dir = config.data.join("runs").join(&info.run_id);
    let stderr_file = File::create(dir.join("claude.stderr")).map_err(|_| "无法创建 Claude 错误日志")?;
    let file = OpenOptions::new().create_new(true).append(true).open(dir.join("events.jsonl")).map_err(|_| "无法创建任务事件日志")?;
    let mut child = Process(Command::new(&launch.executable).args(args).current_dir(&info.worktree)
        .env_clear().envs(&launch.variables).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .process_group(0).spawn().map_err(|_| "无法启动 Claude，请检查可执行文件与权限")?);
    let stdout = child.0.stdout.take().ok_or("无法打开 Claude 输出")?;
    let stderr = child.0.stderr.take().ok_or("无法打开 Claude 错误输出")?;
    let (tx, rx) = mpsc::sync_channel(4);
    let stdout_thread = thread::Builder::new().name("run-stdout".into()).spawn(move || read_stdout(stdout, tx)).map_err(|_| "无法启动输出读取线程")?;
    let stderr_thread = thread::Builder::new().name("run-stderr".into()).spawn(move || cap_stderr(stderr, stderr_file)).map_err(|_| "无法启动错误日志线程")?;
    write_info(&dir, info)?;
    emit_status(&emit, info, None);
    let mut info = info.clone();
    let sessions = config.sessions.clone();
    thread::Builder::new().name(info.run_id.clone()).spawn(move || {
        let mut journal = Journal { file, summary: RunSummary::default(), result: None };
        let mut parser = ClaudeParser::new();
        let mut poller = WatchPoller::new(sessions, &info.worktree.to_string_lossy()).since(info.started_at_ms);
        let started = Instant::now();
        let mut exit = None;
        let mut exited_at = None;
        let mut stopped = false;
        let mut termination_started = false;
        let mut closed = false;
        let mut error = None;
        let mut detail = None;
        let mut last_poll = Instant::now() - Duration::from_secs(1);
        loop {
            let timed_out = started.elapsed() >= max_runtime;
            if !termination_started && (stop.load(Ordering::Acquire) || timed_out || error.is_some()) {
                termination_started = true;
                stopped = stop.load(Ordering::Acquire) || timed_out;
                if timed_out { detail = Some("任务超过 90 分钟，已自动停止"); }
                terminate(&mut child.0);
                cancel_companion(&launch, &info.worktree);
                exit = child.0.try_wait().ok().flatten();
                exited_at = Some(Instant::now());
            }
            let mut changed = false;
            for _ in 0..64 {
                match rx.try_recv() {
                    Ok(Frame::Line(line, received)) => {
                        for mut event in parser.feed_line_at(&line, received) {
                            if event.source_id.is_empty() { event.source_id = info.run_id.clone(); }
                            if let Err(e) = journal.update(WatchUpdate::Event(event), &info.run_id, &emit) { error = Some(e); }
                            changed = true;
                        }
                    }
                    Ok(Frame::Closed(ok)) => { closed = true; if !ok { error = Some("读取 Claude 输出失败".into()); } }
                    Err(mpsc::TryRecvError::Disconnected) => { closed = true; break; }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if last_poll.elapsed() >= Duration::from_millis(400) || exited_at.is_some_and(|at: Instant| at.elapsed() >= GRACE) {
                for update in poller.poll_updates(Instant::now(), time::now_ms(), || false) {
                    if let Err(e) = journal.update(update, &info.run_id, &emit) { error = Some(e); }
                    changed = true;
                }
                last_poll = Instant::now();
            }
            if changed { emit("run-summary", serde_json::json!({"watchId": info.run_id, "summary": journal.summary})); }
            if exited_at.is_none() {
                match child.0.try_wait() {
                    Ok(Some(status)) => { exit = Some(status); exited_at = Some(Instant::now()); }
                    Ok(None) => {},
                    Err(_) => { error = Some("无法获取 Claude 退出状态".into()); }
                }
            }
            if exited_at.is_some_and(|at| at.elapsed() >= GRACE) {
                // A descendant can retain the pipe after Claude exits. Reap that process group
                // before joining readers; never let inherited pipes hang stop or app shutdown.
                if !closed { signal(child.0.id(), "-KILL"); }
                if closed { break; }
            }
            thread::sleep(Duration::from_millis(25));
        }
        drop(child);
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        info.status = if stopped { RunStatus::Stopped }
            else if error.is_some() || !exit.is_some_and(|s| s.success()) || journal.result.is_none() { RunStatus::Failed }
            else if journal.result == Some(true) && journal.summary.last_verdict == Some(VerdictOutcome::Pass) { RunStatus::Passed }
            else { RunStatus::NeedsAttention };
        if let Err(e) = journal.file.sync_data() { let _ = e; error = Some("无法同步任务事件日志".into()); info.status = RunStatus::Failed; }
        if write_info(&dir, &info).is_err() { info.status = RunStatus::Failed; error = Some("无法保存任务最终状态".into()); }
        emit_status(&emit, &info, error.as_deref().or(detail));
    }).map_err(|_| "无法启动任务监控线程".into())
}
fn read_stdout(mut reader: impl Read, tx: mpsc::SyncSender<Frame>) {
    let mut buf = [0u8; 8192];
    let mut pending = Vec::new();
    let mut dropping = false;
    let ok = loop {
        let size = match reader.read(&mut buf) { Ok(0) => break true, Ok(n) => n, Err(_) => break false };
        for &byte in &buf[..size] {
            if byte == b'\n' {
                if !dropping && tx.send(Frame::Line(String::from_utf8_lossy(&pending).into_owned(), time::now_ms())).is_err() { return; }
                pending.clear(); dropping = false;
            } else if !dropping {
                if pending.len() == MAX_LINE { pending.clear(); dropping = true; } else { pending.push(byte); }
            }
        }
    };
    if !dropping && !pending.is_empty() { let _ = tx.send(Frame::Line(String::from_utf8_lossy(&pending).into_owned(), time::now_ms())); }
    let _ = tx.send(Frame::Closed(ok));
}
fn cap_stderr(mut reader: impl Read, mut file: File) {
    let mut buf = [0u8; 8192];
    let mut kept = 0;
    while let Ok(n) = reader.read(&mut buf) {
        if n == 0 { break; }
        let keep = n.min(MAX_STDERR - kept);
        if keep > 0 { let _ = file.write_all(&buf[..keep]); kept += keep; }
        // Continue draining after the cap so the child can never block on a full stderr pipe.
    }
}
fn signal(pgid: u32, signal: &str) -> bool {
    Command::new("/bin/kill").args([signal, &format!("-{pgid}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().is_ok_and(|s| s.success())
}
fn terminate(child: &mut Child) {
    let pgid = child.id();
    signal(pgid, "-TERM");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let _ = child.try_wait();
        if !signal(pgid, "-0") { break; }
        thread::sleep(Duration::from_millis(50));
    }
    if signal(pgid, "-0") { signal(pgid, "-KILL"); }
    let _ = child.wait();
}
fn cancel_companion(launch: &Launch, cwd: &Path) {
    let Ok(mut child) = Command::new(&launch.node).arg(&launch.companion).arg("cancel").current_dir(cwd)
        .env_clear().envs(&launch.variables).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn() else { return; };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if child.try_wait().ok().flatten().is_some() { signal(child.id(), "-KILL"); return; }
        thread::sleep(Duration::from_millis(50));
    }
    signal(child.id(), "-KILL");
    let _ = child.wait();
}
