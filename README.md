# deeprein — DeepSeek Harness 桌面客户端

一个基于 **Rust + Tauri v2** 的原生桌面应用，把本机运行的
[DeepSeek Harness](http://127.0.0.1:3080) Web GUI 包装成独立的桌面窗口应用。
支持 **Windows x86_64** 与 **macOS arm64 (Apple Silicon)**。

## 特性

- 🪟 **原生窗口**：Windows 用 WebView2、macOS 用 WKWebView（系统内核），无捆绑浏览器，安装包小、内存占用低。
- 🔌 **连接检测 + 自动启动后端**：启动时自动探测 Harness（默认 `127.0.0.1:3080`）；未运行时自动拉起 `dsh web` 后端并等待就绪，失败时给出友好提示与重试。
- 🔄 **自动检查更新 + 同步最新版**：每次启动先联网查询 npm registry 上 `@deepseek-ai/dsh` 的最新版本；发现新版本会在启动页提醒并自动下载安装到应用数据目录（首次启动若无后端则直接联网获取最新版）。离线或检查失败时静默跳过，不影响启动。
- 🚀 **壳自身在线更新**：deeprein 桌面应用本身也支持在线更新（Tauri updater 插件 + GitHub Releases 分发）；启动页检查到新版本时提醒用户，点击「立即更新」自动下载安装，macOS 安装后自动重启、Windows 由 NSIS 安装器静默接管。
- 📦 **内置官方 Harness 后端**：应用自带 Node 运行时 + 官方 `@deepseek-ai/dsh` 包作为离线兜底（打包时跟随 npm 最新版，不再写死版本）——本机已安装则优先用本机，未安装则自动启动内置后端，开箱即用。
- 🎨 **深色启动页**：原生 UI 质感，检测成功后自动导航进入 Harness。
- ⚙️ **可配置**：应用旁的 `config.json` 可改地址、启动命令、超时、更新开关等。

## 自动启动后端

应用检测不到 Harness 时，会自动执行后端启动命令并轮询等待服务就绪，然后进入 GUI。

**后端命令解析顺序（应用管理目录优先，自动跟随最新版）**：

1. `config.json` 里的 `backend_command`（显式覆盖）
2. **应用管理的后端**（自动更新生成）：`<应用数据目录>/backend/dsh`（macOS `~/Library/Application Support/com.deeprein.client/backend`、Windows `%APPDATA%\com.deeprein.client\backend`）→ `node <该目录>/lib/bin.js web`
3. **本机已安装的 Harness**：npx 缓存里的 dsh CLI（Windows: `%LOCALAPPDATA%\npm-cache\_npx`；macOS: `~/.npm/_npx`）或 npm 全局安装（`%APPDATA%\npm\node_modules` / `/usr/local/lib/node_modules`）→ `node <dsh>/lib/bin.js web`
4. **内置后端**（随应用打包的离线兜底）：`backend/node`（Node 运行时）+ `backend/dsh`（官方 `@deepseek-ai/dsh`）→ `node <内置>/lib/bin.js web --port <harness_url 端口>`
5. 兜底：Windows `cmd /C "dsh web || npx -y @deepseek-ai/dsh web"`，macOS/Linux `sh -c "dsh web || npx -y @deepseek-ai/dsh web"`

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
  "backend_log_file": "backend.log",
  "check_updates": true,
  "auto_update": true,
  "update_timeout_sec": 600,
  "registry_url": "https://registry.npmjs.org/@deepseek-ai/dsh",
  "check_app_updates": true
}
```

示例（自定义启动命令）：

```json
{ "backend_command": ["C:\\Program Files\\nodejs\\node.exe", "C:\\path\\to\\bin.js", "web"] }
```

## 壳自身在线更新

deeprein 客户端通过 Tauri 官方 updater 插件支持在线更新，更新包经 GitHub Releases 分发：

- **检查**：每次启动先查询 `https://github.com/EvenLRs/deeprein/releases/latest/download/latest.json`；有新版本时启动页提醒「发现 deeprein 新版本 vX」，点击「立即更新」自动下载安装（`check_app_updates: false` 可关闭）。
- **安装**：macOS 下载 `.app.tar.gz` 替换应用后自动重启；Windows 由 NSIS 安装器被动模式（`passive`）静默安装。
- **签名**：更新包用 minisign 密钥对签名，公钥写死在 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`，私钥不入库（本仓库 `.signing/` 已忽略）。CI 需要在仓库 Secrets 里配置两个 secret：`TAURI_SIGNING_PRIVATE_KEY`（`tauri signer generate --password <密码>` 生成的私钥内容）与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`（该密码）；本地打包同理设置这两个环境变量（或 `TAURI_SIGNING_PRIVATE_KEY_PATH`）。
- **发布流程**：推送 `v*` 标签 → CI 双平台构建并签名 → 自动创建 GitHub Release，`scripts/make-latest-json.mjs` 用 `.sig` 组装 `latest.json` 一并上传；已安装的旧版本客户端下次启动即可检测到更新。
- **版本升级**：发布新版本前把 `src-tauri/Cargo.toml` 与 `src-tauri/tauri.conf.json` 里的 `version` 同步递增（updater 按版本号比较）。

## 自动检查更新

每次启动进入 GUI 前，客户端都会先联网查询 npm registry 上 `@deepseek-ai/dsh` 的最新版本：

- **版本不再写死**：打包脚本（`scripts/bundle-backend.mjs`）构建时联网取最新版（可用 `--version=x.y.z` 或环境变量 `DSH_VERSION` 覆盖）；运行时脚本（`scripts/ensure-backend.mjs`）负责首次安装与后续更新。
- **首次启动**：若本机/内置后端都没有，直接联网下载安装最新版到应用数据目录。
- **后续启动**：已安装版本落后于最新版时，启动页提醒「发现新版本 vX」并自动同步安装（`auto_update: true` 默认行为）；设为 `false` 则提供「立即更新 / 跳过更新」按钮。
- **离线兜底**：检查失败（离线、缺少 Node）不阻塞启动，继续走内置/本机后端；`check_updates: false` 可完全关闭检查。
- **更新生效时机**：更新安装到应用数据目录，随后启动的后端进程即使用新版；若 Harness 后端已在运行（旧进程仍在内存中），新版将在下次后端启动时生效。
- **进度与排障**：安装进度实时显示在启动页；完整日志在应用数据目录的 `backend/update.log`。

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

- **推送 `v*` 标签** 或 **手动触发**（Actions 页 → Build Desktop Apps → Run workflow）→ 双平台构建（含内置后端打包与更新包签名）。
- **推送 `v*` 标签**时额外执行发布任务：自动创建 GitHub Release，上传安装包 + 在线更新的 `latest.json`（客户端检测更新的数据源）。手动触发仅上传 Artifacts，不发布。
- 在线更新签名需要仓库 secret `TAURI_SIGNING_PRIVATE_KEY`（见「壳自身在线更新」一节）；缺失时构建无法生成 `.sig`，release 任务会失败。
- macOS 产物为 **ad-hoc 签名**（无需证书即可构建、可运行）；因未用 Apple Developer ID 公证，从网上下载后 Gatekeeper 会拦截一次，首次打开任选其一：
  1. 右键 `deeprein.app` → **打开** → 再点「打开」；
  2. 或在终端执行 `xattr -cr /Applications/deeprein.app`（对 dmg：先 `xattr -cr ~/Downloads/deeprein_*.dmg` 再挂载安装）。
- 想要完全免拦截（双击即开、无任何提示）：需 Apple Developer ID 证书 + 公证，在 CI 配置 `APPLE_CERTIFICATE`/`APPLE_SIGNING_IDENTITY`/`APPLE_ID` 等 secrets 后接入 `tauri-action` 的签名公证流程。

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
│   ├── bundle-backend.mjs      # 下载 Node 运行时 + 安装官方 dsh（打包前运行；构建时取 npm 最新版）
│   ├── ensure-backend.mjs      # 运行时后端安装/更新脚本（内嵌进客户端，首次启动安装最新版、日常检查更新）
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

## 常见问题

- **提示“未检测到 DeepSeek Harness 服务”**：客户端会先尝试自动启动后端（`dsh web`）；若失败，确认 `config.json` 的 `backend_command` 与 `harness_url` 是否正确，或手动在终端运行 `dsh web`。
- **应用直接打开了 Harness 页面、启动页（连接检测）不生效**：说明构建时未启用 `tauri/custom-protocol` feature——没有它 release 会按 dev 模式运行，webview 直接加载 `devUrl`。`src-tauri/Cargo.toml` 已正确配置 `features = ["custom-protocol"]`，请勿移除。
- **编译报错找不到 `link.exe`**：未安装 MSVC 构建工具，安装 Visual Studio Build Tools 的 C++ 工作负载。
- **第一次编译很慢**：Tauri 依赖较多，首次需拉取并编译数百个 crate，属正常现象。
