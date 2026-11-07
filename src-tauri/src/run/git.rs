use std::{path::{Path, PathBuf}, process::{Command, Output, Stdio}};

pub(super) fn output(repo: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args).stdin(Stdio::null());
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"] { command.env_remove(key); }
    command.output().map_err(|_| "无法运行 Git".into())
}
pub(super) fn run(repo: &Path, args: &[&str]) -> Result<String, String> {
    let result = output(repo, args)?;
    if !result.status.success() { return Err("Git 操作失败，请检查仓库状态".into()); }
    String::from_utf8(result.stdout).map(|s| s.trim().to_owned()).map_err(|_| "Git 返回了无效文本".into())
}
pub(super) fn validate_repo(repo: &str) -> Result<(PathBuf, String), String> {
    let repo = Path::new(repo).canonicalize().map_err(|_| "仓库目录不存在")?;
    let top = run(&repo, &["rev-parse", "--show-toplevel"]).map_err(|_| "指定目录不是 Git 工作树")?;
    if Path::new(&top).canonicalize().map_err(|_| "无法定位仓库顶层")? != repo {
        return Err("必须选择 Git 工作树顶层，不能选择子目录".into());
    }
    let base = run(&repo, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(|_| "仓库必须至少有一个提交")?;
    Ok((repo, base))
}
