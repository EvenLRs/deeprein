# DeepRein — DeepSeek Harness 桌面客户端

一个基于 **Rust + Tauri v2** 的原生桌面应用，把本机运行的
[DeepSeek Harness](http://127.0.0.1:3080) Web GUI 包装成独立的桌面窗口应用。
支持 **Windows x86_64** 与 **macOS arm64 (Apple Silicon)**。

## 功能清单

### 🪟 桌面壳基础

- **原生窗口**：Windows 用 WebView2、macOS 用 WKWebView（系统内核），无捆绑浏览器，安装包小、内存占用低。
- **深色启动页**：原生 UI 质感；展示启动各阶段状态（更新检查 → 服务探测 → 启动后端 → 进入 GUI），失败时给出原因、日志尾部与「重试 / 启动后端 / 仍然打开」按钮。
- **窗口管理**：最小化/遮挡自动恢复聚焦；窗口内 `F5` / `Ctrl+R` 刷新。
- **调试埋点**：启动页关键动作写入应用旁 `launcher.log`，排障可查。

### 🔌 Harness 后端生命周期

- **TCP 可达性探测**：Rust 侧直接探测端口（避开 macOS WKWebView 对跨源 fetch 的拦截）。
- **自动启动后端**：探测不到服务时自动拉起 `dsh web` 并轮询等待就绪；超时展示后端日志尾部。
- **五级解析顺序**：`config.json` 覆盖 → 应用管理的后端（自动更新）→ 本机已安装 → 内置打包后端 → `npx` 兜底。
- **进程分离**：后端独立于客户端存活，stdout/stderr 写入应用旁 `backend.log`。
- **状态监测 + 一键重启**：进入 Harness 后定时检测后端状态；连续不通超过 90 秒（可配置）弹出原生对话框询问「是否重启后端」，确认后自动杀掉旧进程组、重启并等待就绪，成功后自动回到 Harness 页面。

### 📦 内置后端（离线兜底）

- 随应用打包 **Node 运行时 + 官方 `@deepseek-ai/dsh`**，断网环境开箱即用。
- **版本不再写死**：打包脚本构建时联网取 npm 最新版（`--version=x.y.z` / `DSH_VERSION` 可覆盖，网络失败自动回退）。
- Windows/macOS 均携带完整 Node 发行目录（含 npm），供运行时更新使用。

### 🔄 Harness 后端自动更新

- **每次启动检查**：联网查询 npm registry 上 `@deepseek-ai/dsh` 最新版本。
- **首次启动**：无任何后端时直接下载安装最新版到应用数据目录。
- **有新版自动同步**：启动页提醒「发现新版本 vX」并自动安装（可改为手动「立即更新 / 跳过更新」）。
- **工程加固**：独立 npm 缓存（规避全局缓存权限问题）、自动注入 node 到 PATH（修复原生包安装脚本）、`update.log` 记录进度。
- **离线容错**：检查失败静默跳过，不影响启动。

### 🚀 壳自身在线更新

- **每次启动检查**：查询 GitHub Releases 的 `latest.json`（Tauri 官方 updater 插件）。
- **一键更新**：启动页提醒「发现 DeepRein 新版本 vX」，点「立即更新」下载安装、实时进度；macOS 安装后自动重启，Windows 由 NSIS `passive` 静默接管。
- **签名安全**：更新包 minisign 签名，公钥内置、私钥不入库，下载后验签通过才安装。
- **CI 自动发布**：推送 `v*` 标签自动构建双平台更新包、生成 `latest.json` 并发布 GitHub Release，旧版客户端下次启动即可检测。

### ⚙️ 可配置

应用旁 `config.json`（首次运行自动生成），全部配置项见下方 [配置说明](#配置说明)。

### 🏗️ 构建与发布

- GitHub Actions 双平台矩阵构建（Windows NSIS、macOS .app/.dmg）。
- 推送 `v*` 标签自动发布 Release（安装包 + 在线更新元数据）；手动触发仅上传 Artifacts。

## 配置说明

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `harness_url` | `http://127.0.0.1:3080` | Harness Web GUI 地址 |
| `backend_command` | `null` | 后端启动命令（数组，显式覆盖自动探测） |
| `backend_cwd` | `null` | 后端进程工作目录 |
| `auto_start_backend` | `true` | 探测不到后端时自动启动 |
| `start_timeout_sec` | `90` | 等待后端就绪的超时（秒） |
| `start_check_interval_ms` | `1500` | 探测间隔（毫秒） |
| `backend_log_file` | `"backend.log"` | 后端日志文件名（相对应用目录） |
| `check_updates` | `true` | 每次启动检查 Harness 后端更新 |
| `auto_update` | `true` | 发现新版自动同步安装（`false` 则手动选择） |
| `update_timeout_sec` | `600` | 后端安装/更新超时（秒，首次安装需下载依赖） |
| `registry_url` | `https://registry.npmjs.org/@deepseek-ai/dsh` | dsh 包版本查询地址 |
| `check_app_updates` | `true` | 每次启动检查 DeepRein 壳自身更新 |
| `monitor_backend` | `true` | 进入 Harness 后持续监测后端状态 |
| `backend_down_threshold_sec` | `90` | 后端连续不通多久后弹窗提示重启（秒） |
| `health_check_interval_ms` | `5000` | 后端状态监测间隔（毫秒） |

示例（自定义启动命令）：

```json
{ "backend_command": ["C:\\Program Files\\nodejs\\node.exe", "C:\\path\\to\\bin.js", "web"] }
```

## 自动启动后端

应用检测不到 Harness 时，会自动执行后端启动命令并轮询等待服务就绪，然后进入 GUI。

**后端命令解析顺序（应用管理目录优先，自动跟随最新版）**：

1. `config.json` 里的 `backend_command`（显式覆盖）
2. **应用管理的后端**（自动更新生成）：`<应用数据目录>/backend/dsh`（macOS `~/Library/Application Support/com.deeprein.client/backend`、Windows `%APPDATA%\com.deeprein.client\backend`）→ `node <该目录>/lib/bin.js web`
3. **本机已安装的 Harness**：npx 缓存里的 dsh CLI（Windows: `%LOCALAPPDATA%\npm-cache\_npx`；macOS: `~/.npm/_npx`）或 npm 全局安装（`%APPDATA%\npm\node_modules` / `/usr/local/lib/node_modules`）→ `node <dsh>/lib/bin.js web`
4. **内置后端**（随应用打包的离线兜底）：`backend/node`（Node 运行时）+ `backend/dsh`（官方 `@deepseek-ai/dsh`）→ `node <内置>/lib/bin.js web --port <harness_url 端口>`
5. 兜底：Windows `cmd /C "dsh web || npx -y @deepseek-ai/dsh web"`，macOS/Linux `sh -c "dsh web || npx -y @deepseek-ai/dsh web"`

后端进程**分离运行**（独立于客户端存活），输出写入应用旁的 `backend.log`，启动页超时会显示日志尾部供排障。

## Harness 后端自动检查更新

每次启动进入 GUI 前，客户端都会先联网查询 npm registry 上 `@deepseek-ai/dsh` 的最新版本：

- **版本不再写死**：打包脚本（`scripts/bundle-backend.mjs`）构建时联网取最新版（可用 `--version=x.y.z` 或环境变量 `DSH_VERSION` 覆盖）；运行时脚本（`scripts/ensure-backend.mjs`）负责首次安装与后续更新。
- **首次启动**：若本机/内置后端都没有，直接联网下载安装最新版到应用数据目录。
- **后续启动**：已安装版本落后于最新版时，启动页提醒「发现新版本 vX」并自动同步安装（`auto_update: true` 默认行为）；设为 `false` 则提供「立即更新 / 跳过更新」按钮。
- **离线兜底**：检查失败（离线、缺少 Node）不阻塞启动，继续走内置/本机后端；`check_updates: false` 可完全关闭检查。
- **更新生效时机**：更新安装到应用数据目录，随后启动的后端进程即使用新版；若 Harness 后端已在运行（旧进程仍在内存中），新版将在下次后端启动时生效。
- **进度与排障**：安装进度实时显示在启动页；完整日志在应用数据目录的 `backend/update.log`。

## 壳自身在线更新

DeepRein 客户端通过 Tauri 官方 updater 插件支持在线更新，更新包经 GitHub Releases 分发：

- **检查**：每次启动先查询 `https://github.com/EvenLRs/deeprein/releases/latest/download/latest.json`；有新版本时启动页提醒「发现 DeepRein 新版本 vX」，点击「立即更新」自动下载安装（`check_app_updates: false` 可关闭）。
- **安装**：macOS 下载 `.app.tar.gz` 替换应用后自动重启；Windows 由 NSIS 安装器被动模式（`passive`）静默安装。
- **签名**：更新包用 minisign 密钥对签名，公钥写死在 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`，私钥不入库（本仓库 `.signing/` 已忽略）。CI 需要在仓库 Secrets 里配置两个 secret：`TAURI_SIGNING_PRIVATE_KEY`（`tauri signer generate --password <密码>` 生成的私钥内容）与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`（该密码）；本地打包同理设置这两个环境变量（或 `TAURI_SIGNING_PRIVATE_KEY_PATH`）。
- **发布流程**：推送 `v*` 标签 → CI 双平台构建并签名 → 自动创建 GitHub Release，`scripts/make-latest-json.mjs` 用 `.sig` 组装 `latest.json` 一并上传；已安装的旧版本客户端下次启动即可检测到更新。
- **版本升级**：发布新版本前把 `src-tauri/Cargo.toml` 与 `src-tauri/tauri.conf.json` 里的 `version` 同步递增（updater 按版本号比较）。

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
| Windows x86_64 | `windows-latest` | NSIS 安装器 `.exe`（构建收窄为 NSIS，不产 MSI） |
| macOS arm64 | `macos-14`（Apple Silicon） | `.app` + `.dmg` |

- **推送 `v*` 标签** 或 **手动触发**（Actions 页 → Build Desktop Apps → Run workflow）→ 双平台构建（含内置后端打包与更新包签名）。
- **推送 `v*` 标签**时额外执行发布任务：自动创建 GitHub Release，上传安装包 + 在线更新的 `latest.json`（客户端检测更新的数据源）。手动触发仅上传 Artifacts，不发布。
- 在线更新签名需要仓库 secrets `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`（见「壳自身在线更新」一节）；缺失时构建无法生成 `.sig`，release 任务会失败。
- macOS 产物为 **ad-hoc 签名**（无需证书即可构建、可运行）；因未用 Apple Developer ID 公证，从网上下载后 Gatekeeper 会拦截一次，首次打开任选其一：
  1. 右键 `DeepRein.app` → **打开** → 再点「打开」；
  2. 或在终端执行 `xattr -cr /Applications/DeepRein.app`（对 dmg：先 `xattr -cr ~/Downloads/DeepRein_*.dmg` 再挂载安装）。
- 一键安装/修复脚本 `scripts/install-macos.sh`：自动把应用复制到 `/Applications`、`xattr -cr` 解除隔离并刷新 Finder/Dock，专门修复「图标问号 / 双击无反应 / 已损坏无法打开」；它会跳过内置 Node 为 0 字节的不完整构建。
- 想要完全免拦截（双击即开、无任何提示）：需 Apple Developer ID 证书 + 公证，在 CI 配置 `APPLE_CERTIFICATE`/`APPLE_SIGNING_IDENTITY`/`APPLE_ID` 等 secrets 后接入 `tauri-action` 的签名公证流程。

## 目录结构

```
DeepRein/
├── dist/                       # 启动页（服务探测/更新检查/后端启动/排障 UI）
│   └── index.html
├── backend/                    # 内置后端（scripts/bundle-backend.mjs 生成，不入库）
│   ├── node/                   # Node 运行时（Windows: node.exe；macOS: bin/node）
│   └── dsh/node_modules/       # 官方 @deepseek-ai/dsh 及其运行时依赖
├── src-tauri/                  # Rust + Tauri 工程
│   ├── src/
│   │   ├── main.rs             # 程序入口（隐藏控制台窗口）
│   │   └── lib.rs              # 窗口创建 + 后端解析/启动 + Harness 更新检查 + 壳更新
│   ├── icons/                  # 应用图标（脚本生成）
│   ├── capabilities/default.json
│   ├── Cargo.toml
│   └── tauri.conf.json         # updater 配置（pubkey/endpoints/createUpdaterArtifacts）
├── scripts/
│   ├── bundle-backend.mjs      # 下载 Node 运行时 + 安装官方 dsh（打包前运行；构建时取 npm 最新版）
│   ├── ensure-backend.mjs      # 运行时后端安装/更新脚本（内嵌进客户端，首次启动安装最新版、日常检查更新）
│   ├── install-macos.sh        # macOS 安装/修复脚本（复制到 Applications + 解除隔离 + 刷新图标）
│   ├── make-latest-json.mjs    # 从签名产物(.sig)组装 updater 的 latest.json（CI release 任务用）
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

## ToDo List

### 🔜 待办（近期）

- [ ] **Windows 实机验证**：NSIS 安装/卸载、`passive` 静默更新、MinGW 构建在实机上完整跑通（当前仅 CI 构建验证，本地未实测）。
- [ ] **壳更新失败恢复**：下载/安装失败时提供回滚与重试入口（当前提示后仅可「跳过更新」继续使用旧版）。
- [ ] **Harness 更新失败重试**：后端更新失败时启动页提供「重试更新」按钮（当前静默继续启动）。
- [ ] **单实例保护**：检测已有实例运行，避免多开导致多个后端进程与重复更新。
- [ ] **发布前检查清单**：版本递增校验（Cargo.toml ↔ tauri.conf.json 一致）加入 CI，防止漏改版本导致更新不生效。

### 💭 想法（远期）

- [ ] **macOS 签名公证**：Apple Developer ID 签名 + 公证，彻底消除 Gatekeeper 拦截（需证书与 secrets）。
- [ ] **代理支持**：自动读取系统代理（企业网络环境下的更新检查）。
- [ ] **图形化配置**：内置 config.json 编辑界面，免手改文件。
- [ ] **Linux 支持**：AppImage/deb 打包 + 对应更新通道。
- [ ] **系统托盘**：关闭窗口后驻留托盘，后端保持运行。
- [ ] **多语言（i18n）**：当前启动页为硬编码中文。
- [ ] **自动化测试**：后端解析顺序、更新流程的单元/集成测试。
- [ ] **安装量/崩溃遥测**：可选、隐私友好的统计。

## 常见问题

- **提示“未检测到 DeepSeek Harness 服务”**：客户端会先尝试自动启动后端（`dsh web`）；若失败，确认 `config.json` 的 `backend_command` 与 `harness_url` 是否正确，或手动在终端运行 `dsh web`。
- **应用直接打开了 Harness 页面、启动页（连接检测）不生效**：说明构建时未启用 `tauri/custom-protocol` feature——没有它 release 会按 dev 模式运行，webview 直接加载 `devUrl`。`src-tauri/Cargo.toml` 已正确配置 `features = ["custom-protocol"]`，请勿移除。
- **编译报错找不到 `link.exe`**：未安装 MSVC 构建工具，安装 Visual Studio Build Tools 的 C++ 工作负载。
- **第一次编译很慢**：Tauri 依赖较多，首次需拉取并编译数百个 crate，属正常现象。
- **壳更新提示「更新安装失败」**：多为网络或权限问题（如应用所在磁盘与系统临时目录不在同一卷）；重试一次，仍失败可「跳过更新」继续使用当前版本，下个版本发布后会自动恢复。
