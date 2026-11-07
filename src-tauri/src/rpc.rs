use crate::{dto, environment::LaunchEnvironment, settings::{plan_edits, SettingsRequest, SettingsResult}};
use serde_json::{json, Value};
use std::{collections::HashMap, process::Stdio, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex}, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{ChildStdin, Command}, sync::{oneshot, Mutex as AsyncMutex}, time::timeout};

type Reply = Result<Value, String>;
// Raw server messages/data never leave the reader. Keep only the conflict discriminator.
#[derive(Debug)]
struct WireError { safe_message: String, code: Option<i64>, version_conflict: bool }
impl From<String> for WireError {
    fn from(safe_message: String) -> Self { Self { safe_message, code: None, version_conflict: false } }
}
impl From<&str> for WireError { fn from(message: &str) -> Self { message.to_owned().into() } }
impl From<WireError> for String { fn from(error: WireError) -> Self { error.safe_message } }
impl WireError {
    fn is_conflict(&self) -> bool { self.code == Some(-32600) && self.version_conflict }
}
type WireReply = Result<Value, WireError>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<WireReply>>>>;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The only public request entry point takes a method, never arbitrary wire parameters.
fn read_params(method: &str) -> Result<Value, String> {
    match method {
        "model/list" => Ok(json!({"limit": 50})),
        "account/rateLimits/read" | "account/usage/read" => Ok(Value::Null),
        "config/read" => Ok(json!({"includeLayers": true})),
        _ => Err("此方法不在只读白名单中".into()),
    }
}

pub struct Client {
    connection: AsyncMutex<Option<Arc<Connection>>>,
    settings_write: AsyncMutex<()>,
    // None resolves the environment at launch, so a missing installation can recover later.
    launch: Option<LaunchEnvironment>,
    request_timeout: Duration,
    stopped: AtomicBool,
}
impl Default for Client {
    fn default() -> Self { Self { connection: AsyncMutex::new(None), settings_write: AsyncMutex::new(()), launch: None, request_timeout: REQUEST_TIMEOUT, stopped: AtomicBool::new(false) } }
}
impl Client {
    pub async fn request(&self, method: &str) -> Reply {
        let params = read_params(method)?;
        timeout(self.request_timeout, async {
            let connection = self.ready().await?;
            connection.send_wire(method, params, self.request_timeout).await.map_err(String::from)
        }).await.unwrap_or_else(|_| Err("读取请求超时，请稍后刷新".into()))
    }

    pub async fn update_settings(&self, request: SettingsRequest) -> Result<SettingsResult, String> {
        // Covers reads, planning, write, and read-back; concurrent settings updates cannot interleave.
        let _write = self.settings_write.lock().await;
        if request.expected_version.is_empty() { return Err("无法读取配置版本".into()); }
        let connection = self.ready().await?;
        let read_config = || async {
            dto::config(connection.send_wire("config/read", read_params("config/read")?, self.request_timeout).await.map_err(String::from)?)
        };
        let current = read_config().await?;
        let models = dto::models(connection.send_wire("model/list", read_params("model/list")?, self.request_timeout).await.map_err(String::from)?)?;
        let edits = plan_edits(&current, &request, &models)?;
        if edits.is_empty() { return Ok(SettingsResult::new("unchanged", current)); }
        // The sole settings write path. Never expose arbitrary methods, parameters or file paths.
        let result = connection.send_wire("config/batchWrite", json!({
            "edits": edits, "expectedVersion": request.expected_version, "reloadUserConfig": false,
        }), self.request_timeout).await;
        let (outcome, metadata) = match result {
            Err(error) if error.is_conflict() => ("conflict", None),
            Err(error) => return Err(error.into()),
            Ok(value) => match value["status"].as_str() {
                Some("ok") => ("applied", None),
                Some("okOverridden") => ("overridden", Some(value["overriddenMetadata"].clone())),
                _ => return Err("无法确认配置保存结果，请刷新后重试".into()),
            },
        };
        let mut result = SettingsResult::new(outcome, read_config().await?);
        if let Some(metadata) = metadata {
            result.overridden_by = Some(metadata["overridingLayer"]["name"]["type"].as_str().unwrap_or("未知层").to_owned());
            result.effective_value = Some(metadata["effectiveValue"].clone());
        }
        Ok(result)
    }

    async fn ready(&self) -> Result<Arc<Connection>, String> {
        let mut slot = self.connection.lock().await;
        if self.stopped.load(Ordering::SeqCst) { return Err("应用正在退出".into()); }
        if let Some(connection) = slot.as_ref().filter(|c| c.alive.load(Ordering::SeqCst)) { return Ok(connection.clone()); }
        *slot = None;
        let current;
        let launch = match &self.launch { Some(launch) => launch, None => { current = LaunchEnvironment::current()?; &current } };
        let connection = Arc::new(Connection::spawn(launch)?);
        // Keep the startup lock through both handshake messages. Cancellation drops and kills it.
        connection.send_wire("initialize", json!({"clientInfo":{"name":"codex-console","title":"Codex 控制台","version":"0.1.0"},"capabilities":{"experimentalApi":true}}), self.request_timeout).await?;
        connection.write(json!({"method":"initialized"})).await?;
        if !connection.alive.load(Ordering::SeqCst) { return Err("Codex 子进程已退出".into()); }
        *slot = Some(connection.clone());
        Ok(connection)
    }

    pub async fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(connection) = self.connection.lock().await.take() {
            connection.stop();
            let reader = connection.reader.lock().unwrap_or_else(|p| p.into_inner()).take();
            // Wait until the child has been killed and reaped before the application exits.
            if let Some(reader) = reader { let _ = reader.await; }
        }
    }
}

struct Connection {
    stdin: AsyncMutex<ChildStdin>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
fn fail_pending(pending: &Pending, alive: &AtomicBool, error: &str) {
    let mut requests = pending.lock().unwrap_or_else(|p| p.into_inner());
    alive.store(false, Ordering::SeqCst);
    for (_, sender) in requests.drain() { let _ = sender.send(Err(error.into())); }
}
struct PendingGuard { id: u64, pending: Pending }
impl Drop for PendingGuard {
    fn drop(&mut self) { self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.id); }
}
impl Connection {
    fn spawn(launch: &LaunchEnvironment) -> Result<Self, String> {
        let mut child = Command::new(&launch.executable).arg("app-server")
            .env_clear().envs(&launch.variables)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
            .spawn().map_err(|_| "无法启动 Codex，请检查可执行文件与 Node 环境".to_owned())?;
        let stdin = child.stdin.take().ok_or("无法打开 Codex 输入管道")?;
        let stdout = child.stdout.take().ok_or("无法打开 Codex 输出管道")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let reader_pending = pending.clone();
        let reader_alive = alive.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    _ = child.wait() => break,
                    line = lines.next_line() => {
                        match line {
                            Ok(Some(line)) => {
                                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue; };
                                // Ignore notifications (including those with an id) and unrelated responses.
                                if value.get("method").is_some() { continue; }
                                let Some(id) = value["id"].as_u64() else { continue; };
                                if value.get("result").is_none() && value.get("error").is_none() { continue; }
                                if let Some(sender) = reader_pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
                                    let reply = if value.get("error").is_some_and(|v| !v.is_null()) { Err(WireError {
                                        safe_message: "Codex 暂时无法完成请求，请稍后重试".into(),
                                        code: value["error"]["code"].as_i64(),
                                        version_conflict: value["error"]["data"]["config_write_error_code"] == "configVersionConflict",
                                    }) }
                                        else { Ok(value["result"].clone()) };
                                    let _ = sender.send(reply);
                                }
                            },
                            _ => break,
                        }
                    }
                }
            }
            fail_pending(&reader_pending, &reader_alive, "Codex 子进程已退出，请刷新重试");
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        Ok(Self { stdin: AsyncMutex::new(stdin), pending, alive, next_id: AtomicU64::new(1), shutdown: Mutex::new(Some(shutdown)), reader: Mutex::new(Some(reader)) })
    }
    async fn write(&self, value: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "无法编码读取请求")?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
            self.stop();
            return Err("Codex 输入管道已关闭".into());
        }
        Ok(())
    }
    // Private transport primitive: only ready() can perform the required initialize handshake.
    async fn send_wire(&self, method: &str, params: Value, duration: Duration) -> WireReply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if !self.alive.load(Ordering::SeqCst) { return Err("Codex 子进程已退出".into()); }
            pending.insert(id, sender);
        }
        let _guard = PendingGuard { id, pending: self.pending.clone() };
        timeout(duration, async {
            // Poll the response concurrently with writing so process exit interrupts even a blocked pipe.
            let write = self.write(json!({"id":id,"method":method,"params":params}));
            tokio::pin!(write);
            let mut receiver = receiver;
            tokio::select! {
                result = &mut receiver => result.unwrap_or_else(|_| Err("Codex 连接已关闭".into())),
                result = &mut write => {
                    result?;
                    receiver.await.unwrap_or_else(|_| Err("Codex 连接已关闭".into()))
                }
            }
        }).await.unwrap_or_else(|_| Err("读取请求超时，请稍后刷新".into()))
    }
    fn stop(&self) {
        fail_pending(&self.pending, &self.alive, "Codex 连接已关闭");
        if let Some(sender) = self.shutdown.lock().unwrap_or_else(|p| p.into_inner()).take() { let _ = sender.send(()); }
    }
}
impl Drop for Connection { fn drop(&mut self) { self.stop(); } }

#[cfg(test)]
mod tests;
