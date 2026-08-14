# deeprein — DeepSeek Harness 桌面客户端

一个基于 **Rust + Tauri v2** 的原生桌面应用，把本机运行的
[DeepSeek Harness](http://127.0.0.1:3080) Web GUI 包装成独立的桌面窗口应用。
支持 **Windows x86_64** 与 **macOS arm64 (Apple Silicon)**。

## 特性

- 🪟 **原生窗口**：Windows 用 WebView2、macOS 用 WKWebView（系统内核），无捆绑浏览器，安装包小、内存占用低。
- 🔌 **连接检测 + 自动启动后端**：启动时自动探测 Harness（默认 `127.0.0.1:3080`）；未运行时自动拉起 `dsh web` 后端并等待就绪，失败时给出友好提示与重试。
- 📦 **内置官方 Harness 后端**：应用自带 Node 运行时 + 官方 `@deepseek-ai/dsh` 包——本机已安装则优先用本机，未安装则自动启动内置后端，开箱即用。
- 🎨 **深色启动页**：原生 UI 质感，检测成功后自动导航进入 Harness。
- ⚙️ **可配置**：应用旁的 `config.json` 可改地址、启动命令、超时等。

## 自动启动后端

应用检测不到 Harness 时，会自动执行后端启动命令并轮询等待服务就绪，然后进入 GUI。

**后端命令解析顺序（本机安装优先）**：

1. `config.json` 里的 `backend_command`（显式覆盖）
2. **本机已安装的 Harness**：npx 缓存里的 dsh CLI（Windows: `%LOCALAPPDATA%\npm-cache\_npx`；macOS: `~/.npm/_npx`）或 npm 全局安装（`%APPDATA%\npm\node_modules` / `/usr/local/lib/node_modules`）→ `node <dsh>/lib/bin.js web`
3. **内置后端**（随应用打包）：`backend/node`（Node 运行时）+ `backend/dsh`（官方 `@deepseek-ai/dsh`）→ `node <内置>/lib/bin.js web --port <harness_url 端口>`
4. 兜底：Windows `cmd /C "dsh web || npx -y @deepseek-ai/dsh web"`，macOS/Linux `sh -c "dsh web || npx -y @deepseek-ai/dsh web"`

后端进程**分离运行**（独立于客户端存活），输出写入应用旁的 `backend.log`，启动页超时会显示日志尾部供排障。
地址、超时等通过应用旁的 `config.json` 调整（首次运行会自动生成默认文件）：

```json
{
  "harness_url": "http://127.0.0.1:3080",
  "backend_command": null,
  "backend_cwd": null,
  "auto_start_backend": true,
  "start_timeout_sec": 90,
  "start_check_interval_ms": 1500,
  "backend_log_file": "backend.log"
}
```

示例（自定义启动命令）：

```json
{ "backend_command": ["C:\\Program Files\\nodejs\\node.exe", "C:\\path\\to\\bin.js", "web"] }
```

## 环境要求

| 依赖 | 说明 |
| --- | --- |
| Windows 10/11 | 目标平台 |
| WebView2 运行时 | Win11 自带；Win10 一般已预装，缺失时应用首次运行会提示下载 |
| Rust 工具链 | `https://rustup.rs`；MSVC 构建需要 Visual Studio Build Tools（C++ 工作负载），MinGW 构建需要 `stable-x86_64-pc-windows-gnu` 工具链 + MinGW-w64 |
| Node.js (可选) | 仅打包 NSIS 安装程序时需要（用于 `@tauri-apps/cli`） |

### 方式一：MSVC（推荐）

安装 rustup 时选 MSVC 目标，并安装 Visual Studio Build Tools 的「使用 C++ 的桌面开发」工作负载（提供 `link.exe`）。

### 方式二：MinGW（免装 VS）

适合无管理员权限的环境（如本机沙箱）：

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
# 下载 w64devkit 或 winlibs 的 MinGW-w64（提供 gcc/ld/windres），解压后把 bin 加入 PATH
$env:PATH = "<mingw>\bin;$env:PATH"
cd src-tauri
cargo +stable-x86_64-pc-windows-gnu build --release
```

> 注：`src-tauri/.cargo/config.toml` 已为 GNU target 指定 `linker = "gcc"`（MSVC 构建不受影响）。
> 若用 w64devkit，其缺少 `libgcc_eh.a`，需在 `lib\gcc\x86_64-w64-mingw32\<版本>\` 下建一个空桩：
> `ar rcs libgcc_eh.a`（winlibs 自带该库，无需此步）。
> ⚠️ **项目路径含空格时，GNU 构建会在图标资源步骤失败**（binutils `windres` 的已知缺陷，`gcc -E` 预处理不处理带空格路径）。
> 解决办法：把 target 指到无空格目录再构建，例如
> `$env:CARGO_TARGET_DIR = "$env:TEMP\dsh-target"`，之后 exe 在 `$env:TEMP\dsh-target\release\` 下。
> MSVC 构建无此限制。

### 方式三：macOS（Apple Silicon）

```bash
# 需要 Xcode Command Line Tools（xcode-select --install）
rustup target add aarch64-apple-darwin
cd src-tauri
cargo build --release --target aarch64-apple-darwin
```

## GitHub Actions 自动打包

仓库内置 `.github/workflows/build.yml`，双平台矩阵自动构建：

| 平台 | Runner | 产物 |
| --- | --- | --- |
| Windows x86_64 | `windows-latest` | NSIS 安装器 `.exe`（+MSI） |
| macOS arm64 | `macos-14`（Apple Silicon） | `.app` + `.dmg` |

- **推送 `v*` 标签** 或 **手动触发**（Actions 页 → Build Desktop Apps → Run workflow）→ 双平台构建（含内置后端打包），产物上传到 Artifacts（保留 90 天）；**不再自动发布 Release**。
- 构建无需配置证书；macOS 产物未签名，首次打开需右键 → 打开（或 `xattr -dr com.apple.quarantine`）。

## 目录结构

```
deeprein/
├── dist/                       # 本地启动页（连接检测/重试/退出）
│   └── index.html
├── backend/                    # 内置后端（scripts/bundle-backend.mjs 生成，不入库）
│   ├── node/                   # Node 运行时（Windows: node.exe；macOS: bin/node）
│   └── dsh/node_modules/       # 官方 @deepseek-ai/dsh 及其运行时依赖
├── src-tauri/                  # Rust + Tauri 工程
│   ├── src/
│   │   ├── main.rs             # 程序入口（隐藏控制台窗口）
│   │   └── lib.rs              # 窗口创建 + 后端解析/启动命令
│   ├── icons/                  # 应用图标（脚本生成）
│   ├── capabilities/default.json
│   ├── Cargo.toml
│   └── tauri.conf.json
├── scripts/
│   ├── bundle-backend.mjs      # 下载 Node 运行时 + 安装官方 dsh（打包前运行）
│   ├── generate-icons.ps1      # 重新生成图标
│   └── build.ps1               # 构建辅助脚本
└── package.json                # 仅供 @tauri-apps/cli 使用
```

## 快速开始

### 1. 编译（免安装 exe）

```powershell
.\scripts\build.ps1 -Mode Release
# 产物：src-tauri\target\release\deeprein.exe
```

或手动：

```powershell
cd src-tauri
cargo build --release
```

### 2. 打包 NSIS 安装程序（可选）

```powershell
.\scripts\build.ps1 -Mode Installer
# 产物：src-tauri\target\release\bundle\nsis\*.exe
```

### 3. 运行

先确保 DeepSeek Harness 已启动（`http://127.0.0.1:3080` 可访问），然后直接运行 exe。
应用会检测服务状态，自动进入 GUI；若未启动会显示提示页，可点击「重试」。

> 窗口内按 `F5` / `Ctrl+R` 可刷新；Harness 端口变更时，修改
> `src-tauri/src/lib.rs` 与 `dist/index.html` 中的 `HARNESS_URL` 后重新编译。

## 常见问题

- **提示“未检测到 DeepSeek Harness 服务”**：客户端会先尝试自动启动后端（`dsh web`）；若失败，确认 `config.json` 的 `backend_command` 与 `harness_url` 是否正确，或手动在终端运行 `dsh web`。
- **应用直接打开了 Harness 页面、启动页（连接检测）不生效**：说明构建时未启用 `tauri/custom-protocol` feature——没有它 release 会按 dev 模式运行，webview 直接加载 `devUrl`。`src-tauri/Cargo.toml` 已正确配置 `features = ["custom-protocol"]`，请勿移除。
- **编译报错找不到 `link.exe`**：未安装 MSVC 构建工具，安装 Visual Studio Build Tools 的 C++ 工作负载。
- **第一次编译很慢**：Tauri 依赖较多，首次需拉取并编译数百个 crate，属正常现象。
