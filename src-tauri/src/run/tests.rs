use super::*;
use crate::events::model::{Agent, EventKind, VerdictOutcome};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, process::{Command, Stdio}, sync::atomic::AtomicU64, thread, time::{Duration, Instant}};

const PASS: &str = include_str!("../fixtures/events/claude_pass.jsonl");
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("run-tests-{}-{}-{}", std::process::id(), time::now_ms(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn executable(path: &Path, text: &str) { fs::write(path, text).unwrap(); fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap(); }
fn init_repo(path: &Path, commit: bool) {
    fs::create_dir_all(path).unwrap();
    git::run(path, &["init"]).unwrap();
    git::run(path, &["config", "user.name", "Test"]).unwrap();
    git::run(path, &["config", "user.email", "test@example.invalid"]).unwrap();
    git::run(path, &["config", "commit.gpgsign", "false"]).unwrap();
    git::run(path, &["config", "core.hooksPath", "/dev/null"]).unwrap();
    fs::write(path.join("file.txt"), "base\n").unwrap();
    if commit { commit_all(path); }
}
fn commit_all(path: &Path) { git::run(path, &["add", "-A"]).unwrap(); git::run(path, &["commit", "-m", "test"]).unwrap(); }
fn head(path: &Path) -> String { git::run(path, &["rev-parse", "HEAD"]).unwrap() }
fn request(path: &Path) -> StartRequest { StartRequest { repo: path.to_string_lossy().into_owned(), goal: "测试任务".into(), verify_commands: vec!["cargo test --offline".into()], model: None } }
struct Harness { manager: RunManager, repo: PathBuf, notices: Arc<Mutex<Vec<(String, Value)>>>, _temp: Temp }
impl Harness {
    fn new(script: &str) -> Self {
        let tmp = Temp::new();
        let home = tmp.0.join("home"); let bin = home.join(".local/bin"); fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("fake-claude");
        executable(&claude, &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKE_ARGS\"\n{script}\n"));
        executable(&bin.join("node"), "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$FAKE_CANCEL\"\nexit 0\n");
        let companion = tmp.0.join("companion with space.mjs"); fs::write(&companion, "fake").unwrap();
        fs::write(tmp.0.join("stream"), PASS).unwrap();
        let variables: BTreeMap<OsString, OsString> = [
            ("PATH", "/usr/bin:/bin".into()), ("HOME", home.as_os_str().to_owned()),
            ("CODEX_CONSOLE_DATA", tmp.0.join("data").into_os_string()),
            ("CODEX_CONSOLE_CLAUDE_BIN", claude.into_os_string()),
            ("CODEX_CONSOLE_COMPANION", companion.into_os_string()),
            ("FAKE_STREAM", tmp.0.join("stream").into_os_string()),
            ("FAKE_ARGS", tmp.0.join("args").into_os_string()),
            ("FAKE_CANCEL", tmp.0.join("cancel").into_os_string()),
        ].into_iter().map(|(k,v)| (k.into(), v)).collect();
        let notices = Arc::new(Mutex::new(Vec::new())); let captured = notices.clone();
        let manager = RunManager::new(Config::from_variables(Some(home), variables).unwrap(), Arc::new(move |name, value| captured.lock().unwrap().push((name.into(), value)))).unwrap();
        let repo = tmp.0.join("repo"); init_repo(&repo, true);
        Self { manager, repo, notices, _temp: tmp }
    }
    fn start(&self) -> RunInfo { self.manager.start_run(request(&self.repo)).unwrap() }
    fn done(&self, id: &str) -> RunInfo {
        let start = Instant::now();
        loop {
            let info = self.manager.read_info(id).unwrap();
            if info.status != RunStatus::Running { return info; }
            assert!(start.elapsed() < Duration::from_secs(20), "任务未及时退出");
            thread::sleep(Duration::from_millis(30));
        }
    }
    fn record(&self, status: RunStatus) -> RunInfo {
        let id = "run-test-1";
        let info = RunInfo { run_id: id.into(), repo: self.repo.clone(), worktree: self.manager.config.data.join("worktrees").join(id), branch: format!("console/{id}"), base: head(&self.repo), status, started_at_ms: 1, goal: "合并测试\n第二行".into() };
        git::run(&self.repo, &["worktree", "add", "-b", &info.branch, path_str(&info.worktree).unwrap(), "HEAD"]).unwrap();
        let dir = self.manager.run_dir(id).unwrap(); fs::create_dir(&dir).unwrap(); write_info(&dir, &info).unwrap(); fs::write(dir.join("events.jsonl"), "").unwrap();
        info
    }
}

#[test]
fn args_have_exact_permissions_and_optional_model_budget() {
    let commands = vec!["cargo test --offline".into(), "npm run check".into()];
    let args = build_claude_args("goal", "/tmp/plugin path/script.mjs", "/Users/x/Library/Application Support/run/task.md", &commands, &ClaudeOptions::default());
    assert_eq!(&args[..8], ["-p", "goal", "--output-format", "stream-json", "--verbose", "--strict-mcp-config", "--max-budget-usd", "10"]);
    assert_eq!(&args[8..], ["--add-dir", "/Users/x/Library/Application Support/run", "--allowedTools", "Read", "Grep", "Glob", "Edit(//Users/x/Library/Application Support/run/task.md)", "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)", "Bash(node \"/tmp/plugin path/script.mjs\":*)", "Bash(node /tmp/plugin path/script.mjs:*)", "Bash(cargo test --offline:*)", "Bash(npm run check:*)"]);
    for bad in ["Bash", "Edit", "Write", "NotebookEdit", "Agent", "--dangerously-skip-permissions", "bypassPermissions"] { assert!(!args.iter().any(|a| a == bad)); }
    let args = build_claude_args("p", "/script", "/run/task.md", &[], &ClaudeOptions { model: Some("model".into()), max_budget_usd: Some(4.0) });
    assert!(args.windows(2).any(|a| a == ["--model", "model"]));
    assert!(args.windows(2).any(|a| a == ["--max-budget-usd", "4"]));
}
#[test]
fn validates_goal_commands_repo_and_ids() {
    let tmp = Temp::new(); let mut req = request(&tmp.0);
    for forbidden in [';', '|', '&', '`', '$', '<', '>', '\n', '\r', '\\'] {
        req.verify_commands = vec![format!("echo a{forbidden}b")]; assert!(validate_request(&req).is_err(), "{forbidden}");
    }
    for invalid in [" ".into(), "a".repeat(201)] { req.verify_commands = vec![invalid]; assert!(validate_request(&req).is_err()); }
    req.verify_commands.clear();
    for invalid in [" \n ".into(), "中".repeat(20001)] { req.goal = invalid; assert!(validate_request(&req).is_err()); }
    req.goal = " 中 ".into(); assert_eq!(validate_request(&req).unwrap(), "中");
    assert!(git::validate_repo(path_str(&tmp.0).unwrap()).is_err());
    let repo = tmp.0.join("repo"); init_repo(&repo, false);
    assert!(git::validate_repo(path_str(&repo).unwrap()).is_err()); commit_all(&repo);
    let child = repo.join("sub"); fs::create_dir(&child).unwrap(); assert!(git::validate_repo(path_str(&child).unwrap()).is_err());
    assert!(git::validate_repo(path_str(&repo).unwrap()).is_ok());
    for id in ["", "../x", "a/b", "a.b", "中", "a\\b"] { assert!(validate_id(id).is_err()); }
}
#[test]
fn unsafe_task_paths_are_rejected_before_creating_a_run() {
    let mut h = Harness::new("exit 0");
    let original_data = h.manager.config.data.clone();
    for forbidden in ['"', '\'', '`', ';', '|', '&', '$', '<', '>', '\\', '\n', '\r', '*', '?', '(', ')'] {
        h.manager.config.data = h._temp.0.join(format!("bad{forbidden}data"));
        let error = h.manager.start_run(request(&h.repo)).unwrap_err();
        assert!(error.contains("任务书路径"), "{forbidden:?}: {error}");
        assert!(!h.manager.config.data.exists());
    }
    assert!(validate_task_file(Path::new("relative/task.md")).is_err());
    assert_eq!(fs::read_dir(original_data.join("runs")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(original_data.join("worktrees")).unwrap().count(), 0);
    assert!(git::run(&h.repo, &["branch", "--list", "console/*"]).unwrap().is_empty());
    assert!(!h._temp.0.join("args").exists());
}
#[test]
fn start_passes_exact_task_file_and_directory_with_spaces() {
    let mut h = Harness::new("cat \"$FAKE_STREAM\"");
    let mut config = h.manager.config.clone();
    config.data = h._temp.0.join("Library/Application Support/console");
    h.manager = RunManager::new(config, h.manager.emit.clone()).unwrap();
    let mut request = request(&h.repo);
    request.goal = "任务含全角空格：甲\u{3000}乙".into();
    let info = h.manager.start_run(request).unwrap();
    assert_eq!(h.done(&info.run_id).status, RunStatus::Passed);
    let dir = h.manager.run_dir(&info.run_id).unwrap();
    let task_file = dir.join("task.md");
    let args = fs::read_to_string(h._temp.0.join("args")).unwrap();
    assert!(args.contains(&format!("--add-dir\n{}\n--allowedTools\n", dir.display())));
    assert!(args.contains(&format!("Edit(/{})\n", task_file.display())));
    assert!(args.contains(&format!("--prompt-file \"{}\" --json", task_file.display())));
    assert!(!args.contains("{{TASKFILE}}"));
    assert!(!task_file.starts_with(&info.worktree));
}
#[test]
fn launch_version_selection_errors_path_and_prompt() {
    let h = Harness::new("exit 0");
    let mut config = h.manager.config.clone();
    let root = config.home.as_ref().unwrap().join(".claude/plugins/cache/openai-codex/codex");
    config.variables.remove(std::ffi::OsStr::new("CODEX_CONSOLE_COMPANION"));
    assert!(config.launch().err().unwrap().contains("插件脚本"));
    for version in ["0.9.0", "0.10.0", "0.10.0-beta", "bad"] { let dir = root.join(version).join("scripts"); fs::create_dir_all(&dir).unwrap(); fs::write(dir.join("codex-companion.mjs"), "fake").unwrap(); }
    let launch = config.launch().unwrap(); assert!(launch.companion.to_string_lossy().contains("/0.10.0/"));
    assert_eq!(launch.variables[std::ffi::OsStr::new("HTTP_PROXY")], "http://127.0.0.1:7897");
    let bin = config.home.as_ref().unwrap().join(".local/bin");
    fs::copy(bin.join("fake-claude"), bin.join("claude")).unwrap();
    config.variables.remove(std::ffi::OsStr::new("CODEX_CONSOLE_CLAUDE_BIN"));
    assert_eq!(config.launch().unwrap().executable, bin.join("claude"));
    let output = config::prompt("goal {{BASE}} {{TASKFILE}}", "branch", "sha", "/script", "/run with spaces/task.md", &[]);
    assert!(output.contains("goal {{BASE}} {{TASKFILE}}")); assert!(output.contains("（未指定，只做读代码审查）")); assert!(output.contains("node \"/script\" review --background --json"));
    assert!(output.contains("--prompt-file \"/run with spaces/task.md\" --json"));
    assert_eq!(output.matches("{{TASKFILE}}").count(), 1);
}
#[test]
fn fake_claude_passed_isolated_events_persisted_and_replayed() {
    let h = Harness::new("cat \"$FAKE_STREAM\"");
    let original = head(&h.repo); let branch = git::run(&h.repo, &["branch", "--show-current"]).unwrap();
    let info = h.start();
    assert!(info.worktree.is_dir()); assert_eq!(head(&info.worktree), original);
    assert_eq!(h.done(&info.run_id).status, RunStatus::Passed);
    assert_eq!(head(&h.repo), original); assert_eq!(fs::read_to_string(h.repo.join("file.txt")).unwrap(), "base\n");
    assert_eq!(git::run(&h.repo, &["branch", "--show-current"]).unwrap(), branch);
    let loaded = h.manager.load_run(&info.run_id).unwrap();
    assert_eq!(loaded.summary.last_verdict, Some(VerdictOutcome::Pass));
    assert!(loaded.events.iter().any(|e| matches!(e.kind, EventKind::RunResult { .. })));
    let log = fs::read_to_string(h.manager.run_dir(&info.run_id).unwrap().join("events.jsonl")).unwrap();
    for forbidden in ["signature", "base_instructions", "private"] { assert!(!log.contains(forbidden)); }
    let notices = h.notices.lock().unwrap();
    let emitted: Vec<_> = notices.iter().filter(|(n,_)| n == "run-event").map(|(_,v)| v["event"].clone()).collect();
    assert_eq!(emitted, loaded.events.iter().map(|e| serde_json::to_value(e).unwrap()).collect::<Vec<_>>());
    assert!(notices.iter().any(|(n,v)| n == "run-summary" && v["watchId"] == info.run_id));
    assert!(notices.iter().any(|(n,v)| n == "run-status" && v["status"] == "passed"));
    assert_eq!(h.manager.list_runs().unwrap()[0].run_id, info.run_id);
}
#[test]
fn normal_missing_verdict_error_result_and_abnormal_exit() {
    for (stream, code, status) in [
        ("{\"type\":\"result\",\"subtype\":\"success\"}\n", 0, RunStatus::NeedsAttention),
        ("{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true}\n", 0, RunStatus::NeedsAttention),
        ("", 2, RunStatus::Failed), ("", 0, RunStatus::Failed), (PASS, 2, RunStatus::Failed),
    ] {
        let h = Harness::new(&format!("cat \"$FAKE_STREAM\"\nexit {code}"));
        fs::write(h._temp.0.join("stream"), stream).unwrap();
        let info = h.start(); assert_eq!(h.done(&info.run_id).status, status);
    }
}
#[test]
fn stop_process_group_single_active_shutdown_and_cancel() {
    let h = Harness::new("sleep 120 &\nkid=$!\ntrap 'kill \"$kid\" 2>/dev/null; wait \"$kid\" 2>/dev/null; exit 0' TERM\nprintf '%s %s' \"$$\" \"$kid\" > pids\nwait \"$kid\"");
    let info = h.start();
    assert!(h.manager.start_run(request(&h.repo)).unwrap_err().contains("已有任务"));
    assert!(h.manager.discard_run(&info.run_id).is_err());
    wait_file(&info.worktree.join("pids"));
    let pids = fs::read_to_string(info.worktree.join("pids")).unwrap();
    h.manager.stop_run(&info.run_id).unwrap();
    assert_eq!(h.manager.read_info(&info.run_id).unwrap().status, RunStatus::Stopped);
    assert!(!alive(&format!("-{}", pids.split_whitespace().next().unwrap())));
    for pid in pids.split_whitespace() { assert!(!alive(pid)); }
    let cancel = fs::read_to_string(h._temp.0.join("cancel")).unwrap(); assert!(cancel.contains(path_str(&info.worktree).unwrap())); assert!(cancel.ends_with("cancel\n"));
    let next = h.start(); wait_file(&next.worktree.join("pids")); h.manager.shutdown();
    assert_eq!(h.manager.read_info(&next.run_id).unwrap().status, RunStatus::Stopped);
    assert!(h.manager.start_run(request(&h.repo)).is_err());
}
fn wait_file(path: &Path) {
    let start = Instant::now(); while !path.exists() || fs::metadata(path).unwrap().len() == 0 { assert!(start.elapsed() < Duration::from_secs(5)); thread::sleep(Duration::from_millis(20)); }
}
fn alive(pid: &str) -> bool { Command::new("/bin/kill").args(["-0", pid]).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success() }

#[test]
fn merge_clean_preserves_branch_no_push_and_commit_message() {
    let h = Harness::new("exit 0"); let info = h.record(RunStatus::Passed);
    fs::write(info.worktree.join("file.txt"), "changed\n").unwrap();
    // A push would fail; successful merge never needs or contacts this remote.
    git::run(&h.repo, &["remote", "add", "origin", "/nonexistent-local-test-remote"]).unwrap();
    let before = head(&h.repo);
    assert_eq!(h.manager.merge_run(&info.run_id).unwrap().outcome, "merged");
    assert_ne!(head(&h.repo), before); assert!(!info.worktree.exists());
    assert!(git::run(&h.repo, &["rev-parse", &info.branch]).is_ok());
    let message = git::run(&h.repo, &["log", "-1", "--format=%B", &info.branch]).unwrap();
    assert!(message.starts_with("console: 合并测试\n\n")); assert!(message.ends_with("Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"));
    assert_eq!(h.manager.read_info(&info.run_id).unwrap().status, RunStatus::Merged);
    assert!(h.manager.discard_run(&info.run_id).is_err());
}
#[test]
fn merge_dirty_commits_only_preserves_user_changes_and_can_retry() {
    let h = Harness::new("exit 0"); let info = h.record(RunStatus::Passed);
    fs::write(info.worktree.join("new.txt"), "implemented\n").unwrap();
    fs::write(h.repo.join("file.txt"), "user change\n").unwrap();
    fs::write(h.repo.join("untracked"), "keep").unwrap();
    let before = head(&h.repo); let dirty = git::run(&h.repo, &["status", "--porcelain"]).unwrap();
    let result = h.manager.merge_run(&info.run_id).unwrap(); assert_eq!(result.outcome, "committedOnly"); assert_eq!(result.branch, info.branch);
    assert_eq!(head(&h.repo), before); assert_eq!(git::run(&h.repo, &["status", "--porcelain"]).unwrap(), dirty);
    assert_eq!(fs::read_to_string(h.repo.join("file.txt")).unwrap(), "user change\n"); assert_eq!(fs::read_to_string(h.repo.join("untracked")).unwrap(), "keep");
    assert_ne!(head(&info.worktree), info.base);
    // Remove only test-owned changes, then retry the already committed task.
    fs::write(h.repo.join("file.txt"), "base\n").unwrap(); fs::remove_file(h.repo.join("untracked")).unwrap();
    assert_eq!(h.manager.merge_run(&info.run_id).unwrap().outcome, "merged");
}
#[test]
fn merge_conflict_aborts_and_nonpassed_rejected_and_nothing() {
    let h = Harness::new("exit 0"); let mut info = h.record(RunStatus::NeedsAttention);
    assert!(h.manager.merge_run(&info.run_id).is_err());
    info.status = RunStatus::Passed; write_info(&h.manager.run_dir(&info.run_id).unwrap(), &info).unwrap();
    assert_eq!(h.manager.merge_run(&info.run_id).unwrap().outcome, "nothing");
    fs::write(info.worktree.join("file.txt"), "task\n").unwrap();
    fs::write(h.repo.join("file.txt"), "main\n").unwrap(); commit_all(&h.repo); let before = head(&h.repo);
    assert_eq!(h.manager.merge_run(&info.run_id).unwrap().outcome, "conflict");
    assert_eq!(head(&h.repo), before); assert_eq!(fs::read_to_string(h.repo.join("file.txt")).unwrap(), "main\n");
    assert!(git::run(&h.repo, &["status", "--porcelain"]).unwrap().is_empty());
    assert!(!git::output(&h.repo, &["rev-parse", "--verify", "MERGE_HEAD"]).unwrap().status.success());
}
#[test]
fn discard_rejects_tampered_paths_branches_symlinks_then_removes_owned() {
    let h = Harness::new("exit 0"); let info = h.record(RunStatus::Stopped);
    let dir = h.manager.run_dir(&info.run_id).unwrap();
    let outside = h._temp.0.join("outside"); fs::create_dir(&outside).unwrap(); fs::write(outside.join("keep"), "keep").unwrap();
    let mut bad = info.clone(); bad.worktree = outside.clone(); write_info(&dir, &bad).unwrap();
    assert!(h.manager.discard_run(&info.run_id).is_err()); assert!(outside.join("keep").exists()); assert!(info.worktree.exists());
    for branch in ["main", "console/some-other-run"] {
        bad = info.clone(); bad.branch = branch.into(); write_info(&dir, &bad).unwrap(); assert!(h.manager.discard_run(&info.run_id).is_err());
    }
    write_info(&dir, &info).unwrap();
    let moved = h._temp.0.join("moved"); fs::rename(&info.worktree, &moved).unwrap(); std::os::unix::fs::symlink(&outside, &info.worktree).unwrap();
    assert!(h.manager.discard_run(&info.run_id).is_err()); assert!(outside.join("keep").exists());
    fs::remove_file(&info.worktree).unwrap(); fs::rename(moved, &info.worktree).unwrap();
    h.manager.discard_run(&info.run_id).unwrap(); assert!(!info.worktree.exists());
    assert!(git::run(&h.repo, &["rev-parse", "--verify", &format!("refs/heads/{}", info.branch)]).is_err());
    assert_eq!(h.manager.read_info(&info.run_id).unwrap().status, RunStatus::Discarded);
    for id in ["../outside", "bad/id"] { assert!(h.manager.load_run(id).is_err()); }
}
#[test]
fn initialization_marks_interrupted_and_lists_newest_first() {
    let h = Harness::new("exit 0"); let info = h.record(RunStatus::Running);
    let manager = RunManager::new(h.manager.config.clone(), h.manager.emit.clone()).unwrap();
    assert_eq!(manager.read_info(&info.run_id).unwrap().status, RunStatus::Interrupted);
    assert!(h.notices.lock().unwrap().iter().any(|(name,v)| name == "run-status" && v["status"] == "interrupted"));
    let mut later = info.clone(); later.run_id = "run-test-2".into(); later.status = RunStatus::Failed; later.started_at_ms = 2;
    let dir = manager.run_dir(&later.run_id).unwrap(); fs::create_dir(dir.clone()).unwrap(); write_info(&dir, &later).unwrap();
    assert_eq!(manager.list_runs().unwrap().iter().map(|i| i.run_id.as_str()).collect::<Vec<_>>(), ["run-test-2", "run-test-1"]);
}

#[test]
fn live_codex_and_claude_share_summary_reset_and_disk_replay() {
    let h = Harness::new("sleep 2\ncat \"$FAKE_STREAM\""); let info = h.start();
    let now = time::now_ms(); let sec = now/1000%86400;
    let ts = format!("{}T{:02}:{:02}:{:02}.{:03}Z", time::date_path((now/86400000) as i64).replace('/', "-"), sec/3600, sec/60%60, sec%60, now%1000);
    let dir = h.manager.config.sessions.join(time::date_path((now/86400000) as i64)); fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-test-codex.jsonl");
    let meta = json!({"type":"session_meta","timestamp":ts,"payload":{"id":"test-codex","cwd":info.worktree,"base_instructions":"private"}}).to_string();
    let command = json!({"type":"event_msg","timestamp":ts,"payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["pwd"],"status":"completed"}}}).to_string();
    fs::write(&path, format!("{meta}\n{command}\n")).unwrap();
    let started = Instant::now();
    loop {
        if h.notices.lock().unwrap().iter().any(|(name,v)| name == "run-event" && v["event"]["kind"] == "command") { break; }
        assert!(started.elapsed() < Duration::from_secs(5)); thread::sleep(Duration::from_millis(25));
    }
    fs::write(&path, format!("{meta}\n")).unwrap();
    assert_eq!(h.done(&info.run_id).status, RunStatus::Passed);
    let loaded = h.manager.load_run(&info.run_id).unwrap();
    assert!(loaded.events.iter().any(|e| e.agent == Agent::Codex)); assert_eq!(loaded.summary.agents[&Agent::Codex].commands, 0);
    let notices = h.notices.lock().unwrap();
    assert!(notices.iter().any(|(name,v)| name == "run-reset" && v["watchId"] == info.run_id && v["sourceId"] == "test-codex"));
    let last = notices.iter().rev().find(|(name,_)| name == "run-summary").unwrap();
    assert_eq!(last.1["summary"], serde_json::to_value(&loaded.summary).unwrap());
}

#[test]
fn startup_failure_rolls_back_worktree_and_branch() {
    let h = Harness::new("exit 0");
    let executable_path = h.manager.config.variables.get(std::ffi::OsStr::new("CODEX_CONSOLE_CLAUDE_BIN")).unwrap();
    fs::set_permissions(Path::new(executable_path), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(h.manager.start_run(request(&h.repo)).is_err());
    assert!(h.manager.list_runs().unwrap().is_empty());
    assert!(git::run(&h.repo, &["branch", "--list", "console/*"]).unwrap().is_empty());
    assert_eq!(fs::read_dir(h.manager.config.data.join("worktrees")).unwrap().count(), 0);
}

#[test]
fn stubborn_process_is_killed_and_cancel_failure_is_best_effort() {
    let h = Harness::new("trap '' TERM\nsleep 120 &\nprintf '%s' \"$$\" > pids\nwait");
    let node = h.manager.config.home.as_ref().unwrap().join(".local/bin/node");
    executable(&node, "#!/bin/sh\nexit 7\n");
    let info = h.start(); wait_file(&info.worktree.join("pids"));
    let pgid = fs::read_to_string(info.worktree.join("pids")).unwrap();
    let started = Instant::now(); h.manager.stop_run(&info.run_id).unwrap();
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(15));
    let deadline = Instant::now();
    while alive(&format!("-{pgid}")) { assert!(deadline.elapsed() < Duration::from_secs(3)); thread::sleep(Duration::from_millis(20)); }
    assert_eq!(h.manager.read_info(&info.run_id).unwrap().status, RunStatus::Stopped);
}

#[test]
fn stderr_is_bounded_and_raw_stream_secrets_never_enter_journal() {
    let h = Harness::new("cat \"$FAKE_STDERR\" >&2\ncat \"$FAKE_STREAM\"");
    // Test the same cap used by the live worker without changing the process environment.
    let path = h._temp.0.join("large-stderr"); fs::write(&path, vec![b'x'; 1200000]).unwrap();
    let mut config = h.manager.config.clone(); config.variables.insert("FAKE_STDERR".into(), path.into_os_string());
    let manager = RunManager::new(config, h.manager.emit.clone()).unwrap();
    let info = manager.start_run(request(&h.repo)).unwrap();
    let started = Instant::now();
    while manager.read_info(&info.run_id).unwrap().status == RunStatus::Running { assert!(started.elapsed() < Duration::from_secs(10)); thread::sleep(Duration::from_millis(30)); }
    assert_eq!(manager.read_info(&info.run_id).unwrap().status, RunStatus::Passed);
    let dir = manager.run_dir(&info.run_id).unwrap();
    assert_eq!(fs::metadata(dir.join("claude.stderr")).unwrap().len(), 1024 * 1024);
    let log = fs::read_to_string(dir.join("events.jsonl")).unwrap(); assert!(!log.contains("base_instructions")); assert!(!log.contains("signature"));
}

#[test]
fn runtime_timeout_stops_and_companion_cancel_has_ten_second_limit() {
    let h = Harness::new("sleep 120 &\nkid=$!\ntrap 'kill \"$kid\" 2>/dev/null; wait \"$kid\" 2>/dev/null; exit 0' TERM\nwait \"$kid\"");
    let node = h.manager.config.home.as_ref().unwrap().join(".local/bin/node");
    executable(&node, "#!/bin/sh\nprintf '%s' \"$$\" > cancel-pgid\nexec sleep 120\n");
    let info = h.record(RunStatus::Running);
    fs::remove_file(h.manager.run_dir(&info.run_id).unwrap().join("events.jsonl")).unwrap();
    let started = Instant::now();
    worker::start_with_timeout(&h.manager.config, &info, h.manager.config.launch().unwrap(), vec![], Arc::new(AtomicBool::new(false)), h.manager.emit.clone(), Duration::from_millis(100)).unwrap().join().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(10)); assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(h.manager.read_info(&info.run_id).unwrap().status, RunStatus::Stopped);
    let pgid = fs::read_to_string(info.worktree.join("cancel-pgid")).unwrap(); assert!(!alive(&format!("-{pgid}")));
    assert!(h.notices.lock().unwrap().iter().any(|(name,v)| name == "run-status" && v["status"] == "stopped" && v["detail"].as_str().is_some_and(|s| s.contains("90 分钟"))));
}

/// Runs real Claude and Codex against the repository in REAL_REPO. It spends real quota, so it only
/// runs when asked for explicitly: cargo test real_run_loop -- --ignored --nocapture
#[test]
#[ignore = "真实 Claude 和 Codex，会消耗额度"]
fn real_run_loop() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("需要环境变量 {name}"));
    let emit: Emit = Arc::new(|name: &str, payload: serde_json::Value| match name {
        "run-status" => println!("STATUS {payload}"),
        "run-event" => {
            let e = &payload["event"];
            let text: String = ["command", "summary", "text", "path", "outcome", "mode", "lastMessage"].iter()
                .find_map(|k| e[*k].as_str()).unwrap_or("").chars().take(90).collect::<String>().replace('\n', " ");
            println!("EVENT {} {} {} {}", e["agent"].as_str().unwrap_or(""), e["kind"].as_str().unwrap_or(""), e["isError"].as_bool().unwrap_or(false), text);
        }
        _ => {}
    });
    let manager = RunManager::new(Config::current().unwrap(), emit).unwrap();
    let verify = var("REAL_VERIFY").lines().map(str::to_owned).collect();
    let info = manager.start_run(StartRequest { repo: var("REAL_REPO"), goal: var("REAL_GOAL"), verify_commands: verify, model: None }).unwrap();
    println!("RUN {} {} {}", info.run_id, info.branch, info.worktree.display());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if manager.read_info(&info.run_id).unwrap().status != RunStatus::Running { break; }
    }
    let loaded = manager.load_run(&info.run_id).unwrap();
    println!("FINAL {:?} round={} verdict={:?} events={}", loaded.info.status, loaded.summary.round, loaded.summary.last_verdict, loaded.events.len());
    println!("AGENTS {}", serde_json::to_string(&loaded.summary.agents).unwrap());
    manager.shutdown();
}
