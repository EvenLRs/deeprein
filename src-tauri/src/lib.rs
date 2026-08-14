use std::fs;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 默认 Harness Web GUI 地址
const DEFAULT_HARNESS_URL: &str = "http://127.0.0.1:3080";

/// 客户端配置（读取 exe 旁的 config.json；缺省用内置默认值）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ClientConfig {
    /// Harness Web GUI 地址
    harness_url: String,
    /// 后端启动命令（显式覆盖，数组形式；留 null 则自动探测 dsh CLI）
    backend_command: Option<Vec<String>>,
    /// 后端工作目录
    backend_cwd: Option<String>,
    /// 检测不到后端时是否自动启动
    auto_start_backend: bool,
    /// 启动后端后等待服务就绪的超时（秒）
    start_timeout_sec: u64,
    /// 探测间隔（毫秒）
    start_check_interval_ms: u64,
    /// 后端日志文件名（相对 exe 目录）
    backend_log_file: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            harness_url: DEFAULT_HARNESS_URL.to_string(),
            backend_command: None,
            backend_cwd: None,
            auto_start_backend: true,
            start_timeout_sec: 90,
            start_check_interval_ms: 1500,
            backend_log_file: "backend.log".into(),
        }
    }
}

struct AppState {
    config: Mutex<ClientConfig>,
    /// Tauri 资源目录（打包时 bundle.resources 的落点，macOS 为 .app/Contents/Resources）
    resource_dir: PathBuf,
}

#[derive(Serialize)]
struct ConfigView {
    harness_url: String,
    auto_start_backend: bool,
    start_timeout_sec: u64,
    start_check_interval_ms: u64,
    backend_command: Option<String>,
}

#[derive(Serialize)]
struct BackendStartResult {
    started: bool,
    pid: Option<u32>,
    command: String,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 读取 exe 旁的 config.json；文件缺失或解析失败时写入默认配置并返回默认值
fn load_config() -> ClientConfig {
    let path = exe_dir().join("config.json");
    if let Ok(text) = fs::read_to_string(&path) {
        // 兼容带 BOM 的 UTF-8 文件（记事本等编辑器保存时可能带 BOM）
        let text = text.trim_start_matches('\u{feff}');
        if let Ok(cfg) = serde_json::from_str::<ClientConfig>(text) {
            return cfg;
        }
    }
    let cfg = ClientConfig::default();
    if let Ok(json) = serde_json::to_string_pretty(&cfg) {
        let _ = fs::write(&path, json);
    }
    cfg
}

fn find_node() -> Option<String> {
    #[cfg(windows)]
    for p in [
        "C:\\Program Files\\nodejs\\node.exe",
        "C:\\Program Files (x86)\\nodejs\\node.exe",
    ] {
        if Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    // 兜底：依赖 PATH 上的 node（macOS/Linux 及已配置 PATH 的 Windows）
    Some("node".to_string())
}

/// 在 npx 缓存与 npm 全局目录里找 @deepseek-ai/dsh 的 CLI 入口（本机已安装的 Harness）
fn find_dsh_bin() -> Option<String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("npm-cache").join("_npx"));
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            // npm install -g 的全局安装目录
            roots.push(PathBuf::from(appdata).join("npm").join("node_modules"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(home).join(".npm").join("_npx"));
        }
        roots.push(PathBuf::from("/usr/local/lib/node_modules"));
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for root in roots {
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let cand = entry
                    .path()
                    .join("node_modules")
                    .join("@deepseek-ai")
                    .join("dsh")
                    .join("lib")
                    .join("bin.js");
                if cand.exists() {
                    let mtime = fs::metadata(&cand)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    let newer = best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true);
                    if newer {
                        best = Some((mtime, cand));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p.to_string_lossy().into_owned())
}

/// Windows 的 resource_dir 会带 `\\?\` 长路径前缀，node 等子进程无法解析，需剥掉
fn normalize_path(s: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    s.to_string()
}

/// 随应用打包的官方 Harness 后端（backend/node + backend/dsh）
fn bundled_backend(resource_dir: &Path, harness_url: &str) -> Option<Vec<String>> {
    #[cfg(windows)]
    let node = resource_dir
        .join("backend")
        .join("node")
        .join("node.exe");
    #[cfg(not(windows))]
    let node = resource_dir
        .join("backend")
        .join("node")
        .join("bin")
        .join("node");
    let bin = resource_dir
        .join("backend")
        .join("dsh")
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    if node.exists() && bin.exists() {
        let mut cmd = vec![
            normalize_path(&node.to_string_lossy()),
            normalize_path(&bin.to_string_lossy()),
            "web".into(),
        ];
        // 让内置后端服务与 harness_url 相同的端口
        if let Ok(url) = harness_url.parse::<tauri::Url>() {
            if let Some(port) = url.port() {
                cmd.push("--port".into());
                cmd.push(port.to_string());
            }
        }
        Some(cmd)
    } else {
        None
    }
}

/// 解析最终的后端启动命令：
/// 配置覆盖 → 本机已安装的 Harness（优先）→ 内置（随应用打包）→ PATH/npx 兜底
fn resolve_backend_command(cfg: &ClientConfig, resource_dir: &Path) -> Vec<String> {
    if let Some(cmd) = &cfg.backend_command {
        if !cmd.is_empty() {
            return cmd.clone();
        }
    }
    // 1) 本机已安装的 Harness（优先使用本地安装）
    if let Some(node) = find_node() {
        if let Some(bin) = find_dsh_bin() {
            return vec![node, bin, "web".into()];
        }
    }
    // 2) 内置（随应用打包）的官方 Harness
    if let Some(cmd) = bundled_backend(resource_dir, &cfg.harness_url) {
        return cmd;
    }
    // 3) 兜底：PATH 上的 dsh，或 npx 现场拉取
    #[cfg(windows)]
    let fallback = vec![
        "cmd".into(),
        "/C".into(),
        "dsh web || npx -y @deepseek-ai/dsh web".into(),
    ];
    #[cfg(not(windows))]
    let fallback = vec![
        "sh".into(),
        "-c".into(),
        "dsh web || npx -y @deepseek-ai/dsh web".into(),
    ];
    fallback
}

/// 读取当前配置（启动页用于决定探测地址与自动启动行为）
#[tauri::command]
fn get_config(state: State<'_, AppState>) -> ConfigView {
    let cfg = state.config.lock().unwrap();
    ConfigView {
        harness_url: cfg.harness_url.clone(),
        auto_start_backend: cfg.auto_start_backend,
        start_timeout_sec: cfg.start_timeout_sec,
        start_check_interval_ms: cfg.start_check_interval_ms,
        backend_command: cfg.backend_command.as_ref().map(|v| v.join(" ")),
    }
}

/// 启动 Harness 后端：分离进程、无窗口，stdout/stderr 写入 exe 旁 backend.log
#[tauri::command]
fn start_backend(state: State<'_, AppState>) -> Result<BackendStartResult, String> {
    let cfg = state.config.lock().unwrap().clone();
    let resource_dir = state.resource_dir.clone();
    let cmdline = resolve_backend_command(&cfg, &resource_dir);
    if cmdline.is_empty() {
        return Err("未配置后端启动命令".into());
    }
    let log_path = exe_dir().join(&cfg.backend_log_file);
    let stdout = fs::File::create(&log_path)
        .map_err(|e| format!("无法创建后端日志 {}: {e}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("日志文件错误: {e}"))?;

    let mut cmd = Command::new(&cmdline[0]);
    cmd.args(&cmdline[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(cwd) = &cfg.backend_cwd {
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP：无窗口、脱离本客户端独立存活
        cmd.creation_flags(0x0800_0000 | 0x0000_0200);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 新进程组：脱离终端/父进程，独立存活
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("启动后端失败 [{}]: {e}", cmdline.join(" ")))?;
    Ok(BackendStartResult {
        started: true,
        pid: Some(child.id()),
        command: cmdline.join(" "),
    })
}

/// 读取后端日志尾部（排障用）
#[tauri::command]
fn read_backend_log(state: State<'_, AppState>, lines: Option<usize>) -> String {
    let cfg = state.config.lock().unwrap();
    let log_path = exe_dir().join(&cfg.backend_log_file);
    let n = lines.unwrap_or(40).max(1);
    match fs::read_to_string(&log_path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(n);
            all[start..].join("\n")
        }
        Err(_) => "(日志尚未生成)".to_string(),
    }
}

/// 启动页调试日志（写入 exe 旁 launcher.log）
#[tauri::command]
fn debug_log(text: String) {
    use std::io::Write;
    let path = exe_dir().join("launcher.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", text);
    }
}

/// 让主窗口从启动页导航到 Harness 页面
#[tauri::command]
fn open_harness(window: WebviewWindow, state: State<'_, AppState>) -> Result<(), String> {
    let url_str = state.config.lock().unwrap().harness_url.clone();
    let url: tauri::Url = url_str
        .parse()
        .map_err(|e| format!("无效的 Harness 地址 {url_str}: {e}"))?;
    window.navigate(url).map_err(|e| format!("导航失败: {e}"))?;
    // 抵消 WebView2 初始化/导航时偶发的窗口最小化竞态
    restore_window(&window);
    let win = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        restore_window(&win);
    });
    Ok(())
}

/// 主动恢复并聚焦主窗口（启动页可在需要时调用）
#[tauri::command]
fn focus_window(window: WebviewWindow) {
    restore_window(&window);
}

fn restore_window(window: &WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// 退出应用
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| exe_dir());
            app.manage(AppState {
                config: Mutex::new(load_config()),
                resource_dir,
            });
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("deeprein")
                .inner_size(1280.0, 860.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .build()?;
            restore_window(&win);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            start_backend,
            read_backend_log,
            debug_log,
            open_harness,
            focus_window,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
