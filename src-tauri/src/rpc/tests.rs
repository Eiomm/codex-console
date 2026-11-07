use super::*;
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf, time::Instant};

static TEST_ID: AtomicU64 = AtomicU64::new(0);
struct Fixture { dir: PathBuf, client: Client }
impl Fixture {
    fn new(mode: &str, duration: Duration) -> Self {
        let dir = std::env::temp_dir().join(format!("codex-console-{}-{}", std::process::id(), TEST_ID.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-codex");
        std::fs::write(&script, include_str!("../fixtures/fake_codex.py")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Exercise the override through the same pure environment path as production, without
        // changing process-global environment variables while other tests are running.
        let vars = BTreeMap::from([
            (OsString::from("CODEX_CONSOLE_BIN"), script.into_os_string()),
            ("TEST_STARTS".into(), dir.join("starts").into_os_string()),
            ("TEST_MODE".into(), mode.into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("CODEX_CONSOLE_PROXY".into(), "".into()),
        ]);
        let launch = LaunchEnvironment::from_variables(Some(&dir), vars).unwrap();
        Self { dir, client: Client { launch: Some(launch), request_timeout: duration, ..Client::default() } }
    }
    fn starts(&self) -> usize { std::fs::read_to_string(self.dir.join("starts")).unwrap().lines().count() }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }

#[tokio::test]
async fn whitelist_rejects_writes_without_starting_process() {
    let f = Fixture::new("normal", Duration::from_secs(3));
    for method in ["account/rateLimitResetCredit/consume", "config/value/write", "config/batchWrite", "account/logout", "initialize", "unknown"] {
        assert!(f.client.request(method).await.unwrap_err().contains("白名单"));
    }
    assert!(!f.dir.join("starts").exists());
}
#[tokio::test]
async fn normal_requests_and_five_concurrent_calls_use_one_process() {
    let f = Fixture::new("reverse", Duration::from_secs(3));
    let (a,b,c,d,e) = tokio::join!(f.client.request("model/list"), f.client.request("config/read"), f.client.request("account/usage/read"), f.client.request("account/rateLimits/read"), f.client.request("model/list"));
    for (result, method) in [(a,"model/list"),(b,"config/read"),(c,"account/usage/read"),(d,"account/rateLimits/read"),(e,"model/list")] { assert_eq!(result.unwrap()["method"], method); }
    assert_eq!(f.starts(), 1);
    f.client.shutdown().await;
}
#[tokio::test]
async fn exit_fails_all_waiters_immediately_and_next_request_recovers() {
    let f = Fixture::new("crash", Duration::from_secs(5));
    let start = Instant::now();
    let (a,b,c,d,e) = tokio::join!(f.client.request("model/list"), f.client.request("config/read"), f.client.request("account/usage/read"), f.client.request("account/rateLimits/read"), f.client.request("model/list"));
    for result in [a,b,c,d,e] { assert!(result.unwrap_err().contains("退出")); }
    assert!(start.elapsed() < Duration::from_secs(2), "must fail on exit, not request timeout");
    assert_eq!(f.client.request("config/read").await.unwrap()["method"], "config/read");
    assert_eq!(f.starts(), 2);
    f.client.shutdown().await;
}
#[tokio::test]
async fn silent_server_times_out_and_releases_waiters() {
    let mut f = Fixture::new("hang", Duration::from_secs(3));
    f.client.ready().await.unwrap();
    f.client.request_timeout = Duration::from_millis(300);
    let start = Instant::now();
    assert!(f.client.request("model/list").await.unwrap_err().contains("超时"));
    assert!(start.elapsed() >= Duration::from_millis(280));
    assert!(start.elapsed() < Duration::from_secs(2));
    let slot = f.client.connection.lock().await;
    assert!(slot.as_ref().unwrap().pending.lock().unwrap().is_empty());
    drop(slot);
    f.client.shutdown().await;
}
#[tokio::test]
async fn dashboard_failure_is_isolated_and_server_errors_are_redacted() {
    let f = Fixture::new("partial", Duration::from_secs(3));
    let result = serde_json::to_value(crate::dashboard(&f.client).await).unwrap();
    assert_eq!(result["usage"]["ok"], false);
    for module in ["quota", "config", "models"] { assert_eq!(result[module]["ok"], true); }
    assert!(!result.to_string().contains("PRIVATE_ACCOUNT_DATA"));
    f.client.shutdown().await;
}

#[tokio::test]
async fn shutdown_reaps_child_and_prevents_restart() {
    let f = Fixture::new("normal", Duration::from_secs(3));
    assert!(f.client.request("model/list").await.is_ok());
    let pid = std::fs::read_to_string(f.dir.join("starts.pid")).unwrap();
    f.client.shutdown().await;
    assert!(!std::process::Command::new("/bin/kill").args(["-0", &pid])
        .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success());
    assert!(f.client.request("model/list").await.unwrap_err().contains("退出"));
    assert_eq!(f.starts(), 1);
}

fn settings_request(value: Value) -> SettingsRequest {
    serde_json::from_value(value).unwrap()
}
fn request_log(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(f.dir.join("starts.requests")).unwrap().lines().map(str::to_owned).collect()
}
#[tokio::test]
async fn update_settings_reads_back_success_conflict_and_override_without_leaking() {
    for (mode, outcome, effort) in [
        ("settings-normal", "applied", "low"),
        ("settings-conflict", "conflict", "low"),
        ("settings-overridden", "overridden", "high"),
    ] {
        let f = Fixture::new(mode, Duration::from_secs(3));
        let result = f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","effort":"low"}))).await.unwrap();
        assert_eq!(result.outcome, outcome);
        assert_eq!(result.config.effort.as_deref(), Some(effort));
        assert_eq!(result.config.version.as_deref(), Some("sha256:v2"));
        let serialized = serde_json::to_value(result).unwrap();
        assert!(!serialized.to_string().contains("PRIVATE_ACCOUNT_DATA"));
        assert!(!serialized.to_string().contains("message"));
        assert!(!serialized.to_string().contains("filePath"));
        if outcome == "overridden" {
            assert_eq!(serialized["overriddenBy"], "system");
            assert_eq!(serialized["effectiveValue"], "high");
        } else {
            assert!(serialized.get("overriddenBy").is_none());
            assert!(serialized.get("effectiveValue").is_none());
        }
        assert_eq!(request_log(&f), ["config/read", "model/list", "config/batchWrite", "config/read"]);
        assert_eq!(f.starts(), 1);
        f.client.shutdown().await;
    }
}
#[tokio::test]
async fn update_settings_generic_errors_are_redacted_and_not_misclassified() {
    let f = Fixture::new("settings-error", Duration::from_secs(3));
    let error = f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","effort":"low"}))).await.unwrap_err();
    assert_eq!(error, "Codex 暂时无法完成请求，请稍后重试");
    assert!(!error.contains("PRIVATE_ACCOUNT_DATA"));
    assert_eq!(request_log(&f), ["config/read", "model/list", "config/batchWrite"]);
    f.client.shutdown().await;
}
#[tokio::test]
async fn update_settings_uses_frontend_version_and_never_retries_conflicts() {
    let f = Fixture::new("settings-normal", Duration::from_secs(3));
    let result = f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:stale","effort":"low"}))).await.unwrap();
    assert_eq!(result.outcome, "conflict");
    assert_eq!(result.config.version.as_deref(), Some("sha256:v2"));
    assert_eq!(request_log(&f), ["config/read", "model/list", "config/batchWrite", "config/read"]);
    f.client.shutdown().await;
}
#[tokio::test]
async fn update_settings_unchanged_and_invalid_requests_never_write() {
    let f = Fixture::new("settings-normal", Duration::from_secs(3));
    let result = f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","model":"fast-model","effort":"high","fast":true}))).await.unwrap();
    assert_eq!(result.outcome, "unchanged");
    assert_eq!(request_log(&f), ["config/read", "model/list"]);
    assert!(f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","model":"unknown"}))).await.is_err());
    assert_eq!(request_log(&f), ["config/read", "model/list", "config/read", "model/list"]);
    f.client.shutdown().await;
    let f = Fixture::new("settings-normal", Duration::from_secs(3));
    assert!(f.client.update_settings(settings_request(json!({"expectedVersion":"","fast":false}))).await.is_err());
    assert!(!f.dir.join("starts").exists());
}
#[tokio::test]
async fn update_settings_concurrent_transactions_do_not_interleave() {
    let f = Fixture::new("settings-normal", Duration::from_secs(3));
    let (first, second) = tokio::join!(
        f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","effort":"low"}))),
        f.client.update_settings(settings_request(json!({"expectedVersion":"sha256:v1","fast":false}))),
    );
    assert_eq!(first.unwrap().outcome, "applied");
    assert_eq!(second.unwrap().outcome, "conflict");
    assert_eq!(request_log(&f), [
        "config/read", "model/list", "config/batchWrite", "config/read",
        "config/read", "model/list", "config/batchWrite", "config/read",
    ]);
    assert_eq!(f.starts(), 1);
    f.client.shutdown().await;
}

#[tokio::test]
#[ignore = "仅审查方显式运行：真实 Codex，CODEX_HOME 和 HOME 均为临时目录；不使用真实用户配置"]
async fn real_settings_isolated() {
    let dir = std::env::temp_dir().join(format!("codex-console-real-settings-{}-{}", std::process::id(), TEST_ID.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("config.toml");
    std::fs::write(&path, "# isolated settings comment\nmodel = \"isolated-before\"\nmodel_reasoning_effort = \"isolated-before\"\nservice_tier = \"fast\"\n\n[marketplaces.demo]\n# unrelated comment\nsource = \"https://example.invalid/demo\"\n").unwrap();
    // Resolve only the executable from the host PATH. The child receives an isolated home,
    // never the host's CODEX_HOME, config, credentials, or process-global environment changes.
    let mut variables = BTreeMap::from([
        (OsString::from("PATH"), std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
        ("CODEX_HOME".into(), dir.clone().into_os_string()),
        ("HOME".into(), dir.clone().into_os_string()),
        ("XDG_CONFIG_HOME".into(), dir.clone().into_os_string()),
        ("CODEX_CONSOLE_PROXY".into(), "".into()),
    ]);
    if let Some(executable) = std::env::var_os("CODEX_CONSOLE_BIN") { variables.insert("CODEX_CONSOLE_BIN".into(), executable); }
    let launch = LaunchEnvironment::from_variables(dirs::home_dir().as_deref(), variables).unwrap();
    assert_eq!(launch.variables[std::ffi::OsStr::new("CODEX_HOME")], dir.as_os_str());
    let client = Client { launch: Some(launch), ..Client::default() };
    let models = dto::models(client.request("model/list").await.unwrap()).unwrap();
    let target = models.iter().find(|m| m.hidden != Some(true) && m.fast_supported && m.efforts.len() >= 2 && m.default_effort.as_ref().is_some_and(|e| m.efforts.contains(e))).expect("需要一个支持 Fast、有效默认强度且有至少两个推理强度的模型");
    let mut config = dto::config(client.request("config/read").await.unwrap()).unwrap();
    fn assert_file(path: &std::path::Path, model: &str, effort: &str, fast: bool) {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("# isolated settings comment"));
        assert!(text.contains("[marketplaces.demo]\n# unrelated comment\nsource = \"https://example.invalid/demo\""));
        let root = text.split("[marketplaces.demo]").next().unwrap();
        let value = |key: &str| root.lines().filter_map(|line| line.split_once('=')).find(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().trim_matches('"'));
        assert_eq!(value("model"), Some(model));
        assert_eq!(value("model_reasoning_effort"), Some(effort));
        assert_eq!(value("service_tier"), if fast { Some("fast") } else { None });
    }
    let model = target.model.as_deref().unwrap();
    let default = target.default_effort.as_deref().unwrap();
    let other_effort = target.efforts.iter().find(|e| e.as_str() != default).unwrap();
    for (update, effort, fast) in [
        (json!({"model":model}), default, true),
        (json!({"effort":other_effort}), other_effort.as_str(), true),
        (json!({"fast":false}), other_effort.as_str(), false),
        (json!({"fast":true}), other_effort.as_str(), true),
    ] {
        let mut update = update;
        update["expectedVersion"] = json!(config.version.as_ref().unwrap());
        let result = client.update_settings(settings_request(update)).await.unwrap();
        assert_eq!(result.outcome, "applied");
        assert_file(&path, model, effort, fast);
        assert_eq!(result.config.model.as_deref(), Some(model));
        assert_eq!(result.config.effort.as_deref(), Some(effort));
        assert_eq!(result.config.service_tier.as_deref(), if fast { Some("fast") } else { None });
        config = result.config;
    }
    let before = std::fs::read_to_string(&path).unwrap();
    let result = client.update_settings(settings_request(json!({"expectedVersion":"sha256:0000000000000000000000000000000000000000000000000000000000000000","fast":false}))).await.unwrap();
    assert_eq!(result.outcome, "conflict");
    assert_eq!(result.config.version, config.version);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_file(&path, model, other_effort, true);
    client.shutdown().await;
}
