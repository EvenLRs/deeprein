use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
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

/// 运行时把随壳打包的 Harness 插件装入 web profile 的脚本（scripts/ensure-plugin.mjs）
const ENSURE_PLUGIN_JS: &str = include_str!("../../scripts/ensure-plugin.mjs");

/// 生成注入到 Harness 页面右下角的「重启后端」浮动按钮脚本。
/// 通过 WebviewWindowBuilder::initialization_script 注入，随主 webview 的每个页面执行；
/// 仅在当前页面 origin 与配置的 harness_url origin 一致时显示，启动页（Windows 上的 http://tauri.localhost、macOS/Linux 上的 tauri://）不显示。
/// 按钮不调用 Tauri IPC（远程页面受 ACL 限制），而是导航到魔法路径 /__deeprein_restart__，
/// 由 Rust 侧 on_navigation 拦截并执行后端重启。
fn build_restart_button_js(harness_url: &str) -> String {
    let Ok(url) = harness_url.parse::<tauri::Url>() else {
        return String::new();
    };
    let target_origin = url.origin().ascii_serialization();
    format!(
        r#"
(() => {{
  const targetOrigin = {target_origin:?};
  if (window.location.origin !== targetOrigin) return;
  const mount = () => {{
    const b = document.createElement('button');
    b.textContent = '重启后端';
    Object.assign(b.style, {{
      position: 'fixed', right: '12px', bottom: '12px', zIndex: '2147483647',
      padding: '6px 12px', borderRadius: '8px', border: '1px solid rgba(255,255,255,.22)',
      background: 'rgba(15,25,45,.82)', color: '#e8eef8', cursor: 'pointer',
      fontSize: '12px', fontFamily: 'system-ui, -apple-system, sans-serif',
      boxShadow: '0 2px 10px rgba(0,0,0,.35)', opacity: '.85',
    }});
    b.onmouseenter = () => {{ b.style.opacity = '1'; }};
    b.onmouseleave = () => {{ b.style.opacity = '.85'; }};
    b.addEventListener('click', () => {{
      b.disabled = true; b.textContent = '重启中…';
      window.location.href = '/__deeprein_restart__';
      // 兜底：若 6 秒内未被 Rust 侧拦截处理（正常会刷新页面），还原按钮
      setTimeout(() => {{ b.disabled = false; b.textContent = '重启后端'; }}, 6000);
    }});
    document.body.appendChild(b);
  }};
  if (document.body) mount();
  else document.addEventListener('DOMContentLoaded', mount);
}})();
"#
    )
}

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
    /// 首次安装后端的超时（秒；首次安装需下载全部依赖，可适当放宽）
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
    /// 最近一次后端健康探测结果（窗口标题 / 状态事件展示用）
    backend_health: Mutex<BackendHealth>,
}

/// 互斥锁抗中毒获取 helper：任何线程持锁 panic 都不导致后续调用崩溃
fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 生成 32 字节密码学安全随机 Hex Token，若系统熵源不可用则直接返回 Err
fn generate_bridge_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("系统熵源不可用，无法生成安全 Token: {e}"))?;
    let mut hex = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", b);
    }
    Ok(hex)
}

/// 原子写入 Bridge Token 文件（先写临时文件再 rename）
fn write_bridge_token_atomic(token_path: &Path, token: &str) -> Result<(), String> {
    let dir = token_path
        .parent()
        .ok_or_else(|| "无法获取 token 目录".to_string())?;
    fs::create_dir_all(dir).map_err(|e| format!("创建 token 目录失败: {e}"))?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut rand_bytes = [0u8; 8];
    let _ = getrandom::getrandom(&mut rand_bytes);
    let rand_suffix: u64 = u64::from_le_bytes(rand_bytes);
    let temp_name = format!(".bridge-token.tmp.{pid}.{nanos}.{rand_suffix:016x}");
    let temp_path = dir.join(temp_name);

    let write_res = (|| -> std::io::Result<()> {
        fs::write(&temp_path, token)?;
        fs::rename(&temp_path, token_path)?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("原子写入 token 文件失败: {e}"));
    }

    Ok(())
}

/// 后端健康状态三态：在线（HTTP 2xx）/ 异常（端口通但服务不正常）/ 离线（连不上）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum BackendHealth {
    Online,
    Degraded,
    Offline,
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

/// 首次安装结果（与 ensure-backend.mjs 的 RESULT 行同构）
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

/// Harness 插件安装结果（与 ensure-plugin.mjs 的 RESULT 行同构）
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct PluginEnsureResult {
    ok: bool,
    /// web profile 尚不存在（后端还没首启），需要延后到后端就绪后重试
    profile_missing: bool,
    /// 本次是否真的新装/更新了插件（true 表示后端需重启生效）
    installed: bool,
    error: Option<String>,
}

impl Default for PluginEnsureResult {
    fn default() -> Self {
        Self {
            ok: true,
            profile_missing: false,
            installed: false,
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

/// 若 cand 存在，按 mtime 保留较新的那份 CLI 入口
fn consider_dsh_bin(best: &mut Option<(SystemTime, PathBuf)>, cand: PathBuf) {
    if !cand.exists() {
        return;
    }
    let mtime = fs::metadata(&cand)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let newer = best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true);
    if newer {
        *best = Some((mtime, cand));
    }
}

/// 扫描 npx 缓存与 npm 全局 node_modules，返回较新的 dsh CLI 入口。
/// npx 缓存：root/<hash>/node_modules/@deepseek-ai/dsh/lib/bin.js
/// 全局安装：root/@deepseek-ai/dsh/lib/bin.js（包直接落在 node_modules 根下）
fn scan_dsh_bin(npx_roots: &[PathBuf], global_roots: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for root in npx_roots {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let cand = entry
                    .path()
                    .join("node_modules")
                    .join("@deepseek-ai")
                    .join("dsh")
                    .join("lib")
                    .join("bin.js");
                consider_dsh_bin(&mut best, cand);
            }
        }
    }
    for root in global_roots {
        let cand = root
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        consider_dsh_bin(&mut best, cand);
    }
    best.map(|(_, p)| p)
}

/// 在 npx 缓存与 npm 全局目录里找 @deepseek-ai/dsh 的 CLI 入口（本机已安装的 Harness）
fn find_dsh_bin() -> Option<String> {
    let mut npx_roots: Vec<PathBuf> = Vec::new();
    let mut global_roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            npx_roots.push(PathBuf::from(local).join("npm-cache").join("_npx"));
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            // npm install -g 的全局安装目录：%APPDATA%\npm\node_modules\@deepseek-ai\dsh\...
            global_roots.push(PathBuf::from(appdata).join("npm").join("node_modules"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            npx_roots.push(PathBuf::from(home).join(".npm").join("_npx"));
        }
        // npm install -g 的全局安装目录：/usr/local/lib/node_modules/@deepseek-ai/dsh/...
        global_roots.push(PathBuf::from("/usr/local/lib/node_modules"));
    }
    scan_dsh_bin(&npx_roots, &global_roots).map(|p| p.to_string_lossy().into_owned())
}

/// PATH 上的 `dsh` 可执行文件（npm -g / 用户手动安装）。存在即视为本机已装。
fn find_dsh_in_path() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        #[cfg(windows)]
        {
            for name in ["dsh.cmd", "dsh.exe", "dsh.bat", "dsh"] {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(normalize_path(&cand.to_string_lossy()));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let cand = dir.join("dsh");
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
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

/// 应用管理的后端（首次安装到 app_data_dir/backend，可写；已装则原样使用，不主动升级）
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

/// 随应用分发的 pnpm 入口（bundled 为 tools/node_modules/pnpm/bin/pnpm.cjs，由 bundle-backend.mjs 安装）
fn bundled_pnpm(resource_dir: &Path) -> Option<String> {
    for root in bundled_roots(resource_dir) {
        let cli = root.join("tools").join("node_modules").join("pnpm").join("bin").join("pnpm.cjs");
        if cli.exists() {
            return Some(normalize_path(&cli.to_string_lossy()));
        }
    }
    None
}

/// 随应用分发的插件清单（bundled 为 backend/plugin/manifest.json）
fn bundled_plugin_manifest(resource_dir: &Path) -> Option<PathBuf> {
    for root in bundled_roots(resource_dir) {
        let manifest = root.join("plugin").join("manifest.json");
        if manifest.exists() {
            return Some(manifest);
        }
    }
    None
}

/// 运行 ensure-backend.mjs（内嵌脚本），check_only=true 仅查版本，否则安装（已装则脚本自行跳过升级由调用方保证）。
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

    let mut child = Command::new(&node);
    child
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：node 是控制台程序，从无控制台的 GUI 父进程启动时
        // 若不隐藏会弹出空白 Windows Terminal；CREATE_NEW_PROCESS_GROUP 脱离本客户端
        child.creation_flags(0x0800_0000 | 0x0000_0200);
    }
    let mut child = child
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
            let _ = lock_or_recover(&watched).kill();
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
    let status = lock_or_recover(&child_mut)
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

/// 运行 ensure-plugin.mjs（内嵌脚本），把随壳打包的插件安装进 dsh web profile。
/// profile 尚不存在时返回 profile_missing=true（由调用方在后端首启后再跑一次）。
fn run_plugin_script(
    cfg: &ClientConfig,
    resource_dir: &Path,
    app_data_dir: &Path,
    home_dir: &Path,
) -> Result<PluginEnsureResult, String> {
    // 旧包/开发态没有插件资源时静默跳过（nothing to do）
    let manifest = match bundled_plugin_manifest(resource_dir) {
        Some(m) => m,
        None => return Ok(PluginEnsureResult::default()),
    };
    let pnpm = bundled_pnpm(resource_dir)
        .ok_or_else(|| "未找到随包分发的 pnpm 运行时，无法安装 Harness 插件".to_string())?;
    let node = find_update_node(resource_dir).ok_or_else(|| {
        "未找到可用的 Node 运行时（内置 backend/node 或本机 node），无法安装 Harness 插件"
            .to_string()
    })?;

    let target = app_data_dir.join("plugin");
    fs::create_dir_all(&target)
        .map_err(|e| format!("无法创建插件数据目录 {}: {e}", target.display()))?;
    let script_path = target.join("ensure-plugin.mjs");
    fs::write(&script_path, ENSURE_PLUGIN_JS)
        .map_err(|e| format!("无法写入插件安装脚本 {}: {e}", script_path.display()))?;

    let log_path = target.join("plugin.log");
    let log_file = fs::File::create(&log_path)
        .map_err(|e| format!("无法创建插件安装日志 {}: {e}", log_path.display()))?;
    let stderr_log = log_file
        .try_clone()
        .map_err(|e| format!("插件安装日志句柄错误: {e}"))?;

    let profile = home_dir.join(".dsh").join("profiles").join("web");
    let mut cmd_args = vec![
        normalize_path(&script_path.to_string_lossy()),
        "--profile".into(),
        normalize_path(&profile.to_string_lossy()),
        "--manifest".into(),
        normalize_path(&manifest.to_string_lossy()),
        "--pnpm".into(),
        pnpm,
        "--node".into(),
        node.clone(),
        "--target".into(),
        normalize_path(&target.to_string_lossy()),
    ];
    let mut child = Command::new(&node);
    child
        .args(&mut cmd_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 同更新脚本：隐藏 node 的控制台窗口，避免弹出空白 Terminal
        child.creation_flags(0x0800_0000 | 0x0000_0200);
    }
    let mut child = child
        .spawn()
        .map_err(|e| format!("无法启动插件安装脚本 [{node}]: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取插件安装脚本输出".to_string())?;
    let child_mut = Arc::new(Mutex::new(child));

    // 超时看护：首次安装需要 pnpm 拉取插件依赖，可能耗时数分钟
    let timeout = Duration::from_secs(cfg.update_timeout_sec.max(30));
    {
        let watched = child_mut.clone();
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let _ = lock_or_recover(&watched).kill();
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
    let status = lock_or_recover(&child_mut)
        .wait()
        .map_err(|e| format!("等待插件安装脚本退出失败: {e}"))?;

    if let Some(json) = result_line {
        return serde_json::from_str(&json)
            .map_err(|e| format!("插件安装脚本返回异常 [{json}]: {e}"));
    }
    Err(if !status.success() {
        format!(
            "插件安装超时或失败（{} 秒），详情见 {}",
            timeout.as_secs(),
            log_path.display()
        )
    } else {
        format!("插件安装脚本未返回结果，详情见 {}", log_path.display())
    })
}

/// 解析最终的后端启动命令：
/// 配置覆盖 → 应用管理目录（已装则直接用，不升级）→ PATH 上的 dsh → npx 缓存/全局 node_modules → 空（调用方再走首次安装）
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
    // 1) 应用管理目录里已有 dsh（先前首次安装留下的），原样启动，禁止主动升级
    if let Some(cmd) = managed_backend(app_data_dir, resource_dir, &cfg.harness_url) {
        return cmd;
    }
    // 2) PATH 上的 dsh（npm -g / 用户手动安装）
    if let Some(dsh) = find_dsh_in_path() {
        let mut cmd = vec![dsh, "web".into()];
        push_port(&mut cmd, &cfg.harness_url);
        return cmd;
    }
    // 3) npx 缓存或 npm 全局 node_modules 里的 CLI 入口
    if let Some(node) = find_node() {
        if let Some(bin) = find_dsh_bin() {
            let mut cmd = vec![node, bin, "web".into()];
            push_port(&mut cmd, &cfg.harness_url);
            return cmd;
        }
    }
    // 4) 本机未装：返回空命令，由启动流程走首次安装（ensure-backend.mjs）
    Vec::new()
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

/// 后端健康探测：优先读取 host-bridge 插件的 /__deeprein/health 端点获取真实内部状态。
/// 若端点不可用（404/401/超时等未装插件或旧后端场景），平滑降级为 HTTP GET 根路径探测。
fn check_backend_health(url_str: &str, app_data_dir: Option<&Path>) -> BackendHealth {
    let url = match url_str.parse::<tauri::Url>() {
        Ok(u) => u,
        Err(_) => return BackendHealth::Offline,
    };
    let host = match url.host_str() {
        Some(h) => h.to_string(),
        None => return BackendHealth::Offline,
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a,
        Err(_) => return BackendHealth::Offline,
    };

    let token = app_data_dir.and_then(|dir| {
        let path = dir.join("bridge-token");
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    });

    for addr in addrs {
        let mut stream = match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let host_header = if host.contains(':') {
            format!("[{host}]:{port}")
        } else if port == 80 {
            host.clone()
        } else {
            format!("{host}:{port}")
        };

        // 1. 优先尝试请求 host-bridge 健康检查端点（仅限本地安全地址发送 token，杜绝泄露至远程）
        let is_local_host = host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") || host == "::1";
        if is_local_host {
            if let Some(ref tok) = token {
                let req = format!(
                    "GET /__deeprein/health HTTP/1.1\r\nHost: {host_header}\r\nAuthorization: Bearer {tok}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
                );
                if stream.write_all(req.as_bytes()).is_ok() {
                    let mut resp_bytes = Vec::new();
                    let _ = stream.read_to_end(&mut resp_bytes);
                    let resp_str = String::from_utf8_lossy(&resp_bytes);
                    if let Some((headers, body)) = resp_str.split_once("\r\n\r\n") {
                        let mut parts = headers.lines().next().unwrap_or("").split_whitespace();
                        let _ver = parts.next();
                        let status_code = parts.next().and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
                        if status_code == 200 {
                            #[derive(Deserialize)]
                            struct BridgeHealthResponse {
                                ok: bool,
                                problems: Option<Vec<String>>,
                            }
                            if let Ok(data) = serde_json::from_str::<BridgeHealthResponse>(body.trim()) {
                                if data.ok && data.problems.as_ref().map_or(true, |p| p.is_empty()) {
                                    return BackendHealth::Online;
                                } else {
                                    if let Some(probs) = data.problems {
                                        rust_log(&format!("check_backend_health: host-bridge 报告内部异常: {:?}", probs));
                                    }
                                    return BackendHealth::Degraded;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. 降级回退：手写 HTTP GET 根路径探测三态
        let mut fallback_stream = match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => s,
            Err(_) => return BackendHealth::Degraded,
        };
        let _ = fallback_stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = fallback_stream.set_write_timeout(Some(Duration::from_secs(2)));
        let path = if url.path().is_empty() { "/" } else { url.path() };
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
        );
        if fallback_stream.write_all(request.as_bytes()).is_err() {
            return BackendHealth::Degraded;
        }
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match fallback_stream.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    head.push(byte[0]);
                    if byte[0] == b'\n' || head.len() >= 512 {
                        break;
                    }
                }
            }
        }
        let line = String::from_utf8_lossy(&head);
        let mut parts = line.split_whitespace();
        let _version = parts.next();
        return match parts.next().and_then(|code| code.parse::<u16>().ok()) {
            Some(code) if (200..300).contains(&code) => BackendHealth::Online,
            Some(_) => BackendHealth::Degraded,
            None => BackendHealth::Degraded,
        };
    }
    BackendHealth::Offline
}

/// 探测后端是否可达（启动页轮询用）
#[tauri::command]
fn check_backend(state: State<'_, AppState>) -> bool {
    check_backend_health(&lock_or_recover(&state.config).harness_url, Some(&state.app_data_dir)) == BackendHealth::Online
}

/// 读取当前配置（启动页用于决定探测地址与自动启动行为）
#[tauri::command]
fn get_config(state: State<'_, AppState>) -> ConfigView {
    let cfg = lock_or_recover(&state.config);
    ConfigView {
        harness_url: cfg.harness_url.clone(),
        auto_start_backend: cfg.auto_start_backend,
        start_timeout_sec: cfg.start_timeout_sec,
        start_check_interval_ms: cfg.start_check_interval_ms,
        backend_command: cfg.backend_command.as_ref().map(|v| v.join(" ")),
        check_app_updates: cfg.check_app_updates,
    }
}

/// 启动 Harness 后端：分离进程、无窗口，stdout/stderr 写入 exe 旁 backend.log。
/// 本机未装 dsh 时会在阻塞线程池里走首次安装（可能数分钟），避免卡住 UI。
#[tauri::command]
async fn start_backend(state: State<'_, AppState>) -> Result<BackendStartResult, String> {
    let cfg = lock_or_recover(&state.config).clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();
    let backend_pid = state.backend_pid.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = spawn_backend_impl(&cfg, &resource_dir, &app_data_dir)?;
        if let Some(pid) = result.pid {
            *lock_or_recover(&backend_pid) = Some(pid);
        }
        Ok(result)
    })
    .await
    .map_err(|e| format!("启动后端失败: {e}"))?
}

/// 重启 Harness 后端：杀掉当前监听端口的后端（本客户端启动的实例），
/// 若端口被外部进程占用则返回冲突错误；再按解析器（本机安装优先）重新拉起并等待就绪。
fn restart_backend_impl(state: &AppState) -> Result<BackendStartResult, String> {
    // 互斥锁抗中毒：任何线程先前持锁 panic 都不影响后续重启
    let cfg = lock_or_recover(&state.config).clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();

    let owned = lock_or_recover(&state.backend_pid).is_some();
    let listener = if owned { None } else { listener_pid(&cfg.harness_url) };
    match (owned, listener) {
        (true, _) => {
            let pid = *lock_or_recover(&state.backend_pid);
            if let Some(pid) = pid {
                kill_backend_process(pid);
            }
        }
        (false, Some(pid)) => {
            let name = process_name(pid).unwrap_or_else(|| "未知".into());
            let port_str = cfg.harness_url.parse::<tauri::Url>().ok()
                .and_then(|u| u.port_or_known_default())
                .map(|p| p.to_string())
                .unwrap_or_else(|| cfg.harness_url.clone());
            let err_msg = format!(
                "端口 {port_str} 正被外部进程占用 (PID: {pid}, 映像名: {name})，无法自动重启。请手动退出该进程或在 config.json 中修改 harness_url。"
            );
            rust_log(&format!("restart_backend: {err_msg}"));
            return Err(err_msg);
        }
        (false, None) => {}
    }
    *lock_or_recover(&state.backend_pid) = None;

    let result = match spawn_backend_impl(&cfg, &resource_dir, &app_data_dir) {
        Ok(r) => r,
        Err(e) => {
            rust_log(&format!("restart_backend: 重启失败: {e}"));
            return Err(e);
        }
    };
    *lock_or_recover(&state.backend_pid) = result.pid;

    let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_sec.max(10));
    while Instant::now() < deadline && !backend_reachable(&cfg.harness_url) {
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(result)
}

#[tauri::command]
fn restart_backend(state: State<'_, AppState>) -> Result<BackendStartResult, String> {
    restart_backend_impl(&state)
}

fn spawn_backend_impl(
    cfg: &ClientConfig,
    resource_dir: &Path,
    app_data_dir: &Path,
) -> Result<BackendStartResult, String> {
    let mut cmdline = resolve_backend_command(cfg, resource_dir, app_data_dir);
    if cmdline.is_empty() {
        // 本机未装 dsh：仅在首次缺失时安装，已装绝不主动升级
        rust_log("spawn_backend_impl: 本机未检测到 dsh，开始首次安装");
        let installed = run_update_script(cfg, resource_dir, app_data_dir, false)?;
        if !installed.ok {
            return Err(installed.error.unwrap_or_else(|| {
                "首次安装 DeepSeek Harness 失败，详见 update.log".into()
            }));
        }
        cmdline = resolve_backend_command(cfg, resource_dir, app_data_dir);
        if cmdline.is_empty() {
            return Err("首次安装完成，但仍未找到可用的 dsh 启动入口".into());
        }
    }
    let log_path = exe_dir().join(&cfg.backend_log_file);
    // 重启场景：旧后端进程刚被 taskkill，其 stdout/stderr 句柄可能仍短暂占用 backend.log，
    // 直接 File::create 会报“拒绝访问”。短重试等待句柄释放。
    let mut stdout = None;
    for attempt in 0..12 {
        match fs::File::create(&log_path) {
            Ok(f) => {
                stdout = Some(f);
                break;
            }
            Err(e) if attempt < 11 => {
                rust_log(&format!(
                    "spawn_backend_impl: 日志文件被占用，重试 ({attempt}/11): {e}"
                ));
                std::thread::sleep(Duration::from_millis(400));
            }
            Err(e) => {
                return Err(format!("无法创建后端日志 {}: {e}", log_path.display()));
            }
        }
    }
    let stdout = stdout.expect("重试循环内必然产出日志文件");
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("日志文件错误: {e}"))?;

    let mut cmd = Command::new(&cmdline[0]);
    cmd.args(&cmdline[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    // 生成并注入 host-bridge 认证 token（必须安全成功生成并原子写入，否则拒绝启动）
    let token = generate_bridge_token()?;
    let token_path = app_data_dir.join("bridge-token");
    write_bridge_token_atomic(&token_path, &token)?;
    cmd.env("DEEPREIN_BRIDGE_TOKEN_PATH", &token_path);
    cmd.env("DEEPREIN_APP_DATA_DIR", app_data_dir);

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
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        use std::os::windows::process::CommandExt;
        // taskkill 是控制台程序，同样隐藏窗口避免闪屏
        cmd.creation_flags(0x0800_0000);
        let _ = cmd.status();
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

/// 杀掉外部后端进程（保留供未来确认接管功能使用，当前不再自动调用）
#[allow(dead_code)]
fn kill_external_process(pid: u32) {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
        let _ = cmd.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// 查询指定 PID 的进程名称（映像名）
fn process_name(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // 隐藏 tasklist 控制台窗口
        let out = cmd.output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            // 只认 /FO CSV 的数据行：首字段必为带引号的映像名（"node.exe","1234",...）。
            // 不能靠 starts_with("INFO:") 排除“无匹配”提示——该提示已本地化
            // （中文系统为「信息: 没有运行的任务匹配指定标准。」），前缀判断会失效，
            // 导致把整段提示当成进程名返回。判断引号则与系统语言无关。
            if !trimmed.starts_with('"') {
                continue;
            }
            let name = match trimmed.split(',').next() {
                Some(field) => field.trim_matches('"').trim(),
                None => continue,
            };
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// 找到监听 harness_url 端口的进程 PID（跨平台：Windows netstat / macOS lsof）
fn listener_pid(harness_url: &str) -> Option<u32> {
    let url: tauri::Url = harness_url.parse().ok()?;
    let port = url.port_or_known_default()?;
    #[cfg(windows)]
    {
        let mut cmd = Command::new("netstat");
        cmd.arg("-ano")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // 隐藏 netstat 控制台窗口
        let out = cmd.output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let needle = format!(":{port}");
        for line in text.lines() {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.len() >= 5
                && p[0].eq_ignore_ascii_case("tcp")
                && p[1].ends_with(&needle)
                && p[3].eq_ignore_ascii_case("listening")
            {
                if let Ok(pid) = p[4].parse::<u32>() {
                    if pid != 0 {
                        return Some(pid);
                    }
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("lsof")
            .args(["-ti", &format!("tcp:{port}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().find_map(|l| l.trim().parse::<u32>().ok())
    }
}

/// 插件待生效标记：ensure-plugin 变更过插件（本次安装/版本不一致/bundles reconcile）后
/// 置为“待重启”，后端重启加载插件成功后清除。标记缺失视为待重启（保守：首启先重启一次，
/// 确保旧的外部后端也加载到最新插件）。
fn plugins_pending_flag(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("plugin").join("plugins-ready")
}
fn plugins_pending(app_data_dir: &Path) -> bool {
    !plugins_pending_flag(app_data_dir).exists()
}
fn mark_plugins_pending(app_data_dir: &Path) {
    let _ = fs::remove_file(plugins_pending_flag(app_data_dir));
}
fn mark_plugins_ready(app_data_dir: &Path) {
    let _ = fs::write(plugins_pending_flag(app_data_dir), "1");
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
    let cfg = lock_or_recover(&state.config);
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

/// 把随壳打包的 Harness 插件安装进 dsh web profile（阻塞线程池执行，可能耗时数分钟）
#[tauri::command]
async fn ensure_plugins(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginEnsureResult, String> {
    let cfg = lock_or_recover(&state.config).clone();
    let resource_dir = state.resource_dir.clone();
    let app_data_dir = state.app_data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let home = app
            .path()
            .home_dir()
            .map_err(|e| format!("无法定位用户目录: {e}"))?;
        run_plugin_script(&cfg, &resource_dir, &app_data_dir, &home)
    })
    .await
    .map_err(|e| format!("插件安装失败: {e}"))?
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
    let cfg = lock_or_recover(&state.config).clone();
    let app_data = state.app_data_dir.clone();
    let resource_dir = state.resource_dir.clone();
    let home = window
        .app_handle()
        .path()
        .home_dir()
        .map_err(|e| format!("无法定位用户目录: {e}"))?;

    // 1) 进入 Harness 前先把随壳打包的插件同步进 web profile
    let mut ensure = run_plugin_script(&cfg, &resource_dir, &app_data, &home);
    if matches!(ensure, Ok(ref e) if e.profile_missing) {
        // 后端首启中、profile 尚未生成：稍候重试一次
        std::thread::sleep(Duration::from_secs(3));
        ensure = run_plugin_script(&cfg, &resource_dir, &app_data, &home);
    }
    match ensure {
        Ok(e) if e.installed => mark_plugins_pending(&app_data),
        Err(e) => rust_log(&format!("open_harness: 插件同步未完成，继续打开: {e}")),
        _ => {}
    }

    // 2) 有待生效的插件变更（本次安装过，或上次安装后后端未被重启过）→ 重启后端加载插件。
    //    后端不是本客户端启动的外部实例时，不杀外部进程也不重复 spawn，仅记日志提醒用户手动重启。
    if plugins_pending(&app_data) {
        let owned = lock_or_recover(&state.backend_pid).is_some();
        let listener = if owned { None } else { listener_pid(&cfg.harness_url) };
        let restarted = match (owned, listener) {
            (true, _) => {
                if let Some(pid) = *lock_or_recover(&state.backend_pid) {
                    kill_backend_process(pid);
                }
                spawn_backend_impl(&cfg, &resource_dir, &app_data).ok()
            }
            (false, Some(pid)) => {
                let name = process_name(pid).unwrap_or_else(|| "未知".into());
                rust_log(&format!(
                    "open_harness: 检测到插件更新，但端口被外部实例占用 (PID: {pid}, 映像名: {name})。跳过自动强杀与拉起，请手动重启外部后端以加载新插件。"
                ));
                None
            }
            (false, None) => None, // 无监听者：由启动流程拉起新后端，自然加载插件
        };
        if let Some(ref backend) = restarted {
            *lock_or_recover(&state.backend_pid) = backend.pid;
            let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_sec.max(10));
            while Instant::now() < deadline && !backend_reachable(&cfg.harness_url) {
                std::thread::sleep(Duration::from_secs(1));
            }
            // 仅当本客户端成功重启拉起后端后清除待生效标记
            mark_plugins_ready(&app_data);
        } else if listener.is_none() && !owned {
            // 无监听且未受管（冷启动）：首次拉起时清除标记
            mark_plugins_ready(&app_data);
        }
        // 注意：若端口被外部实例占用 (false, Some(pid))，不写入 ready 标记，保留 pending 以便下次启动重试
    }

    navigate_to_harness(&window, &cfg.harness_url)?;
    // 首次进入 Harness 后启动后端状态监测线程（只启动一次）
    if cfg.monitor_backend && !*lock_or_recover(&state.monitor_started) {
        *lock_or_recover(&state.monitor_started) = true;
        start_backend_monitor(
            window.app_handle().clone(),
            cfg,
            resource_dir,
            app_data,
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
                    if let Some(pid) = *lock_or_recover(&backend_pid) {
                        rust_log(&format!("monitor: 终止旧后端进程组 pid={pid}"));
                        kill_backend_process(pid);
                    }
                    let restart = spawn_backend_impl(&cfg, &resource_dir, &app_data_dir);
                    match restart {
                        Ok(r) => {
                            if let Some(pid) = r.pid {
                                *lock_or_recover(&backend_pid) = Some(pid);
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

/// 后端在线状态标题文案
fn backend_title(health: BackendHealth) -> &'static str {
    match health {
        BackendHealth::Online => "DeepRein · 后端在线",
        BackendHealth::Degraded => "DeepRein · 后端异常",
        BackendHealth::Offline => "DeepRein · 后端离线",
    }
}

/// 后端状态观察线程：定期 HTTP 健康探测，更新窗口标题并广播状态变化事件。
/// 每轮都重设标题：主窗口导航到 Harness 后页面自身的 <title> 会覆盖窗口标题，
/// 这里周期性重申，确保标题始终反映后端状态。
fn start_status_watcher(app: AppHandle, harness_url: String, interval: Duration) {
    std::thread::spawn(move || {
        let mut last: Option<BackendHealth> = None;
        loop {
            let app_data_dir = app.try_state::<AppState>().map(|s| s.app_data_dir.clone());
            let health = check_backend_health(&harness_url, app_data_dir.as_deref());
            if last != Some(health) {
                last = Some(health);
                if let Some(state) = app.try_state::<AppState>() {
                    *lock_or_recover(&state.backend_health) = health;
                }
                let _ = app.emit(
                    "backend-status-changed",
                    serde_json::json!({ "status": health }),
                );
            }
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_title(backend_title(health));
            }
            std::thread::sleep(interval);
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
            let config = load_config();
            let (harness_url, health_interval) = (
                config.harness_url.clone(),
                Duration::from_millis(config.health_check_interval_ms.max(1000)),
            );
            let restart_button_js = build_restart_button_js(&harness_url);
            app.manage(AppState {
                config: Mutex::new(config),
                resource_dir,
                app_data_dir,
                backend_pid: Arc::new(Mutex::new(None)),
                monitor_started: Arc::new(Mutex::new(false)),
                backend_health: Mutex::new(BackendHealth::Offline),
            });
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("DeepRein")
                .inner_size(1280.0, 860.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .initialization_script(&restart_button_js)
                // Harness 页面（远程源）受 ACL 限制无法调自定义命令，
                // 重启按钮改走「导航到魔法路径」由这里拦截执行。
                .on_navigation({
                    let handle = app.handle().clone();
                    move |url| {
                        if url.path().starts_with("/__deeprein_restart__") {
                            let handle = handle.clone();
                            std::thread::spawn(move || {
                                let state = handle.state::<AppState>();
                                match restart_backend_impl(&state) {
                                    Ok(_) => {
                                        if let Some(win) = handle.get_webview_window("main") {
                                            let _ = win.eval("window.location.reload()");
                                        }
                                    }
                                    Err(e) => {
                                        rust_log(&format!("on_navigation: 重启失败: {e}"));
                                        let msg = e.replace('\\', "\\\\").replace('\'', "\\'");
                                        if let Some(win) = handle.get_webview_window("main") {
                                            let _ = win
                                                .eval(&format!("window.alert('重启后端失败：{msg}')"));
                                        }
                                    }
                                }
                            });
                            return false; // 拦截该导航，页面停留原处
                        }
                        true
                    }
                })
                .build()?;
            restore_window(&win);
            let _ = win.set_title("DeepRein · 后端检测中…");
            start_status_watcher(app.handle().clone(), harness_url, health_interval);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            check_backend,
            start_backend,
            restart_backend,
            read_backend_log,
            debug_log,
            open_harness,
            focus_window,
            quit_app,
            read_update_log,
            check_app_update,
            install_app_update,
            ensure_plugins
        ])
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_fake_bin(path: &PathBuf) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(path, "// fake dsh bin\n").unwrap();
    }

    #[test]
    fn scan_dsh_bin_finds_global_layout() {
        let tmp = std::env::temp_dir().join(format!(
            "deeprein-dsh-scan-global-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let global_root = tmp.join("node_modules");
        let bin = global_root
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        write_fake_bin(&bin);

        let found = scan_dsh_bin(&[], &[global_root.clone()]).expect("应找到全局安装的 bin.js");
        assert_eq!(found, bin);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scan_dsh_bin_finds_npx_cache_layout() {
        let tmp = std::env::temp_dir().join(format!(
            "deeprein-dsh-scan-npx-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let npx_root = tmp.join("_npx");
        let bin = npx_root
            .join("abc123hash")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        write_fake_bin(&bin);

        let found = scan_dsh_bin(&[npx_root.clone()], &[]).expect("应找到 npx 缓存里的 bin.js");
        assert_eq!(found, bin);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scan_dsh_bin_global_not_confused_with_npx_layout() {
        let tmp = std::env::temp_dir().join(format!(
            "deeprein-dsh-scan-mix-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let global_root = tmp.join("node_modules");
        // 故意放一个 npx 形态的错误路径，确认全局扫描不会去拼 <entry>/node_modules/...
        let wrong = global_root
            .join("@deepseek-ai")
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        write_fake_bin(&wrong);
        let right = global_root
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        write_fake_bin(&right);

        let found = scan_dsh_bin(&[], &[global_root.clone()]).expect("应命中全局直铺路径");
        assert_eq!(found, right);

        let _ = fs::remove_dir_all(&tmp);
    }
}
