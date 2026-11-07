use super::{codex::CodexParser, discover::DiscoveryCache, model::{Agent, RunEvent}, summary::RunSummary, tail::Tailer, time};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf, sync::{mpsc, Mutex}, thread::{self, JoinHandle}, time::{Duration, Instant}};

const POLL_INTERVAL: Duration = Duration::from_millis(400);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchEvent { pub watch_id: String, pub event: RunEvent }
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSummary<'a> { pub watch_id: &'a str, pub summary: &'a RunSummary }
/// Summary borrows the reducer only while emitting, avoiding a clone of its private history.
pub enum WatchNotification<'a> { Event(WatchEvent), Summary(WatchSummary<'a>), Reset(WatchReset) }
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchReset { pub watch_id: String, pub source_id: String }
pub(crate) enum WatchUpdate { Event(RunEvent), Reset(String) }

struct WatchedFile { tail: Tailer, parser: CodexParser, source_id: String }
pub(crate) struct WatchPoller {
    root: PathBuf,
    since_ms: u64,
    discovery: DiscoveryCache,
    files: BTreeMap<PathBuf, WatchedFile>,
    last_scan: Option<Instant>,
    summary: RunSummary,
}
impl WatchPoller {
    pub(crate) fn new(root: PathBuf, cwd: &str) -> Self {
        Self { root, since_ms: 0, discovery: DiscoveryCache::new(Some(cwd)), files: BTreeMap::new(), last_scan: None, summary: RunSummary::default() }
    }
    pub(crate) fn since(mut self, since_ms: u64) -> Self { self.since_ms = since_ms; self }
    /// Shared discovery/tailing core. The automatic runner owns its combined summary.
    pub(crate) fn poll_updates<C>(&mut self, now: Instant, now_ms: u64, cancelled: C) -> Vec<WatchUpdate>
    where C: Fn() -> bool {
        let mut updates = Vec::new();
        if cancelled() { return updates; }
        if self.last_scan.is_none_or(|last| now.duration_since(last) >= DISCOVERY_INTERVAL) {
            for info in self.discovery.find_new_at(&self.root, self.since_ms, now_ms) {
                self.files.insert(info.path, WatchedFile { tail: Tailer::new(), parser: CodexParser::new(), source_id: info.thread_id });
            }
            self.last_scan = Some(now);
        }
        for (path, file) in &mut self.files {
            if cancelled() { break; }
            let Ok(batch) = file.tail.poll(path) else { continue; };
            if batch.reset {
                file.parser = CodexParser::new();
                updates.push(WatchUpdate::Reset(file.source_id.clone()));
            }
            file.parser.skipped += batch.skipped;
            for line in batch.lines {
                for event in file.parser.feed_line(&line) {
                    file.source_id = event.source_id.clone();
                    updates.push(WatchUpdate::Event(event));
                }
            }
        }
        updates
    }
    /// Injectable clocks keep cadence and notification tests deterministic, using only temp files.
    pub(super) fn poll_at<F, C>(&mut self, watch_id: &str, now: Instant, now_ms: u64, emit: &F, cancelled: C) -> bool
    where F: for<'a> Fn(WatchNotification<'a>), C: Fn() -> bool {
        let updates = self.poll_updates(now, now_ms, &cancelled);
        let changed = !updates.is_empty();
        for update in updates {
            if cancelled() { return false; }
            match update {
                WatchUpdate::Reset(source_id) => {
                    self.summary.remove_source(Agent::Codex, &source_id);
                    emit(WatchNotification::Reset(WatchReset { watch_id: watch_id.into(), source_id }));
                }
                WatchUpdate::Event(event) => {
                    self.summary.apply(&event);
                    emit(WatchNotification::Event(WatchEvent { watch_id: watch_id.into(), event }));
                }
            }
        }
        if cancelled() { return false; }
        if changed { emit(WatchNotification::Summary(WatchSummary { watch_id, summary: &self.summary })); }
        true
    }
}
struct Worker { stop: mpsc::Sender<()>, handle: JoinHandle<()> }
#[derive(Default)]
struct State { next_id: u64, stopped: bool, workers: BTreeMap<String, Worker> }
#[derive(Default)]
pub struct WatchManager { state: Mutex<State> }
impl WatchManager {
    /// Event-only adapter retained for existing callers; the worker still maintains its summary.
    pub fn start<F>(&self, root: PathBuf, cwd: String, emit: F) -> Result<String, String>
    where F: Fn(WatchEvent) + Send + 'static {
        self.start_with_notifications(root, cwd, move |notification| {
            if let WatchNotification::Event(event) = notification { emit(event); }
        })
    }
    /// No I/O on the calling thread; directory scanning and tailing run in a dedicated worker.
    pub fn start_with_notifications<F>(&self, root: PathBuf, cwd: String, emit: F) -> Result<String, String>
    where F: for<'a> Fn(WatchNotification<'a>) + Send + 'static {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.stopped { return Err("应用正在退出".into()); }
        state.next_id += 1;
        let id = format!("watch-{}", state.next_id);
        let watch_id = id.clone();
        let (stop, rx) = mpsc::channel();
        let handle = thread::Builder::new().name(id.clone()).spawn(move || {
            let mut poller = WatchPoller::new(root, &cwd);
            loop {
                if !poller.poll_at(&watch_id, Instant::now(), time::now_ms(), &emit,
                    || rx.try_recv() != Err(mpsc::TryRecvError::Empty)) { break; }
                if rx.recv_timeout(POLL_INTERVAL) != Err(mpsc::RecvTimeoutError::Timeout) { break; }
            }
        }).map_err(|_| "无法启动会话跟踪")?;
        state.workers.insert(id.clone(), Worker { stop, handle });
        Ok(id)
    }
    pub fn stop(&self, id: &str) {
        let worker = self.state.lock().unwrap_or_else(|p| p.into_inner()).workers.remove(id);
        if let Some(worker) = worker { let _ = worker.stop.send(()); let _ = worker.handle.join(); }
    }
    pub fn shutdown(&self) {
        let workers = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.stopped = true;
            std::mem::take(&mut state.workers)
        };
        for worker in workers.values() { let _ = worker.stop.send(()); }
        for (_, worker) in workers { let _ = worker.handle.join(); }
    }
}
impl Drop for WatchManager { fn drop(&mut self) { self.shutdown(); } }
