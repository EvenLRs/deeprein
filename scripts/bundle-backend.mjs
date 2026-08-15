// 把官方 DeepSeek Harness 后端（@deepseek-ai/dsh + Node 运行时）打包到 backend/
// CI 与本地均可运行：node scripts/bundle-backend.mjs
// 产物结构：
//   backend/node/node.exe          (Windows) / backend/node/bin/node (macOS)
//   backend/dsh/node_modules/...   官方 dsh 包及其运行时依赖
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, renameSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// 官方 @deepseek-ai/dsh 版本解析（不再写死）：
//   1) 命令行 --version=x.y.z 显式指定（可复现构建）
//   2) 环境变量 DSH_VERSION
//   3) 联网获取 npm registry 的最新版本（默认行为，构建时跟随最新版）
//   4) 网络失败时回退到 FALLBACK_DSH_VERSION
const FALLBACK_DSH_VERSION = '0.1.0-rc.6';
const NODE_VERSION = 'v24.18.0';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const backend = join(root, 'backend');

function resolveDshVersion(argv) {
  const cliArg = argv.find((a) => a.startsWith('--version='));
  if (cliArg) {
    const v = cliArg.split('=')[1].trim();
    if (v) {
      console.log(`[版本] 使用命令行指定版本 ${v}`);
      return v;
    }
  }
  if (process.env.DSH_VERSION) {
    console.log(`[版本] 使用环境变量 DSH_VERSION=${process.env.DSH_VERSION}`);
    return process.env.DSH_VERSION.trim();
  }
  return null;
}

async function latestDshVersion() {
  try {
    const res = await fetch('https://registry.npmjs.org/@deepseek-ai/dsh/latest', {
      redirect: 'follow',
    });
    if (!res.ok) throw new Error(`registry HTTP ${res.status}`);
    const json = await res.json();
    if (!json || typeof json.version !== 'string') throw new Error('registry 响应缺少 version');
    return json.version;
  } catch (e) {
    console.warn(`[警告] 获取 npm 最新版本失败（${e.message}），回退到 ${FALLBACK_DSH_VERSION}`);
    return FALLBACK_DSH_VERSION;
  }
}

const platform = process.platform; // win32 | darwin
const arch = process.arch; // x64 | arm64

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', cwd: backend, ...opts });
  if (r.status !== 0) {
    console.error(`[FAIL] ${cmd} ${args.join(' ')} exit=${r.status}`);
    process.exit(r.status ?? 1);
  }
}

// 用 node + npm-cli.js 调 npm：Windows 上裸 'npm' 可能被 .ps1 遮蔽导致 spawn 失败
function npmCli() {
  const p = join(dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js');
  return existsSync(p) ? p : null;
}

function runNpm(args, opts = {}) {
  const cli = npmCli();
  if (cli) {
    sh(process.execPath, [cli, ...args], opts);
  } else {
    sh('npm', args, opts);
  }
}

// npm 11+ 才有 allowScripts 机制（默认跳过未批准脚本，需 approve + rebuild）；npm 10 直接执行
function npmMajor() {
  const cli = npmCli();
  const r = cli
    ? spawnSync(process.execPath, [cli, '--version'], { encoding: 'utf8' })
    : spawnSync('npm', ['--version'], { encoding: 'utf8' });
  const m = parseInt((r.stdout || '').trim().split('.')[0], 10);
  return Number.isFinite(m) ? m : 0;
}

async function download(url, out) {
  if (existsSync(out)) {
    console.log('[skip] 已存在', out);
    return;
  }
  console.log('[下载]', url);
  const res = await fetch(url, { redirect: 'follow' });
  if (!res.ok) throw new Error(`download failed HTTP ${res.status}: ${url}`);
  const buf = Buffer.from(await res.arrayBuffer());
  writeFileSync(out, buf);
  console.log('[ok]', out, `${Math.round(buf.length / 1024 / 1024)} MB`);
}

mkdirSync(backend, { recursive: true });

// ---- 1. Node 运行时 ----
const nodeDir = join(backend, 'node');
let nodeExe;
if (platform === 'win32') {
  mkdirSync(nodeDir, { recursive: true });
  const name = `node-${NODE_VERSION}-win-x64`;
  if (!existsSync(join(nodeDir, 'node.exe'))) {
    const zip = join(backend, `${name}.zip`);
    await download(`https://nodejs.org/dist/${NODE_VERSION}/${name}.zip`, zip);
    // Windows 自带 bsdtar，支持 zip
    sh('tar', ['-xf', zip]);
    // 整体移动发行目录：node_modules/npm 与 corepack 一并保留，
    // 供运行时更新脚本（ensure-backend.mjs）在无系统 npm 的机器上安装/更新后端
    renameSync(join(backend, name), nodeDir);
    rmSync(zip, { force: true });
  }
  nodeExe = join(nodeDir, 'node.exe');
} else if (platform === 'darwin' && arch === 'arm64') {
  const name = `node-${NODE_VERSION}-darwin-arm64`;
  if (!existsSync(join(nodeDir, 'bin', 'node'))) {
    const tgz = join(backend, `${name}.tar.gz`);
    await download(`https://nodejs.org/dist/${NODE_VERSION}/${name}.tar.gz`, tgz);
    sh('tar', ['-xzf', tgz]);
    // 整体移动发行目录：bin/ 内含指向 ../lib 的符号链接（corepack/npm/npx），必须保留
    renameSync(join(backend, name), nodeDir);
    rmSync(tgz, { force: true });
  }
  nodeExe = join(nodeDir, 'bin', 'node');
} else {
  throw new Error(`不支持的目标平台: ${platform}/${arch}`);
}

// ---- 2. 官方 dsh 包 ----
const dshVersion = resolveDshVersion(process.argv.slice(2)) ?? (await latestDshVersion());
console.log(`[版本] 使用 @deepseek-ai/dsh@${dshVersion}`);
const dshDir = join(backend, 'dsh');
mkdirSync(dshDir, { recursive: true });
const dshBin = join(dshDir, 'node_modules', '@deepseek-ai', 'dsh', 'lib', 'bin.js');
if (!existsSync(dshBin)) {
  console.log(`[npm] 安装 @deepseek-ai/dsh@${dshVersion} ...`);
  runNpm([
    'install',
    '--prefix',
    dshDir,
    `@deepseek-ai/dsh@${dshVersion}`,
    '--omit=dev',
    '--no-audit',
    '--no-fund',
    '--no-package-lock',
  ]);
  // npm 11 的 allowScripts 机制默认跳过未批准的安装脚本（node-pty/koffi 等原生包依赖它们）
  if (npmMajor() >= 11) {
    runNpm(['approve-scripts', '--all'], { cwd: dshDir });
    runNpm(['rebuild'], { cwd: dshDir });
  }
}

// ---- 3. 元信息 ----
writeFileSync(
  join(backend, 'bundle-info.json'),
  JSON.stringify({ dsh: dshVersion, node: NODE_VERSION, platform, arch }, null, 2),
);

console.log('[OK] 后端已打包');
console.log('  node:', nodeExe);
console.log('  dsh :', dshBin);
