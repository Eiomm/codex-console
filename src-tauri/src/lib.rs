mod dto;
mod environment;
pub mod events;
mod rpc;
mod settings;
pub mod run;

use dto::{Dashboard, Module};
use rpc::Client;
use tauri::{Emitter, Manager};

async fn dashboard(client: &Client) -> Dashboard {
    let (quota, usage, config, models) = tokio::join!(
        client.request("account/rateLimits/read"), client.request("account/usage/read"),
        client.request("config/read"), client.request("model/list"),
    );
    Dashboard {
        quota: Module::from_result(quota.and_then(dto::quota)),
        usage: Module::from_result(usage.and_then(dto::usage)),
        config: Module::from_result(config.and_then(dto::config)),
        models: Module::from_result(models.and_then(dto::models)),
    }
}

#[tauri::command]
async fn get_dashboard(client: tauri::State<'_, Client>) -> Result<Dashboard, String> {
    Ok(dashboard(&client).await)
}

#[tauri::command]
async fn update_settings(
    client: tauri::State<'_, Client>, expected_version: String,
    model: Option<String>, effort: Option<String>, fast: Option<bool>,
) -> Result<settings::SettingsResult, String> {
    client.update_settings(settings::SettingsRequest { expected_version, model, effort, fast }).await
}

#[tauri::command]
async fn list_codex_sessions(cwd: Option<String>) -> Result<Vec<events::discover::SessionInfo>, String> {
    events::list_sessions(cwd).await
}

#[tauri::command]
async fn load_codex_session(thread_id: String) -> Result<events::LoadedSession, String> {
    events::load_session(thread_id).await
}

#[tauri::command]
async fn watch_codex(
    app: tauri::AppHandle, watches: tauri::State<'_, events::watch::WatchManager>, cwd: String,
) -> Result<String, String> {
    let root = events::discover::default_root().ok_or("无法确定会话目录")?;
    watches.start_with_notifications(root, cwd, move |notification| {
        match notification {
            events::watch::WatchNotification::Event(payload) => { let _ = app.emit("run-event", payload); }
            events::watch::WatchNotification::Reset(payload) => { let _ = app.emit("run-reset", payload); }
            events::watch::WatchNotification::Summary(payload) => { let _ = app.emit("run-summary", payload); }
        }
    })
}

#[tauri::command]
async fn unwatch_codex(watches: tauri::State<'_, events::watch::WatchManager>, watch_id: String) -> Result<(), String> {
    watches.stop(&watch_id);
    Ok(())
}

// Git/process operations run off the async/UI thread. RunManager serializes mutations.
#[tauri::command]
async fn start_run(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>, repo: String, goal: String, verify_commands: Vec<String>, model: Option<String>) -> Result<run::RunInfo, String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.start_run(run::StartRequest { repo, goal, verify_commands, model })).await.map_err(|_| "启动任务中断")?
}
#[tauri::command]
async fn stop_run(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>, run_id: String) -> Result<(), String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.stop_run(&run_id)).await.map_err(|_| "停止任务中断")?
}
#[tauri::command]
async fn merge_run(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>, run_id: String) -> Result<run::MergeResult, String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.merge_run(&run_id)).await.map_err(|_| "合并任务中断")?
}
#[tauri::command]
async fn discard_run(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>, run_id: String) -> Result<(), String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.discard_run(&run_id)).await.map_err(|_| "放弃任务中断")?
}
#[tauri::command]
async fn list_runs(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>) -> Result<Vec<run::RunInfo>, String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.list_runs()).await.map_err(|_| "读取任务列表中断")?
}
#[tauri::command]
async fn load_run(manager: tauri::State<'_, std::sync::Arc<run::RunManager>>, run_id: String) -> Result<run::LoadedRun, String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.load_run(&run_id)).await.map_err(|_| "读取任务中断")?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Client::default())
        .manage(events::watch::WatchManager::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let manager = run::RunManager::current(move |name, payload| { let _ = handle.emit(name, payload); })
                .map_err(std::io::Error::other)?;
            app.manage(std::sync::Arc::new(manager));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_dashboard, update_settings, list_codex_sessions, load_codex_session, watch_codex, unwatch_codex, start_run, stop_run, merge_run, discard_run, list_runs, load_run])
        .build(tauri::generate_context!())
        .expect("无法启动 Codex 控制台")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<std::sync::Arc<run::RunManager>>().shutdown();
                app.state::<events::watch::WatchManager>().shutdown();
                tauri::async_runtime::block_on(app.state::<Client>().shutdown());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "需要真实 Codex、登录状态与网络；仅在审查方环境显式运行"]
    async fn real_dashboard() {
        let client = Client::default();
        let result = dashboard(&client).await;
        // Only module status and list length are printed; never raw server/account data.
        for (name, ok) in [
            ("额度", matches!(result.quota, Module::Success { .. })),
            ("用量", matches!(result.usage, Module::Success { .. })),
            ("配置", matches!(result.config, Module::Success { .. })),
            ("模型", matches!(result.models, Module::Success { .. })),
        ] { println!("{name}: {}", if ok { "成功" } else { "失败" }); assert!(ok, "{name}模块失败"); }
        if let Module::Success { data, .. } = result.models { println!("模型数量：{}", data.len()); }
        client.shutdown().await;
    }
}
