use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MIN_NODE_MAJOR: u32 = 22;
const MIN_NODE_MINOR: u32 = 19;

#[derive(Clone, Debug)]
pub struct NodeToolchain {
    pub node: PathBuf,
    pub npx: PathBuf,
    pub node_version: String,
    pub path_env: OsString,
    pub source: String,
}

#[derive(Debug)]
struct Candidate {
    npx: PathBuf,
    path_env: OsString,
    source: String,
}

pub fn discover_node_toolchain() -> Result<NodeToolchain, String> {
    if let Some(explicit) = env::var_os("DSH_NPX") {
        let candidate = candidate_from_npx(
            PathBuf::from(explicit),
            env::var_os("PATH").unwrap_or_default(),
            "DSH_NPX",
        );
        return validate_candidate(candidate)
            .map_err(|error| format!("DSH_NPX 指向的 npx 不可用：{error}"));
    }

    let mut candidates = Vec::new();
    let current_path = env::var_os("PATH").unwrap_or_default();
    if let Some(npx) = find_in_path("npx", &current_path) {
        candidates.push(candidate_from_npx(npx, current_path.clone(), "当前 PATH"));
    }

    if let Some(shell_candidate) = discover_from_login_shell() {
        candidates.push(shell_candidate);
    }

    if let Some(nvm_bin) = env::var_os("NVM_BIN") {
        let bin = PathBuf::from(nvm_bin);
        candidates.push(candidate_from_bin(&bin, current_path.clone(), "NVM_BIN"));
    }

    candidates.extend(known_manager_candidates(&current_path));

    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    for candidate in candidates {
        if !seen.insert(candidate.npx.clone()) {
            continue;
        }
        match validate_candidate(candidate) {
            Ok(toolchain) => return Ok(toolchain),
            Err(error) => errors.push(error),
        }
    }

    let detail = if errors.is_empty() {
        "没有发现任何候选路径".to_string()
    } else {
        errors.join("；")
    };
    Err(format!(
        "找不到可用的 Node/npm 环境。请先在登录 shell 中安装 Node >= {MIN_NODE_MAJOR}.{MIN_NODE_MINOR}，或设置 DSH_NPX。{detail}"
    ))
}

fn validate_candidate(candidate: Candidate) -> Result<NodeToolchain, String> {
    if !candidate.npx.is_file() {
        return Err(format!(
            "{}：{} 不存在",
            candidate.source,
            candidate.npx.display()
        ));
    }

    let bin_dir = candidate
        .npx
        .parent()
        .ok_or_else(|| format!("{}：npx 路径没有父目录", candidate.source))?;
    let node = executable_in_dir(bin_dir, "node")
        .or_else(|| find_in_path("node", &candidate.path_env))
        .ok_or_else(|| format!("{}：找到 npx，但找不到同环境的 node", candidate.source))?;

    let path_env = prepend_path(bin_dir, &candidate.path_env)?;
    let output = Command::new(&node)
        .arg("--version")
        .env("PATH", &path_env)
        .output()
        .map_err(|error| format!("{}：无法执行 {}：{error}", candidate.source, node.display()))?;
    if !output.status.success() {
        return Err(format!("{}：node --version 执行失败", candidate.source));
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let (major, minor, _) = parse_node_version(&version)
        .ok_or_else(|| format!("{}：无法识别 Node 版本 {version}", candidate.source))?;
    if major < MIN_NODE_MAJOR || (major == MIN_NODE_MAJOR && minor < MIN_NODE_MINOR) {
        return Err(format!(
            "{}：Node {version} 低于要求的 v{MIN_NODE_MAJOR}.{MIN_NODE_MINOR}.0",
            candidate.source
        ));
    }

    let npx_output = Command::new(&candidate.npx)
        .arg("--version")
        .env("PATH", &path_env)
        .output()
        .map_err(|error| {
            format!(
                "{}：无法执行 {}：{error}",
                candidate.source,
                candidate.npx.display()
            )
        })?;
    if !npx_output.status.success() {
        return Err(format!("{}：npx --version 执行失败", candidate.source));
    }

    Ok(NodeToolchain {
        node,
        npx: candidate.npx,
        node_version: version,
        path_env,
        source: candidate.source,
    })
}

fn discover_from_login_shell() -> Option<Candidate> {
    let shell = env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
    let output = Command::new(shell)
        .args([
            "-lic",
            "printf '__DSH_NPX__=%s\\n' \"$(command -v npx)\"; printf '__DSH_PATH__=%s\\n' \"$PATH\"",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut npx = None;
    let mut path_env = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("__DSH_NPX__=") {
            if !value.is_empty() {
                npx = Some(PathBuf::from(value));
            }
        } else if let Some(value) = line.strip_prefix("__DSH_PATH__=") {
            path_env = Some(OsString::from(value));
        }
    }

    Some(candidate_from_npx(
        npx?,
        path_env.unwrap_or_default(),
        "登录 shell",
    ))
}

fn known_manager_candidates(current_path: &OsString) -> Vec<Candidate> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut bins = vec![
        (home.join(".volta/bin"), "Volta"),
        (home.join(".asdf/shims"), "asdf"),
        (home.join(".local/share/mise/shims"), "mise"),
        (PathBuf::from("/opt/homebrew/bin"), "Homebrew"),
        (PathBuf::from("/usr/local/bin"), "usr/local"),
        (PathBuf::from("/usr/bin"), "系统 PATH"),
    ];

    bins.extend(versioned_bin_dirs(
        &home.join(".nvm/versions/node"),
        "bin",
        "NVM",
    ));
    bins.extend(versioned_bin_dirs(
        &home.join(".local/share/fnm/node-versions"),
        "installation/bin",
        "fnm",
    ));

    bins.into_iter()
        .filter(|(bin, _)| executable_in_dir(bin, "npx").is_some())
        .map(|(bin, source)| candidate_from_bin(&bin, current_path.clone(), source))
        .collect()
}

fn versioned_bin_dirs(
    root: &Path,
    suffix: &str,
    source: &'static str,
) -> Vec<(PathBuf, &'static str)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    versions.sort_by_key(|path| std::cmp::Reverse(version_key(path)));
    versions
        .into_iter()
        .map(|version| (version.join(suffix), source))
        .collect()
}

fn version_key(path: &Path) -> (u32, u32, u32) {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(parse_node_version)
        .unwrap_or_default()
}

fn candidate_from_bin(bin: &Path, base_path: OsString, source: &str) -> Candidate {
    candidate_from_npx(
        executable_in_dir(bin, "npx").unwrap_or_else(|| bin.join(executable_name("npx"))),
        prepend_path(bin, &base_path).unwrap_or(base_path),
        source,
    )
}

fn candidate_from_npx(npx: PathBuf, path_env: OsString, source: &str) -> Candidate {
    Candidate {
        npx,
        path_env,
        source: source.to_string(),
    }
}

fn prepend_path(bin: &Path, base_path: &OsString) -> Result<OsString, String> {
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(env::split_paths(base_path));
    env::join_paths(paths).map_err(|error| format!("无法构造 PATH：{error}"))
}

fn find_in_path(name: &str, path_env: &OsString) -> Option<PathBuf> {
    env::split_paths(path_env).find_map(|dir| executable_in_dir(&dir, name))
}

fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(executable_name(name));
    path.is_file().then_some(path)
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    }
}

fn parse_node_version(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.trim().trim_start_matches('v').split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.split('-').next()?.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_versions() {
        assert_eq!(parse_node_version("v24.14.0"), Some((24, 14, 0)));
        assert_eq!(parse_node_version("22.19.0"), Some((22, 19, 0)));
        assert_eq!(parse_node_version("unknown"), None);
    }

    #[test]
    fn sorts_version_directory_names() {
        assert_eq!(version_key(Path::new("v24.14.0")), (24, 14, 0));
        assert_eq!(version_key(Path::new("v9.1.0")), (9, 1, 0));
    }
}
