use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

/// 默认 Harness Web GUI 地址
const DEFAULT_HARNESS_URL: &str = "http://127.0.0.1:3080";

/// 默认 npm registry 上 dsh 包的地址（版本查询用）
const DEFAULT_REGISTRY_URL: &str = "https://registry.npmjs.org/@deepseek-ai/dsh";

/// 运行时安装/更新后端脚本（scripts/ensure-backend.mjs），随二进制内嵌，无需额外资源文件
const ENSURE_BACKEND_JS: &str = include_str!("../../scripts/ensure-backend.mjs");

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
    /// 每次启动时联网检查 DeepSeek Harness 更新
    check_updates: bool,
    /// 发现新版本时自动下载安装（同步更新）
    auto_update: bool,
    /// 安装/更新后端的超时（秒；首次安装需下载全部依赖，可适当放宽）
    update_timeout_sec: u64,
    /// npm registry 上 dsh 包的地址（版本查询用）
    registry_url: String,
    /// 每次启动时检查 DeepRein 壳自身的更新
    check_app_updates: bool,
    /// 打开 Harness 后是否持续监测后端状态（离线超阈值弹窗提示重启）
    monitor_backend: bool,
    /// 后端连续不通多久后弹窗提示（秒，默认 90）
    backend_down_threshold_sec: u64,
    /// 后端状态监测间隔（毫秒）
    health_check_interval_ms: u64,
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
            check_updates: true,
            auto_update: true,
            update_timeout_sec: 600,
            registry_url: DEFAULT_REGISTRY_URL.to_string(),
            check_app_updates: true,
            monitor_backend: true,
            backend_down_threshold_sec: 90,
            health_check_interval_ms: 5000,
        }
    }
}

struct AppState {
    config: Mutex<ClientConfig>,
    /// Tauri 资源目录（打包时 bundle.resources 的落点，macOS 为 .app/Contents/Resources）
    resource_dir: PathBuf,
    /// 应用数据目录（自动更新后的后端存放于此，可写；如 macOS ~/Library/Application Support/com.deeprein.client）
    app_data_dir: PathBuf,
    /// 由本客户端启动的后端进程 pid（进程组组长；重启后端时先杀旧进程）
    backend_pid: Arc<Mutex<Option<u32>>>,
    /// 后端状态监测线程是否已启动（防止重复启动）
    monitor_started: Arc<Mutex<bool>>,
}

#[derive(Serialize)]
struct ConfigView {
    harness_url: String,
    auto_start_backend: bool,
    start_timeout_sec: u64,
    start_check_interval_ms: u64,
    backend_command: Option<String>,
    check_app_updates: bool,
}

#[derive(Serialize)]
struct BackendStartResult {
    started: bool,
    pid: Option<u32>,
    command: String,
}

/// 当前后端版本与更新开关信息（启动页展示用）
#[derive(Serialize)]
struct UpdateInfo {
    check_updates: bool,
    auto_update: bool,
    update_timeout_sec: u64,
    registry_url: String,
    /// 当前可用后端的版本（应用管理目录 → 内置 → 本机安装）
    current_version: Option<String>,
    /// 版本来源：managed | bundled | local | none
    current_source: String,
}

/// 更新检查/安装结果（与 ensure-backend.mjs 的 RESULT 行同构）
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct UpdateCheck {
    ok: bool,
    checked: bool,
    current: Option<String>,
    latest: Option<String>,
    update_available: bool,
    updated: bool,
    error: Option<String>,
}

/// DeepRein 壳自身的可用更新信息（tauri-plugin-updater 检查结果）
#[derive(Debug, Serialize)]
struct AppUpdateInfo {
    version: String,
    current_version: String,
    body: Option<String>,
    date: Option<String>,
}

impl Default for UpdateCheck {
    fn default() -> Self {
        Self {
            ok: false,
            checked: false,
            current: None,
            latest: None,
            update_available: false,
            updated: false,
            error: None,
        }
    }
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

/// 随应用打包的后端（backend/node + backend/dsh）候选根目录。
/// 打包器对含 ".." 的资源路径（如 ../backend）会映射为 _up_/backend；
/// resource_dir() 本身不含 _up_，因此多候选根目录兜底：
///  1) resource_dir/_up_/backend   —— tauri 打包后的正式布局（macOS/Windows）
///  2) resource_dir/backend        —— 无 ".." 的资源配置或手动放置
///  3) exe_dir/backend             —— 开发构建/手动复制到 exe 旁
fn bundled_roots(resource_dir: &Path) -> Vec<PathBuf> {
    vec![
        resource_dir.join("_up_").join("backend"),
        resource_dir.join("backend"),
        exe_dir().join("backend"),
    ]
}

/// 内置后端里 Node 可执行文件的相对路径
fn bundled_node_rel() -> PathBuf {
    #[cfg(windows)]
    let rel = ["node", "node.exe"].iter().collect::<PathBuf>();
    #[cfg(not(windows))]
    let rel = ["node", "bin", "node"].iter().collect::<PathBuf>();
    rel
}

/// 内置后端里 dsh CLI 入口的相对路径
fn dsh_bin_rel() -> PathBuf {
    ["dsh", "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"]
        .iter()
        .collect::<PathBuf>()
}

/// 让后端服务与 harness_url 相同的端口
fn push_port(cmd: &mut Vec<String>, harness_url: &str) {
    if let Ok(url) = harness_url.parse::<tauri::Url>() {
        if let Some(port) = url.port() {
            cmd.push("--port".into());
            cmd.push(port.to_string());
        }
    }
}

/// 随应用打包的官方 Harness 后端（backend/node + backend/dsh）
fn bundled_backend(resource_dir: &Path, harness_url: &str) -> Option<Vec<String>> {
    for root in bundled_roots(resource_dir) {
        let node = root.join(bundled_node_rel());
        let bin = root.join(dsh_bin_rel());
        if node.exists() && bin.exists() {
            let mut cmd = vec![
                normalize_path(&node.to_string_lossy()),
                normalize_path(&bin.to_string_lossy()),
                "web".into(),
            ];
            push_port(&mut cmd, harness_url);
            return Some(cmd);
        }
    }
    None
}

/// 应用管理的后端（自动更新安装到 app_data_dir/backend，可写）
fn managed_backend(
    app_data_dir: &Path,
    resource_dir: &Path,
    harness_url: &str,
) -> Option<Vec<String>> {
    let bin = app_data_dir.join("backend").join(dsh_bin_rel());
    if !bin.exists() {
        return None;
    }
    let node = find_update_node(resource_dir)?;
    let mut cmd = vec![
        normalize_path(&node),
        normalize_path(&bin.to_string_lossy()),
        "web".into(),
    ];
    push_port(&mut cmd, harness_url);
    Some(cmd)
}

/// 读取 package.json 的 version 字段
fn read_pkg_version(pkg_path: &Path) -> Option<String> {
    let text = fs::read_to_string(pkg_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("version").and_then(|x| x.as_str()).map(String::from)
}

/// 读取 bundle-info.json 的 dsh 版本字段（打包脚本与更新脚本都会写入）
fn read_bundle_info_version(info_path: &Path) -> Option<String> {
    let text = fs::read_to_string(info_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("dsh").and_then(|x| x.as_str()).map(String::from)
}

/// 定位可运行 ensure-backend.mjs 的 Node：内置（自带 npm，优先）→ 本机 PATH/标准目录
fn find_update_node(resource_dir: &Path) -> Option<String> {
    for root in bundled_roots(resource_dir) {
        let node = root.join(bundled_node_rel());
        if node.exists() {
            return Some(normalize_path(&node.to_string_lossy()));
        }
    }
    // find_node() 总返回 Some（兜底 "node"），交由脚本自行找 npm
    find_node()
}

/// 当前可用的后端版本与来源：应用管理目录 → 内置 → 本机安装 → 无
fn current_backend_version(
    app_data_dir: &Path,
    resource_dir: &Path,
) -> (Option<String>, &'static str) {
    let managed_pkg = app_data_dir
        .join("backend")
        .join("dsh")
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("package.json");
    if let Some(v) = read_pkg_version(&managed_pkg) {
        return (Some(v), "managed");
    }
    for root in bundled_roots(resource_dir) {
        if let Some(v) = read_bundle_info_version(&root.join("bundle-info.json")) {
            return (Some(v), "bundled");
        }
    }
    if let Some(bin) = find_dsh_bin() {
        // .../node_modules/@deepseek-ai/dsh/lib/bin.js → package.json 在 dsh/ 目录下
        let pkg = Path::new(&bin)
            .parent()
            .and_then(|p| p.parent())
            .map(|d| d.join("package.json"));
        if let Some(p) = pkg {
            if let Some(v) = read_pkg_version(&p) {
                return (Some(v), "local");
            }
        }
    }
    (None, "none")
}

/// 运行 ensure-backend.mjs（内嵌脚本），check_only=true 仅查版本，否则安装/更新。
/// 输出写入 app_data_dir/backend/update.log；解析最后的 RESULT 行返回。
fn run_update_script(
    cfg: &ClientConfig,
    resource_dir: &Path,
    app_data_dir: &Path,
    check_only: bool,
) -> Result<UpdateCheck, String> {
    let node = find_update_node(resource_dir).ok_or_else(|| {
        "未找到可用的 Node 运行时（内置 backend/node 或本机 node），无法联网检查/更新 Harness"
            .to_string()
    })?;
    let target = app_data_dir.join("backend");
    fs::create_dir_all(&target)
        .map_err(|e| format!("无法创建后端数据目录 {}: {e}", target.display()))?;

    // 每次运行前刷新内嵌脚本（脚本随客户端版本升级）
    let script_path = target.join("ensure-backend.mjs");
    fs::write(&script_path, ENSURE_BACKEND_JS)
        .map_err(|e| format!("无法写入更新脚本 {}: {e}", script_path.display()))?;

    let log_path = target.join("update.log");
    let log_file = fs::File::create(&log_path)
        .map_err(|e| format!("无法创建更新日志 {}: {e}", log_path.display()))?;
    let stderr_log = log_file
        .try_clone()
        .map_err(|e| format!("更新日志句柄错误: {e}"))?;

    let mut args = vec![
        normalize_path(&script_path.to_string_lossy()),
        "--target".into(),
        normalize_path(&target.to_string_lossy()),
        "--registry".into(),
        cfg.registry_url.clone(),
        "--node".into(),
        node.clone(),
    ];
    if check_only {
        args.push("--check-only".into());
    }

    let mut child = Command::new(&node)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_log))
        .spawn()
        .map_err(|e| format!("无法启动更新脚本 [{node}]: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取更新脚本输出".to_string())?;
    let child_mut = Arc::new(Mutex::new(child));

    // 超时看护：超过 update_timeout_sec 杀掉脚本进程（首次安装可能下载数百 MB 依赖）
    let timeout = Duration::from_secs(cfg.update_timeout_sec.max(30));
    {
        let watched = child_mut.clone();
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let _ = watched.lock().unwrap().kill();
        });
    }

    let mut log_writer = log_file;
    let mut result_line: Option<String> = None;
    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(l) => {
                let _ = writeln!(log_writer, "{l}");
                if let Some(payload) = l.strip_prefix("RESULT ") {
                    result_line = Some(payload.to_string());
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let status = child_mut
        .lock()
        .unwrap()
        .wait()
        .map_err(|e| format!("等待更新脚本退出失败: {e}"))?;

    if let Some(json) = result_line {
        let mut parsed: UpdateCheck = serde_json::from_str(&json)
            .map_err(|e| format!("更新脚本返回异常 [{json}]: {e}"))?;
        parsed.checked = true;
        return Ok(parsed);
    }
    let timed_out = !status.success();
    Ok(UpdateCheck {
        ok: false,
        checked: true,
        error: Some(if timed_out {
            format!(
                "更新/安装超时（{} 秒），详情见 {}",
                timeout.as_secs(),
                log_path.display()
            )
        } else {
            format!("更新脚本未返回结果，详情见 {}", log_path.display())
        }),
        ..UpdateCheck::default()
    })
}

/// 解析最终的后端启动命令：
/// 配置覆盖 → 应用管理（自动更新目录，优先）→ 本机已安装 → 内置 → PATH/npx 兜底
fn resolve_backend_command(
    cfg: &ClientConfig,
    resource_dir: &Path,
    app_data_dir: &Path,
) -> Vec<String> {
    if let Some(cmd) = &cfg.backend_command {
        if !cmd.is_empty() {
            return cmd.clone();
        }
    }
    // 1) 应用管理的后端（首次启动/自动更新生成，跟随最新版本）
    if let Some(cmd) = managed_backend(app_data_dir, resource_dir, &cfg.harness_url) {
        return cmd;
    }
    // 2) 本机已安装的 Harness（优先使用本地安装）
    if let Some(node) = find_node() {
        if let Some(bin) = find_dsh_bin() {
            return vec![node, bin, "web".into()];
        }
    }
    // 3) 内置（随应用打包）的官方 Harness
    if let Some(cmd) = bundled_backend(resource_dir, &cfg.harness_url) {
        return cmd;
    }
    // 4) 兜底：PATH 上的 dsh，或 npx 现场拉取
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

/// 后端地址是否可达（TCP 连接探测，2 秒超时）。
/// 不能依赖启动页 JS 的 fetch：macOS 上 WKWebView 会拦截自定义协议页面
/// （tauri://localhost）向 http 地址发起的跨源请求。
fn backend_reachable(url_str: &str) -> bool {
    let url = match url_str.parse::<tauri::Url>() {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = match url.host_str() {
        Some(h) => h.to_string(),
        None => return false,
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a,
        Err(_) => return false,
    };
    for addr in addrs {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_ok() {
            return true;
        }
    }
    false
}

/// 探测后端是否可达（启动页轮询用）
#[tauri::command]
fn check_backend(state: State<'_, AppState>) -> bool {
    backend_reachable(&state.config.lock().unwrap().harness_url)
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
        check_app_updates: cfg.check_app_updates,
    }
}

/// 启动 Harness 后端：分离进程、无窗口，stdout/stderr 写入 exe 旁 backend.log
#[tauri::command]
fn start_backend(state: State<'_, AppState>) -> Result<BackendStartResult, String> {
    let cfg = state.config.lock().unwrap().clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();
    let result = spawn_backend_impl(&cfg, &resource_dir, &app_data_dir)?;
    // 记录进程组组长 pid，供后端状态监测重启时先杀旧进程
    if let Some(pid) = result.pid {
        *state.backend_pid.lock().unwrap() = Some(pid);
    }
    Ok(result)
}

fn spawn_backend_impl(
    cfg: &ClientConfig,
    resource_dir: &Path,
    app_data_dir: &Path,
) -> Result<BackendStartResult, String> {
    let cmdline = resolve_backend_command(cfg, resource_dir, app_data_dir);
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

/// 杀掉由本客户端启动的后端进程（连同整个进程组/子进程树）
fn kill_backend_process(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        // 后端以新进程组启动，负数 pid 表示杀掉整个进程组
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Rust 侧调试日志（写入 exe 旁 launcher.log，与启动页 debug_log 同文件）
fn rust_log(text: &str) {
    let path = exe_dir().join("launcher.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", text);
    }
}

/// 读取后端日志尾部（排障用）
#[tauri::command]
fn read_backend_log(state: State<'_, AppState>, lines: Option<usize>) -> String {
    let cfg = state.config.lock().unwrap();
    let log_path = exe_dir().join(&cfg.backend_log_file);
    read_log_tail(&log_path, lines)
}

/// 读取更新日志尾部（app_data_dir/backend/update.log，启动页展示更新进度用）
#[tauri::command]
fn read_update_log(state: State<'_, AppState>, lines: Option<usize>) -> String {
    let log_path = state.app_data_dir.join("backend").join("update.log");
    read_log_tail(&log_path, lines)
}

fn read_log_tail(log_path: &Path, lines: Option<usize>) -> String {
    let n = lines.unwrap_or(40).max(1);
    match fs::read_to_string(log_path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(n);
            all[start..].join("\n")
        }
        Err(_) => "(日志尚未生成)".to_string(),
    }
}

/// 当前后端版本与更新开关（启动页先调用，用于展示与决定是否检查更新）
#[tauri::command]
fn get_update_info(state: State<'_, AppState>) -> UpdateInfo {
    let cfg = state.config.lock().unwrap();
    let (current_version, current_source) =
        current_backend_version(&state.app_data_dir, &state.resource_dir);
    UpdateInfo {
        check_updates: cfg.check_updates,
        auto_update: cfg.auto_update,
        update_timeout_sec: cfg.update_timeout_sec,
        registry_url: cfg.registry_url.clone(),
        current_version,
        current_source: current_source.to_string(),
    }
}

/// 联网检查 DeepSeek Harness 是否有新版本（运行 ensure-backend.mjs --check-only）。
/// 放到阻塞线程池执行，避免阻塞主线程（同步命令会卡 UI）。
#[tauri::command]
async fn check_update(state: State<'_, AppState>) -> Result<UpdateCheck, String> {
    let cfg = state.config.lock().unwrap().clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_update_script(&cfg, &resource_dir, &app_data_dir, true)
    })
    .await
    .map_err(|e| format!("检查更新失败: {e}"))?
}

/// 安装/更新 DeepSeek Harness 后端到应用数据目录（首次安装或同步到最新版）。
/// 可能耗时数分钟（下载依赖），同样放到阻塞线程池；进度见 update.log（read_update_log 轮询）。
#[tauri::command]
async fn install_update(state: State<'_, AppState>) -> Result<UpdateCheck, String> {
    let cfg = state.config.lock().unwrap().clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_update_script(&cfg, &resource_dir, &app_data_dir, false)
    })
    .await
    .map_err(|e| format!("安装/更新失败: {e}"))?
}

/// 检查 DeepRein 壳自身的更新（tauri-plugin-updater）。
/// 返回 None 表示已是最新版本或端点不可用。
#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<Option<AppUpdateInfo>, String> {
    let updater = app.updater().map_err(|e| format!("updater 初始化失败: {e}"))?;
    let update = updater.check().await.map_err(|e| format!("检查更新失败: {e}"))?;
    Ok(update.map(|u| AppUpdateInfo {
        version: u.version,
        current_version: u.current_version,
        body: u.body,
        date: u.date.map(|d| d.to_string()),
    }))
}

/// 下载并安装 DeepRein 壳更新。
/// 下载进度通过 app-update-progress 事件发给启动页；
/// macOS：安装完成后自动重启加载新版本；Windows(NSIS)：安装器接管后退出本进程。
#[tauri::command]
async fn install_app_update(app: AppHandle) -> Result<(), String> {
    // macOS：应用若从只读 DMG 直接运行，更新替换会失败并可能把应用包留在损坏状态
    // （图标问号、双击无反应）。下载前先拦截；同时记下包路径，安装完成后解除隔离。
    #[cfg(target_os = "macos")]
    let macos_bundle: Option<PathBuf> = {
        let exe = std::env::current_exe().map_err(|e| format!("无法定位当前应用: {e}"))?;
        let bundle = exe
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| "无法定位 .app 包路径".to_string())?;
        if bundle.starts_with("/Volumes/") {
            return Err(
                "当前从只读磁盘镜像(DMG)运行，无法原地更新。请先把 DeepRein.app 拖入「应用程序」后再更新。"
                    .into(),
            );
        }
        Some(bundle.to_path_buf())
    };
    let updater = app.updater().map_err(|e| format!("updater 初始化失败: {e}"))?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("检查更新失败: {e}"))?
        .ok_or_else(|| "没有可用的更新".to_string())?;

    let events_progress = app.clone();
    let events_done = app.clone();
    let result = update
        .download_and_install(
            move |chunk_length, content_length| {
                let _ = events_progress.emit(
                    "app-update-progress",
                    serde_json::json!({ "chunk": chunk_length, "total": content_length }),
                );
            },
            move || {
                let _ = events_done.emit("app-update-downloaded", ());
            },
        )
        .await
        .map_err(|e| format!("下载/安装更新失败: {e}"));

    // Windows(NSIS)：安装器已接管，退出本进程；
    // macOS：插件替换 .app 后需重启进程加载新版本
    #[cfg(windows)]
    app.exit(0);
    #[cfg(target_os = "macos")]
    {
        if result.is_ok() {
            // 更新包经网络下载、解压替换，可能携带隔离属性；解除后再重启，
            // 避免新版本被 Gatekeeper 判为「已损坏/无法验证」。
            if let Some(bundle) = macos_bundle {
                let _ = std::process::Command::new("xattr")
                    .args(["-cr"])
                    .arg(&bundle)
                    .status();
            }
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
            app.exit(0);
        }
    }
    result
}

/// 启动页调试日志（写入 exe 旁 launcher.log）
#[tauri::command]
fn debug_log(text: String) {
    let path = exe_dir().join("launcher.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", text);
    }
}

/// 让主窗口从启动页导航到 Harness 页面
#[tauri::command]
fn open_harness(window: WebviewWindow, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.config.lock().unwrap().clone();
    navigate_to_harness(&window, &cfg.harness_url)?;
    // 首次进入 Harness 后启动后端状态监测线程（只启动一次）
    if cfg.monitor_backend && !*state.monitor_started.lock().unwrap() {
        *state.monitor_started.lock().unwrap() = true;
        start_backend_monitor(
            window.app_handle().clone(),
            cfg,
            state.resource_dir.clone(),
            state.app_data_dir.clone(),
            state.backend_pid.clone(),
        );
    }
    Ok(())
}

/// 导航主窗口到 Harness 页面（抵消 WebView2 初始化/导航时偶发的窗口最小化竞态）
fn navigate_to_harness(window: &WebviewWindow, url_str: &str) -> Result<(), String> {
    let url: tauri::Url = url_str
        .parse()
        .map_err(|e| format!("无效的 Harness 地址 {url_str}: {e}"))?;
    window.navigate(url).map_err(|e| format!("导航失败: {e}"))?;
    restore_window(window);
    let win = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        restore_window(&win);
    });
    Ok(())
}

/// 后端状态监测：定时 TCP 探测；连续不通超过阈值（默认 90 秒）时弹窗询问是否重启后端。
/// 用户确认后杀掉旧后端进程组 → 重新启动 → 等待就绪 → 重新打开 Harness 页面。
fn start_backend_monitor(
    app: AppHandle,
    cfg: ClientConfig,
    resource_dir: PathBuf,
    app_data_dir: PathBuf,
    backend_pid: Arc<Mutex<Option<u32>>>,
) {
    let interval = Duration::from_millis(cfg.health_check_interval_ms.max(1000));
    let threshold = Duration::from_secs(cfg.backend_down_threshold_sec.max(10));
    let harness_url = cfg.harness_url.clone();
    std::thread::spawn(move || {
        let mut down_since: Option<Instant> = None;
        loop {
            std::thread::sleep(interval);
            let reachable = backend_reachable(&harness_url);
            match (reachable, &down_since) {
                (true, Some(_)) => {
                    rust_log("monitor: 后端已恢复在线");
                    down_since = None;
                }
                (true, None) => {}
                (false, None) => {
                    rust_log("monitor: 后端无响应，开始计时");
                    down_since = Some(Instant::now());
                }
                (false, Some(since)) => {
                    if since.elapsed() < threshold {
                        continue;
                    }
                    rust_log(&format!(
                        "monitor: 后端已离线 {} 秒（阈值 {} 秒），弹出重启询问",
                        since.elapsed().as_secs(),
                        threshold.as_secs()
                    ));
                    // 无论用户如何选择，重置计时避免连续弹窗；稍后再断则重新计满阈值再问
                    down_since = None;
                    let confirmed = app
                        .dialog()
                        .message(format!(
                            "DeepSeek Harness 后端已 {} 秒无响应。\n是否重启后端？",
                            threshold.as_secs()
                        ))
                        .title("后端无响应")
                        .kind(MessageDialogKind::Warning)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "重启后端".into(),
                            "稍后再说".into(),
                        ))
                        .blocking_show();
                    if !confirmed {
                        rust_log("monitor: 用户选择稍后再说");
                        continue;
                    }
                    rust_log("monitor: 用户选择重启后端");
                    if let Some(pid) = *backend_pid.lock().unwrap() {
                        rust_log(&format!("monitor: 终止旧后端进程组 pid={pid}"));
                        kill_backend_process(pid);
                    }
                    let restart = spawn_backend_impl(&cfg, &resource_dir, &app_data_dir);
                    match restart {
                        Ok(r) => {
                            if let Some(pid) = r.pid {
                                *backend_pid.lock().unwrap() = Some(pid);
                            }
                            rust_log("monitor: 已重新启动后端，等待就绪…");
                            let deadline = Instant::now()
                                + Duration::from_secs(cfg.start_timeout_sec.max(10));
                            let mut ok = false;
                            while Instant::now() < deadline {
                                std::thread::sleep(Duration::from_secs(1));
                                if backend_reachable(&harness_url) {
                                    ok = true;
                                    break;
                                }
                            }
                            if ok {
                                rust_log("monitor: 重启成功，重新打开 Harness 页面");
                                if let Some(win) = app.get_webview_window("main") {
                                    let _ = navigate_to_harness(&win, &harness_url);
                                }
                            } else {
                                rust_log("monitor: 重启后等待就绪超时");
                                let _ = app
                                    .dialog()
                                    .message(format!(
                                        "后端已重新启动，但 {} 秒内仍未就绪。\n请查看应用旁的 backend.log，或手动在终端运行 dsh web。",
                                        cfg.start_timeout_sec
                                    ))
                                    .title("后端重启未就绪")
                                    .kind(MessageDialogKind::Error)
                                    .buttons(MessageDialogButtons::Ok)
                                    .blocking_show();
                            }
                        }
                        Err(e) => {
                            rust_log(&format!("monitor: 重启后端失败: {e}"));
                            let _ = app
                                .dialog()
                                .message(format!("重启后端失败：{e}"))
                                .title("后端重启失败")
                                .kind(MessageDialogKind::Error)
                                .buttons(MessageDialogButtons::Ok)
                                .blocking_show();
                        }
                    }
                }
            }
        }
    });
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
            // 可写的应用数据目录：自动更新的后端安装于此
            let app_data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| exe_dir().join("data"));
            app.manage(AppState {
                config: Mutex::new(load_config()),
                resource_dir,
                app_data_dir,
                backend_pid: Arc::new(Mutex::new(None)),
                monitor_started: Arc::new(Mutex::new(false)),
            });
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("DeepRein")
                .inner_size(1280.0, 860.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .build()?;
            restore_window(&win);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            check_backend,
            start_backend,
            read_backend_log,
            debug_log,
            open_harness,
            focus_window,
            quit_app,
            get_update_info,
            check_update,
            install_update,
            read_update_log,
            check_app_update,
            install_app_update
        ])
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
