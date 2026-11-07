use crate::environment;
use std::{collections::BTreeMap, ffi::{OsStr, OsString}, fs, path::{Path, PathBuf}};

#[derive(Default)]
pub struct ClaudeOptions { pub model: Option<String>, pub max_budget_usd: Option<f64> }

/// Each permission is a separate argv entry; the companion path is always fully expanded.
pub fn build_claude_args(prompt: &str, companion: &str, verify_commands: &[String], options: &ClaudeOptions) -> Vec<String> {
    let mut args = vec!["-p".into(), prompt.into(), "--output-format".into(), "stream-json".into(),
        "--verbose".into(), "--strict-mcp-config".into(), "--max-budget-usd".into(),
        options.max_budget_usd.unwrap_or(10.0).to_string()];
    if let Some(model) = &options.model { args.extend(["--model".into(), model.clone()]); }
    args.push("--allowedTools".into());
    args.extend(["Read", "Grep", "Glob", "Agent", "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)"].map(str::to_owned));
    args.extend([format!("Bash(node \"{companion}\":*)"), format!("Bash(node {companion}:*)")]);
    args.extend(verify_commands.iter().map(|command| format!("Bash({command}:*)")));
    args
}

#[derive(Clone)]
pub(super) struct Config {
    pub data: PathBuf,
    pub sessions: PathBuf,
    pub home: Option<PathBuf>,
    pub variables: BTreeMap<OsString, OsString>,
}
pub(super) struct Launch {
    pub executable: PathBuf,
    pub companion: PathBuf,
    pub node: PathBuf,
    pub variables: BTreeMap<OsString, OsString>,
}
impl Config {
    pub fn current() -> Result<Self, String> {
        let home = dirs::home_dir();
        Self::from_variables(home, std::env::vars_os().collect())
    }
    pub fn from_variables(home: Option<PathBuf>, variables: BTreeMap<OsString, OsString>) -> Result<Self, String> {
        let data = variables.get(OsStr::new("CODEX_CONSOLE_DATA")).filter(|s| !s.is_empty()).map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|p| p.join("com.didi.codexconsole"))).ok_or("无法确定任务数据目录")?;
        let sessions = home.as_ref().ok_or("无法确定会话目录")?.join(".codex/sessions");
        Ok(Self { data, sessions, home, variables })
    }
    pub fn launch(&self) -> Result<Launch, String> {
        let path = environment::child_path(self.home.as_deref(), self.variables.get(OsStr::new("PATH")).map(OsString::as_os_str))?;
        let executable = match self.variables.get(OsStr::new("CODEX_CONSOLE_CLAUDE_BIN")) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            Some(_) => return Err("CODEX_CONSOLE_CLAUDE_BIN 不能为空".into()),
            None => find_program("claude", &path).ok_or("找不到 Claude，请安装或设置 CODEX_CONSOLE_CLAUDE_BIN")?,
        };
        let executable = fs::canonicalize(executable).map_err(|_| "无法定位 Claude 可执行文件")?;
        let companion = match self.variables.get(OsStr::new("CODEX_CONSOLE_COMPANION")) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            Some(_) => return Err("CODEX_CONSOLE_COMPANION 不能为空".into()),
            None => latest_companion(&self.home.as_ref().ok_or("无法确定插件目录")?.join(".claude/plugins/cache/openai-codex/codex"))
                .ok_or("找不到 Codex 插件脚本，请安装插件或设置 CODEX_CONSOLE_COMPANION")?,
        };
        let companion = fs::canonicalize(companion).map_err(|_| "找不到 Codex 插件脚本，请检查 CODEX_CONSOLE_COMPANION")?;
        let text = companion.to_str().ok_or("插件脚本路径必须是有效文本")?;
        if !companion.is_file() || text.chars().any(|c| "\"'`;|&$<>\\\n\r*?()".contains(c)) {
            return Err("插件脚本路径含有权限规则不支持的字符".into());
        }
        let node = find_program("node", &path).ok_or("找不到 node 可执行文件")?;
        let configured = self.variables.get(OsStr::new("CODEX_CONSOLE_PROXY"));
        let mut variables = environment::proxy_environment(self.variables.clone(), configured.map(OsString::as_os_str));
        variables.insert("PATH".into(), path);
        Ok(Launch { executable, companion, node, variables })
    }
}
fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path).map(|dir| dir.join(name)).find(|p| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}
fn latest_companion(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root).ok()?.flatten().filter_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        let (core, suffix) = name.split_once('-').unwrap_or((&name, ""));
        let version: Vec<u64> = core.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
        let file = entry.path().join("scripts/codex-companion.mjs");
        file.is_file().then_some(((version, suffix.is_empty(), name), file))
    }).max_by(|a,b| a.0.cmp(&b.0)).map(|(_, file)| file)
}

pub(super) fn prompt(goal: &str, branch: &str, base: &str, companion: &str, verify: &[String]) -> String {
    let verify = if verify.is_empty() { "（未指定，只做读代码审查）".into() } else { verify.join("\n") };
    // Single pass: user text containing a placeholder is never recursively substituted.
    let values = [("{{GOAL}}", goal), ("{{BRANCH}}", branch), ("{{BASE}}", base), ("{{COMPANION}}", companion), ("{{VERIFY}}", &verify)];
    let mut remaining = include_str!("prompt.md");
    let mut result = String::new();
    while let Some((at, key, value)) = values.iter().filter_map(|(k,v)| remaining.find(k).map(|at| (at, *k, *v))).min_by_key(|x| x.0) {
        result.push_str(&remaining[..at]); result.push_str(value); remaining = &remaining[at + key.len()..];
    }
    result.push_str(remaining);
    result
}
