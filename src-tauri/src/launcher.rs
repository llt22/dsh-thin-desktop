use crate::discovery::discover_node_toolchain;
use serde::Serialize;
use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Url};

const MAX_LOG_LINES: usize = 1000;
const DEFAULT_PROFILE: &str = "web";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: &str = "0";

#[derive(Clone)]
pub struct LauncherState {
    inner: Arc<LauncherInner>,
}

struct LauncherInner {
    data: Mutex<RuntimeData>,
    process_exited: Condvar,
    next_generation: AtomicU64,
}

struct RuntimeData {
    current: Option<ProcessInfo>,
    dsh_url: Option<String>,
    logs: VecDeque<String>,
    runtime: Option<String>,
    status: String,
}

#[derive(Clone, Copy)]
struct ProcessInfo {
    generation: u64,
    pid: u32,
    stopping: bool,
}

struct LaunchSpec {
    executable: PathBuf,
    args: Vec<String>,
    path_env: Option<OsString>,
    label: String,
    runtime: String,
}

#[derive(Serialize)]
pub struct RuntimeSnapshot {
    status: String,
    runtime: Option<String>,
    logs: Vec<String>,
    dsh_url: Option<String>,
}

impl LauncherState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LauncherInner {
                data: Mutex::new(RuntimeData {
                    current: None,
                    dsh_url: None,
                    logs: VecDeque::new(),
                    runtime: None,
                    status: "正在准备...".to_string(),
                }),
                process_exited: Condvar::new(),
                next_generation: AtomicU64::new(1),
            }),
        }
    }

    pub fn start(&self, app: &AppHandle) -> Result<(), String> {
        {
            let data = self.inner.data.lock().map_err(lock_error)?;
            if data.current.is_some() {
                return Err("DSH 已经在运行".to_string());
            }
        }

        self.set_status(app, "正在查找本地 Node 和 npx");
        let launch = build_launch_spec()?;
        self.set_runtime(app, launch.runtime.clone());
        self.append_log(app, format!("[thin-desktop] runtime: {}", launch.runtime));
        self.append_log(app, format!("[thin-desktop] launching: {}", launch.label));
        self.append_log(
            app,
            format!(
                "[thin-desktop] args: {}",
                launch
                    .args
                    .iter()
                    .map(|arg| format!("{arg:?}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );

        let mut command = Command::new(&launch.executable);
        command
            .args(&launch.args)
            .current_dir(home_dir()?)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path_env) = launch.path_env {
            command.env("PATH", path_env);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 DSH：{error}"))?;
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        {
            let mut data = self.inner.data.lock().map_err(lock_error)?;
            data.current = Some(ProcessInfo {
                generation,
                pid,
                stopping: false,
            });
            data.dsh_url = None;
        }
        self.set_status(app, "正在启动 DSH Web");

        if let Some(stdout) = stdout {
            self.read_output(app.clone(), generation, stdout, "dsh:stdout");
        }
        if let Some(stderr) = stderr {
            self.read_output(app.clone(), generation, stderr, "dsh:stderr");
        }
        self.wait_for_exit(app.clone(), generation, child);
        Ok(())
    }

    pub fn restart(&self, app: &AppHandle) -> Result<(), String> {
        self.set_status(app, "正在重启 DSH");
        self.stop_current(app)?;
        self.start(app)
    }

    pub fn stop_current(&self, app: &AppHandle) -> Result<(), String> {
        let process = {
            let mut data = self.inner.data.lock().map_err(lock_error)?;
            let Some(mut process) = data.current else {
                return Ok(());
            };
            process.stopping = true;
            data.current = Some(process);
            process
        };

        self.append_log(
            app,
            format!("[thin-desktop] stopping DSH process {}", process.pid),
        );
        terminate_process_tree(process.pid, false)?;
        if self.wait_until_stopped(process.generation, Duration::from_secs(3))? {
            return Ok(());
        }

        self.append_log(
            app,
            format!("[thin-desktop] force stopping DSH process {}", process.pid),
        );
        terminate_process_tree(process.pid, true)?;
        if self.wait_until_stopped(process.generation, Duration::from_secs(2))? {
            Ok(())
        } else {
            Err(format!("DSH 进程 {} 未能退出", process.pid))
        }
    }

    pub fn snapshot(&self) -> Result<RuntimeSnapshot, String> {
        let data = self.inner.data.lock().map_err(lock_error)?;
        Ok(RuntimeSnapshot {
            status: data.status.clone(),
            runtime: data.runtime.clone(),
            logs: data.logs.iter().cloned().collect(),
            dsh_url: data.dsh_url.clone(),
        })
    }

    pub fn diagnostics(&self) -> Result<String, String> {
        let snapshot = self.snapshot()?;
        Ok([
            "DSH Thin Desktop diagnostics".to_string(),
            format!("platform={}", env::consts::OS),
            format!("arch={}", env::consts::ARCH),
            format!(
                "runtime={}",
                snapshot
                    .runtime
                    .unwrap_or_else(|| "<not-found>".to_string())
            ),
            format!(
                "dshUrl={}",
                snapshot
                    .dsh_url
                    .unwrap_or_else(|| "<not-ready>".to_string())
            ),
            String::new(),
            snapshot.logs.join(""),
        ]
        .join("\n"))
    }

    pub fn set_start_error(&self, app: &AppHandle, error: String) {
        self.set_status(app, format!("启动失败：{error}"));
    }

    fn read_output<R>(&self, app: AppHandle, generation: u64, reader: R, stream: &'static str)
    where
        R: Read + Send + 'static,
    {
        let state = self.clone();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                match line {
                    Ok(line) => {
                        state.append_log(&app, format!("[{stream}] {line}"));
                        if let Some(url) = extract_dsh_url(&line) {
                            state.handle_dsh_url(app.clone(), generation, url);
                        }
                    }
                    Err(error) => {
                        state.append_log(&app, format!("[{stream}] 读取日志失败：{error}"));
                        break;
                    }
                }
            }
        });
    }

    fn handle_dsh_url(&self, app: AppHandle, generation: u64, url: Url) {
        let should_open = {
            let Ok(mut data) = self.inner.data.lock() else {
                return;
            };
            let is_current = data
                .current
                .is_some_and(|process| process.generation == generation && !process.stopping);
            if !is_current || data.dsh_url.is_some() {
                false
            } else {
                data.dsh_url = Some(url.to_string());
                true
            }
        };
        if !should_open {
            return;
        }

        self.set_status(&app, format!("已发现 DSH 地址，正在等待服务：{url}"));
        let state = self.clone();
        thread::spawn(move || match wait_for_http(&url, Duration::from_secs(30)) {
            Ok(()) => {
                if !state.is_current(generation) {
                    return;
                }
                state.set_status(&app, format!("正在打开 {url}"));
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(error) = window.navigate(url) {
                        state.set_status(&app, format!("打开 DSH 页面失败：{error}"));
                    }
                }
            }
            Err(error) => state.set_status_if_current(&app, generation, error),
        });
    }

    fn wait_for_exit(&self, app: AppHandle, generation: u64, mut child: Child) {
        let state = self.clone();
        thread::spawn(move || {
            let result = child.wait();
            let (was_current, was_stopping) = {
                let Ok(mut data) = state.inner.data.lock() else {
                    return;
                };
                let Some(process) = data.current else {
                    return;
                };
                if process.generation != generation {
                    return;
                }
                data.current = None;
                data.dsh_url = None;
                state.inner.process_exited.notify_all();
                (true, process.stopping)
            };
            if !was_current {
                return;
            }
            match result {
                Ok(status) if was_stopping => {
                    state.append_log(&app, format!("[thin-desktop] DSH stopped: {status}"));
                }
                Ok(status) => state.set_status(&app, format!("DSH 意外退出：{status}")),
                Err(error) => state.set_status(&app, format!("等待 DSH 退出失败：{error}")),
            }
        });
    }

    fn wait_until_stopped(&self, generation: u64, timeout: Duration) -> Result<bool, String> {
        let data = self.inner.data.lock().map_err(lock_error)?;
        let (data, _) = self
            .inner
            .process_exited
            .wait_timeout_while(data, timeout, |data| {
                data.current
                    .is_some_and(|process| process.generation == generation)
            })
            .map_err(lock_error)?;
        Ok(!data
            .current
            .is_some_and(|process| process.generation == generation))
    }

    fn is_current(&self, generation: u64) -> bool {
        self.inner
            .data
            .lock()
            .ok()
            .and_then(|data| data.current)
            .is_some_and(|process| process.generation == generation && !process.stopping)
    }

    fn set_status_if_current(&self, app: &AppHandle, generation: u64, status: String) {
        if self.is_current(generation) {
            self.set_status(app, status);
        }
    }

    fn append_log(&self, app: &AppHandle, line: String) {
        let line = if line.ends_with('\n') {
            line
        } else {
            line + "\n"
        };
        if let Ok(mut data) = self.inner.data.lock() {
            data.logs.push_back(line.clone());
            if data.logs.len() > MAX_LOG_LINES {
                data.logs.pop_front();
            }
        }
        let _ = app.emit("dsh-log", line);
    }

    fn set_status(&self, app: &AppHandle, status: impl Into<String>) {
        let status = status.into();
        if let Ok(mut data) = self.inner.data.lock() {
            data.status = status.clone();
        }
        self.append_log(app, format!("[thin-desktop] {status}"));
        let _ = app.emit("dsh-status", status);
    }

    fn set_runtime(&self, app: &AppHandle, runtime: String) {
        if let Ok(mut data) = self.inner.data.lock() {
            data.runtime = Some(runtime.clone());
        }
        let _ = app.emit("dsh-runtime", runtime);
    }
}

fn build_launch_spec() -> Result<LaunchSpec, String> {
    let profile = env_value("DSH_PROFILE", DEFAULT_PROFILE);
    let host = env_value("DSH_HOST", DEFAULT_HOST);
    let port = env_value("DSH_PORT", DEFAULT_PORT);
    validate_host_and_port(&host, &port)?;
    let extra_args = parse_extra_args()?;
    let mut args = vec![
        "--profile".to_string(),
        profile.clone(),
        "--host".to_string(),
        host,
        "--port".to_string(),
        port,
    ];
    args.extend(extra_args);

    if let Some(executable) = env::var_os("DSH_EXECUTABLE") {
        let executable = PathBuf::from(executable);
        if !executable.is_file() {
            return Err(format!("DSH_EXECUTABLE 不存在：{}", executable.display()));
        }
        return Ok(LaunchSpec {
            label: format!("{} --profile {profile}", executable.display()),
            runtime: format!("DSH executable · {}", executable.display()),
            executable,
            args,
            path_env: None,
        });
    }

    let toolchain = discover_node_toolchain()?;
    let package = match env::var("DSH_VERSION")
        .ok()
        .filter(|value| !value.is_empty())
    {
        Some(version) => format!("@deepseek-ai/dsh@{version}"),
        None => "@deepseek-ai/dsh@latest".to_string(),
    };
    let mut npx_args = vec!["-y".to_string(), package.clone()];
    npx_args.extend(args);
    Ok(LaunchSpec {
        executable: toolchain.npx.clone(),
        args: npx_args,
        path_env: Some(toolchain.path_env),
        label: format!(
            "{} -y {package} --profile {profile}",
            toolchain.npx.display()
        ),
        runtime: format!(
            "Node {} · {} · {}",
            toolchain.node_version,
            toolchain.source,
            toolchain.node.display()
        ),
    })
}

fn validate_host_and_port(host: &str, port: &str) -> Result<(), String> {
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(format!("DSH_HOST 必须是本机回环地址，当前值为 {host}"));
    }
    port.parse::<u16>()
        .map(|_| ())
        .map_err(|_| format!("DSH_PORT 必须是 0 到 65535 之间的整数，当前值为 {port}"))
}

fn parse_extra_args() -> Result<Vec<String>, String> {
    let Some(value) = env::var("DSH_EXTRA_ARGS")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    let args =
        shell_words::split(&value).map_err(|error| format!("DSH_EXTRA_ARGS 格式错误：{error}"))?;
    validate_extra_args(&args)?;
    Ok(args)
}

fn validate_extra_args(args: &[String]) -> Result<(), String> {
    const PROTECTED_OPTIONS: [&str; 2] = ["--host", "--port"];
    if let Some(option) = args.iter().find_map(|arg| {
        PROTECTED_OPTIONS
            .iter()
            .find(|option| arg == **option || arg.starts_with(&format!("{option}=")))
    }) {
        return Err(format!(
            "DSH_EXTRA_ARGS 不能覆盖 {option}；请使用对应的 DSH 环境变量"
        ));
    }
    Ok(())
}

fn env_value(name: &str, fallback: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "找不到用户 HOME 目录".to_string())
}

fn extract_dsh_url(line: &str) -> Option<Url> {
    let marker = line.find("dsh web:")?;
    let candidate = line[marker + "dsh web:".len()..]
        .split_whitespace()
        .next()?;
    let url = Url::parse(candidate.trim_end_matches([',', ';', ')', ']'])).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match url.host_str()? {
        "127.0.0.1" | "localhost" | "::1" => Some(url),
        _ => None,
    }
}

fn wait_for_http(url: &Url, timeout: Duration) -> Result<(), String> {
    let host = url
        .host_str()
        .ok_or_else(|| "DSH URL 缺少 host".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "DSH URL 缺少端口".to_string())?;
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("无法解析 DSH 地址：{error}"))?
        .collect();
    if addresses.is_empty() {
        return Err(format!("无法解析 DSH 地址 {host}:{port}"));
    }

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if addresses
            .iter()
            .any(|address| TcpStream::connect_timeout(address, Duration::from_millis(700)).is_ok())
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!("等待 DSH Web 就绪超时：{url}"))
}

#[cfg(unix)]
fn terminate_process_tree(pid: u32, force: bool) -> Result<(), String> {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(format!("停止 DSH 进程组失败：{error}"))
        }
    }
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32, force: bool) -> Result<(), String> {
    let mut command = Command::new("taskkill");
    command.args(["/PID", &pid.to_string(), "/T"]);
    if force {
        command.arg("/F");
    }
    let status = command
        .status()
        .map_err(|error| format!("无法运行 taskkill：{error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("taskkill 执行失败：{status}"))
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> String {
    format!("运行状态锁已损坏：{error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_loopback_dsh_urls() {
        assert_eq!(
            extract_dsh_url("dsh web: http://127.0.0.1:3210").map(|url| url.to_string()),
            Some("http://127.0.0.1:3210/".to_string())
        );
        assert_eq!(
            extract_dsh_url("ready - dsh web: http://localhost:8080/path")
                .map(|url| url.to_string()),
            Some("http://localhost:8080/path".to_string())
        );
    }

    #[test]
    fn rejects_non_loopback_and_non_http_urls() {
        assert!(extract_dsh_url("dsh web: https://example.com:8080").is_none());
        assert!(extract_dsh_url("dsh web: file:///tmp/index.html").is_none());
    }

    #[test]
    fn parses_quoted_extra_arguments() {
        env::set_var("DSH_EXTRA_ARGS", "--name 'desktop profile' --verbose");
        assert_eq!(
            parse_extra_args().unwrap(),
            vec!["--name", "desktop profile", "--verbose"]
        );
        env::remove_var("DSH_EXTRA_ARGS");
    }

    #[test]
    fn validates_loopback_bindings() {
        assert!(validate_host_and_port("127.0.0.1", "0").is_ok());
        assert!(validate_host_and_port("localhost", "65535").is_ok());
        assert!(validate_host_and_port("0.0.0.0", "3000").is_err());
        assert!(validate_host_and_port("127.0.0.1", "70000").is_err());
    }

    #[test]
    fn rejects_network_overrides_in_extra_arguments() {
        for args in [
            vec!["--host".to_string(), "0.0.0.0".to_string()],
            vec!["--host=0.0.0.0".to_string()],
            vec!["--port".to_string(), "3000".to_string()],
            vec!["--port=3000".to_string()],
        ] {
            assert!(validate_extra_args(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn allows_unrelated_extra_arguments() {
        let args = vec!["--verbose".to_string(), "--name=desktop".to_string()];
        assert!(validate_extra_args(&args).is_ok());
    }
}
