use crate::discovery::discover_node_toolchain;
use serde::Serialize;
use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::webview::cookie::SameSite;
use tauri::webview::Cookie;
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

    pub fn owns_url(&self, url: &Url) -> bool {
        self.inner
            .data
            .lock()
            .ok()
            .and_then(|data| {
                data.dsh_url
                    .as_deref()
                    .and_then(|value| Url::parse(value).ok())
            })
            .is_some_and(|dsh_url| dsh_url.origin() == url.origin())
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
                // 存 origin 基地址（去掉 token），owns_url 仅比对 origin，且避免 token 泄进 UI 日志
                data.dsh_url = Some(base_url(&url).to_string());
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
                let Some(window) = app.get_webview_window("main") else {
                    return;
                };
                // 新版 dsh web 强制会话鉴权。若直接把带 token 的地址交给 webview，
                // dsh 会 303 跳到 /，而这一跳的发起源是 tauri 应用页（异源），
                // WKWebView 不会在异源跳转上回传 SameSite=Strict 的会话 cookie → 401。
                // 因此先在 Rust 侧用 token 换取会话 cookie，以 SameSite=Lax 注入
                // webview，再同源加载 /（无 token），让 Lax cookie 随顶层导航发送。
                let destination = match exchange_token_for_cookie(&url) {
                    Ok(cookies) => {
                        let domain = url.host_str().unwrap_or("127.0.0.1").to_string();
                        let mut injected: Option<String> = None;
                        for (name, value) in cookies {
                            let cookie = Cookie::build((name.clone(), value))
                                .domain(domain.clone())
                                .path("/")
                                .http_only(true)
                                .same_site(SameSite::Lax)
                                .build();
                            match window.set_cookie(cookie) {
                                Ok(()) => injected = Some(name),
                                Err(error) => state.append_log(
                                    &app,
                                    format!("[thin-desktop] 注入鉴权 cookie 失败：{error}"),
                                ),
                            }
                        }
                        // set_cookie 在 WKWebView 上异步落库，导航前确认已写入
                        if let Some(name) = injected {
                            wait_for_cookie_written(&window, &base_url(&url), &name);
                        }
                        base_url(&url)
                    }
                    Err(error) => {
                        state.append_log(
                            &app,
                            format!("[thin-desktop] 换取鉴权 cookie 失败，回退直接导航：{error}"),
                        );
                        url.clone()
                    }
                };
                state.set_status(&app, format!("正在打开 {destination}"));
                if let Err(error) = window.navigate(destination) {
                    state.set_status(&app, format!("打开 DSH 页面失败：{error}"));
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
        // 桌面壳自带窗口，禁止 dsh 另开系统浏览器抢占同一个鉴权 token
        "--no-open".to_string(),
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

/// 去掉 token 与 fragment、路径归一为 `/` 的 origin 基地址。
fn base_url(url: &Url) -> Url {
    let mut base = url.clone();
    base.set_query(None);
    base.set_fragment(None);
    base.set_path("/");
    base
}

/// 用带 token 的地址向 dsh 发一次明文 HTTP GET，取回它下发的会话 cookie。
/// 只读响应头，返回全部 `Set-Cookie` 的 name/value（无 cookie 则空列表，如旧版不鉴权）。
fn exchange_token_for_cookie(url: &Url) -> Result<Vec<(String, String)>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "DSH URL 缺少 host".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "DSH URL 缺少端口".to_string())?;
    let mut target = url.path().to_string();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }

    let address = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("解析 DSH 地址失败：{error}"))?
        .next()
        .ok_or_else(|| format!("无法解析 DSH 地址 {host}:{port}"))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|error| format!("连接 DSH 失败：{error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("设置读取超时失败：{error}"))?;

    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("发送鉴权请求失败：{error}"))?;
    stream
        .flush()
        .map_err(|error| format!("刷新鉴权请求失败：{error}"))?;

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("读取鉴权响应失败：{error}"))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&buffer) {
            buffer.truncate(end);
            break;
        }
        if buffer.len() > 64 * 1024 {
            break;
        }
    }

    Ok(parse_set_cookies(&String::from_utf8_lossy(&buffer)))
}

/// 在字节缓冲里定位 HTTP 头结束标记 `\r\n\r\n`。
fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 从响应头文本里解析所有 `Set-Cookie` 的 `name=value`（大小写不敏感，取到首个 `;` 为止）。
fn parse_set_cookies(headers: &str) -> Vec<(String, String)> {
    const PREFIX: &str = "set-cookie:";
    headers
        .lines()
        .filter_map(|line| {
            let head = line.get(..PREFIX.len())?;
            if !head.eq_ignore_ascii_case(PREFIX) {
                return None;
            }
            let pair = line[PREFIX.len()..].split(';').next()?.trim();
            let (name, value) = pair.split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                None
            } else {
                Some((name.to_string(), value.trim().to_string()))
            }
        })
        .collect()
}

/// set_cookie 在 WKWebView 上异步落库，导航前轮询确认目标 cookie 已写入（有上限）。
fn wait_for_cookie_written<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    url: &Url,
    name: &str,
) {
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        let present = window
            .cookies_for_url(url.clone())
            .map(|cookies| cookies.iter().any(|cookie| cookie.name() == name))
            .unwrap_or(false);
        if present || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
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

    #[test]
    fn recognizes_only_the_active_dsh_origin() {
        let state = LauncherState::new();
        state.inner.data.lock().unwrap().dsh_url =
            Some("http://127.0.0.1:3210/conversation".to_string());

        assert!(state.owns_url(&Url::parse("http://127.0.0.1:3210/settings").unwrap()));
        assert!(!state.owns_url(&Url::parse("http://127.0.0.1:9999/").unwrap()));
        assert!(!state.owns_url(&Url::parse("https://example.com/").unwrap()));
    }

    #[test]
    fn parses_set_cookie_headers_case_insensitively() {
        let headers = "HTTP/1.1 303 See Other\r\nlocation: /\r\nset-cookie: dsh-auth-abc=v1.tokenvalue; Max-Age=2592000; Path=/; HttpOnly; SameSite=Strict\r\nSet-Cookie: other=1; Path=/";
        assert_eq!(
            parse_set_cookies(headers),
            vec![
                ("dsh-auth-abc".to_string(), "v1.tokenvalue".to_string()),
                ("other".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_responses_without_set_cookie() {
        let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n";
        assert!(parse_set_cookies(headers).is_empty());
    }

    #[test]
    fn base_url_strips_token_and_path() {
        let url = Url::parse("http://127.0.0.1:52956/some/path?token=secret#frag").unwrap();
        assert_eq!(base_url(&url).to_string(), "http://127.0.0.1:52956/");
    }
}
