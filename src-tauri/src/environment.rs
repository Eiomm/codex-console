use std::{collections::{BTreeMap, HashSet}, ffi::{OsStr, OsString}, path::{Path, PathBuf}};

/// Finder does not inherit a login shell's PATH. Keep this independent of the host environment.
pub fn child_path(home: Option<&Path>, inherited: Option<&OsStr>) -> Result<OsString, String> {
    let mut paths = Vec::new();
    if let Some(home) = home { paths.push(home.join(".local/bin")); }
    paths.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    if let Some(path) = inherited { paths.extend(std::env::split_paths(path)); }
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    std::env::join_paths(paths).map_err(|_| "无法构建子进程 PATH".into())
}

pub fn proxy_environment(mut env: BTreeMap<OsString, OsString>, configured: Option<&OsStr>) -> BTreeMap<OsString, OsString> {
    let keys = ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY", "all_proxy"];
    if !keys.iter().any(|key| env.contains_key(OsStr::new(key))) {
        let proxy = configured.unwrap_or(OsStr::new("http://127.0.0.1:7897"));
        if !proxy.is_empty() {
            for key in &keys[..4] { env.insert((*key).into(), proxy.to_owned()); }
        }
    }
    env.entry("NO_PROXY".into()).or_insert_with(|| "localhost,127.0.0.1".into());
    env
}

pub struct LaunchEnvironment {
    pub executable: PathBuf,
    pub variables: BTreeMap<OsString, OsString>,
}

impl LaunchEnvironment {
    pub fn from_variables(home: Option<&Path>, variables: BTreeMap<OsString, OsString>) -> Result<Self, String> {
        let path = child_path(home, variables.get(OsStr::new("PATH")).map(OsString::as_os_str))?;
        let executable = if let Some(explicit) = variables.get(OsStr::new("CODEX_CONSOLE_BIN")) {
            if explicit.is_empty() { return Err("CODEX_CONSOLE_BIN 不能为空".into()); }
            PathBuf::from(explicit)
        } else {
            std::env::split_paths(&path).map(|dir| dir.join("codex")).find(|file| {
                use std::os::unix::fs::PermissionsExt;
                file.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }).ok_or("找不到 Codex 可执行文件，请检查安装或 CODEX_CONSOLE_BIN")?
        };
        let configured = variables.get(OsStr::new("CODEX_CONSOLE_PROXY")).cloned();
        let mut variables = proxy_environment(variables, configured.as_deref());
        variables.insert("PATH".into(), path);
        Ok(Self { executable, variables })
    }

    pub fn current() -> Result<Self, String> {
        Self::from_variables(dirs::home_dir().as_deref(), std::env::vars_os().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_order_and_deduplication() {
        let path = child_path(Some(Path::new("/home/test")), Some(OsStr::new("/usr/bin:/usr/local/bin:/home/test/.local/bin:/opt/homebrew/bin:/bin:/usr/bin"))).unwrap();
        assert_eq!(path, "/home/test/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    }
    #[test]
    fn proxies_default_existing_empty_and_custom() {
        let defaults = proxy_environment(BTreeMap::new(), None);
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert_eq!(defaults[OsStr::new(key)], "http://127.0.0.1:7897");
        }
        assert_eq!(defaults[OsStr::new("NO_PROXY")], "localhost,127.0.0.1");
        let existing = BTreeMap::from([("https_proxy".into(), "http://existing:80".into()), ("NO_PROXY".into(), "internal".into())]);
        assert_eq!(proxy_environment(existing.clone(), None), existing);
        let empty = proxy_environment(BTreeMap::new(), Some(OsStr::new("")));
        assert!(!empty.contains_key(OsStr::new("HTTP_PROXY")));
        let custom = proxy_environment(BTreeMap::new(), Some(OsStr::new("http://custom:80")));
        assert_eq!(custom[OsStr::new("HTTPS_PROXY")], "http://custom:80");
    }
}
