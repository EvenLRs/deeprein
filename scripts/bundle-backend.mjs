// 把官方 DeepSeek Harness 后端（@deepseek-ai/dsh + Node 运行时）打包到 backend/
// CI 与本地均可运行：node scripts/bundle-backend.mjs
// 产物结构：
//   backend/node/node.exe          (Windows) / backend/node/bin/node (macOS)
//   backend/dsh/node_modules/...   官方 dsh 包及其运行时依赖
import { spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// 按目标平台裁剪内置后端（与 scripts/ensure-backend.mjs 内的同名逻辑保持同步）：
//  1) node-pty 只保留当前平台的 prebuilds（他平台二进制各 28-30MB，运行时用不到）
//  2) Node 发行目录的 include/ 头文件（仅 node-gyp 编译原生模块需要；本项目原生依赖均带平台 prebuild）
//  3) npm 自带的文档目录
// 注意：node-pty 的 src/deps/third_party 仅在没有 prebuild 时才需要，一并移除；保留 binding.gyp 以兼容 npm rebuild。
function pruneBackendPlatform(dshDir, nodeDir, platform, arch) {
  let removed = 0;
  const measure = (p) => {
    try {
      let total = 0;
      const walk = (d) => {
        for (const e of readdirSync(d, { withFileTypes: true })) {
          const f = join(d, e.name);
          if (e.isDirectory()) walk(f);
          else total += statSync(f).size;
        }
      };
      walk(p);
      return total;
    } catch {
      return 0;
    }
  };
  const prune = (p) => {
    if (!existsSync(p)) return 0;
    const n = measure(p);
    rmSync(p, { recursive: true, force: true });
    removed += n;
    return n;
  };

  // 1) node-pty 其他平台 prebuilds
  const ptyPre = join(dshDir, 'node_modules', 'node-pty', 'prebuilds');
  if (existsSync(ptyPre)) {
    const want = `${platform}-${arch}`;
    for (const d of readdirSync(ptyPre)) {
      if (d !== want) prune(join(ptyPre, d));
    }
  }
  // node-pty 编译源码（prebuilds 存在时不会走 gyp 编译）
  prune(join(dshDir, 'node_modules', 'node-pty', 'src'));
  prune(join(dshDir, 'node_modules', 'node-pty', 'deps'));
  prune(join(dshDir, 'node_modules', 'node-pty', 'third_party'));

  // 2) 大体积 sourcemap（生产运行时不需要）
  if (existsSync(dshDir)) {
    const stack = [dshDir];
    while (stack.length) {
      const dir = stack.pop();
      let entries;
      try {
        entries = readdirSync(dir, { withFileTypes: true });
      } catch {
        continue;
      }
      for (const e of entries) {
        const f = join(dir, e.name);
        if (e.isDirectory()) stack.push(f);
        else if (e.name.endsWith('.map')) prune(f);
      }
    }
  }

  // 3) Node 发行目录:include 头文件 + npm 文档(macOS: lib/...;Windows: 根下 node_modules/...)
  if (nodeDir) {
    prune(join(nodeDir, 'include'));
    prune(join(nodeDir, 'lib', 'node_modules', 'npm', 'docs'));
    prune(join(nodeDir, 'node_modules', 'npm', 'docs'));
  }

  const mb = (removed / 1024 / 1024).toFixed(1);
  console.log(`[裁剪] 目标平台 ${platform}-${arch}，移除 ${mb} MB 冗余文件`);
}

// 官方 @deepseek-ai/dsh 版本解析（不再写死）：
//   1) 命令行 --version=x.y.z 显式指定（可复现构建）
//   2) 环境变量 DSH_VERSION
//   3) 联网获取 npm registry 的最新版本（默认行为，构建时跟随最新版）
//   4) 网络失败时回退到 FALLBACK_DSH_VERSION
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
  const name = `node-${NODE_VERSION}-win-x64`;
  if (!existsSync(join(nodeDir, 'node.exe'))) {
    const zip = join(backend, `${name}.zip`);
    await download(`https://nodejs.org/dist/${NODE_VERSION}/${name}.zip`, zip);
    // Windows 自带 bsdtar，支持 zip
    sh('tar', ['-xf', zip]);
    // 整体移动发行目录：node_modules/npm 与 corepack 一并保留，
    // 供运行时更新脚本（ensure-backend.mjs）在无系统 npm 的机器上安装/更新后端
    // Windows 的 rename 不能覆盖已存在的目录（EPERM），先移除空的目标目录
    rmSync(nodeDir, { recursive: true, force: true });
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
    // 目标目录若存在（残留/占位）先移除，避免 rename 失败（Windows EPERM / POSIX ENOTEMPTY）
    rmSync(nodeDir, { recursive: true, force: true });
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
    '--ignore-scripts',
    '--omit=dev',
    '--no-audit',
    '--no-fund',
    '--no-package-lock',
  ]);

  const allowedScripts = [
    '@deepseek-ai/dsh-subprocess-local',
    '@google/genai',
    'koffi',
    'node-pty',
    'protobufjs',
  ];

  for (const pkgName of allowedScripts) {
    try {
      runNpm(['rebuild', pkgName], { cwd: dshDir });
    } catch (err) {
      console.warn(`[提示] 原生模块 ${pkgName} 定向构建未触发: ${err.message || err}`);
    }
  }

  const pkgJsonPath = join(dshDir, 'package.json');
  if (!existsSync(pkgJsonPath)) {
    throw new Error(`无法找到后端根目录 package.json (${pkgJsonPath})，无法写入 allowScripts 策略`);
  }
  const pkgJson = JSON.parse(readFileSync(pkgJsonPath, 'utf8'));
  const newAllowScripts = {};
  for (const name of allowedScripts) {
    const pkgFile = join(dshDir, 'node_modules', ...name.split('/'), 'package.json');
    if (!existsSync(pkgFile)) {
      throw new Error(`无法找到已安装包 ${name} 的 package.json (${pkgFile})`);
    }
    const pkgData = JSON.parse(readFileSync(pkgFile, 'utf8'));
    const ver = pkgData.version;
    if (!ver || typeof ver !== 'string') {
      throw new Error(`已安装包 ${name} 缺少有效的 version 字段`);
    }
    newAllowScripts[`${name}@${ver}`] = true;
  }
  pkgJson.allowScripts = newAllowScripts;
  writeFileSync(pkgJsonPath, JSON.stringify(pkgJson, null, 2) + '\n');
}

// 按目标平台裁剪冗余内容（他平台 prebuilds、头文件、sourcemap、文档）
pruneBackendPlatform(dshDir, nodeDir, platform, arch);

// ---- 3. 第三方 Harness 插件（随壳打包；运行时由 ensure-plugin.mjs 装入 web profile） ----
const pluginDir = join(backend, 'plugin');
mkdirSync(pluginDir, { recursive: true });
const pluginManifest = [];
// 清理上一轮打包的 tarball，避免版本变化后旧产物残留进 manifest/壳资源
for (const file of readdirSync(pluginDir)) {
  if (file.endsWith('.tgz')) rmSync(join(pluginDir, file), { force: true });
}

// 当前 dsh web profile 实际引用的插件清单（与 README 中的记录保持一致）：
//   - 本地 fork 优先（本机当前安装的就是它）；CI 无该目录时回退到 npm 发布版
const bundledPlugins = [
  {
    name: '@zebbkira/dsh-skills-mcp-manager',
    local: join(root, 'plugins', 'dsh-skills-mcp-manager'),
    fallback: '@zebbkira/dsh-skills-mcp-manager@0.1.3',
  },
  { name: 'dsh-better-sidebar', spec: 'dsh-better-sidebar@0.12.1' },
  { name: 'dsh-browser', spec: 'dsh-browser@0.1.0' },
  { name: 'dsh-mnemon', spec: 'dsh-mnemon@0.1.4' },
  { name: 'dshmarket', spec: 'dshmarket@1.5.0' },
  // 消息渠道网关插件，构建时从 npm registry 拉取
  { name: 'dsh-messaging', spec: 'dsh-messaging@0.1.2' },
  // 本仓库自带的 host 桥插件：向外壳暴露后端内部健康/诊断状态。
  // 与外壳强耦合、版本必须同步，故随仓库分发而非发 npm；无 fallback 是有意的——
  // 目录已入库必然存在，若被误删则构建响亮失败，优于静默少装一个一方插件。
  { name: '@deeprein/host-bridge', local: join(root, 'plugins', 'deeprein-host-bridge') },
];

for (const p of bundledPlugins) {
  let spec = p.spec ?? null;
  let version = spec ? spec.split('@').pop() : null;
  if (p.local) {
    const pkgPath = join(p.local, 'package.json');
    if (existsSync(pkgPath)) {
      spec = p.local;
      try {
        version = JSON.parse(readFileSync(pkgPath, 'utf8')).version;
      } catch {
        version = null;
      }
    } else {
      console.warn(`[警告] 本地插件目录缺失 ${p.local}，回退到 registry ${p.fallback}`);
      spec = p.fallback;
      version = p.fallback.split('@').pop();
    }
  }
  if (!spec) throw new Error(`插件 ${p.name} 既无本地目录也无 registry 回退，无法打包`);
  console.log(`[插件] 打包 ${spec}`);
  runNpm(['pack', spec, '--pack-destination', pluginDir]);
  // 预期产物名：<scope 去 @ 并把 / 换成 ->-<version>.tgz（与 npm pack 一致）
  const prefix = p.name.replace(/^@/, '').replace('/', '-');
  const tarball =
    readdirSync(pluginDir).find((f) => f === `${prefix}-${version}.tgz`) ??
    readdirSync(pluginDir).find((f) => f.endsWith('.tgz') && f.startsWith(`${prefix}-`));
  if (!tarball) throw new Error(`未找到插件 ${p.name} 的打包产物（${spec}）`);
  console.log(`[插件] ${p.name}@${version ?? '?'} → ${tarball}`);
  pluginManifest.push({ name: p.name, version, tarball });
}

// ---- 4. pnpm 运行时（安装插件用；随壳分发，不依赖目标机 PATH） ----
const pnpmCli = join(backend, 'tools', 'node_modules', 'pnpm', 'bin', 'pnpm.cjs');
if (!existsSync(pnpmCli)) {
  console.log('[工具] 安装 pnpm（运行时插件安装器）');
  runNpm([
    'install',
    '--prefix',
    join(backend, 'tools'),
    'pnpm@11',
    '--omit=dev',
    '--no-audit',
    '--no-fund',
    '--no-package-lock',
  ]);
}

// ---- 5. 元信息 ----
writeFileSync(
  join(pluginDir, 'manifest.json'),
  JSON.stringify(
    {
      generated_at: new Date().toISOString(),
      pnpm: 'tools/node_modules/pnpm/bin/pnpm.cjs',
      plugins: pluginManifest,
    },
    null,
    2,
  ),
);
writeFileSync(
  join(backend, 'bundle-info.json'),
  JSON.stringify(
    { dsh: dshVersion, node: NODE_VERSION, platform, arch, plugins: pluginManifest },
    null,
    2,
  ),
);

console.log('[OK] 后端已打包');
console.log('  node:', nodeExe);
console.log('  dsh :', dshBin);
console.log('  插件:', join(pluginDir, 'manifest.json'));
