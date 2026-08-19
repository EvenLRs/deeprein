// DeepSeek Harness 运行时后端安装/更新脚本（纯 Node 内置模块，无第三方依赖）。
// 由桌面客户端在启动时调用：
//   node ensure-backend.mjs --target <目录> [--registry <基础URL>] [--node <node可执行文件>] [--check-only]
//
// 行为：
//   --check-only  ：仅查询 npm registry 的最新版本，与已安装版本比较，不安装；
//   不带该参数   ：已安装则对比最新版本、有更新自动安装；未安装则安装最新版本。
//
// 输出约定（stdout）：
//   普通行       → 进度文本（客户端写入 update.log 供启动页展示）
//   PROGRESS <json> → 结构化进度（保留扩展用）
//   RESULT <json>  → 最后一行结果：
//     { ok, checked, current, latest, update_available, updated, error? }
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { delimiter, dirname, join, resolve } from 'node:path';

// ---------- 参数解析 ----------
function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--check-only') {
      args.checkOnly = true;
      continue;
    }
    const m = a.match(/^--([^=]+)=(.*)$/);
    if (m) {
      args[m[1]] = m[2];
      continue;
    }
    if (a.startsWith('--') && i + 1 < argv.length && !argv[i + 1].startsWith('--')) {
      args[a.slice(2)] = argv[i + 1];
      i++;
      continue;
    }
    if (!args.target) args.target = a; // 位置参数视为 target
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const target = args.target ? resolve(args.target) : null;
const registryBase = (args.registry || 'https://registry.npmjs.org/@deepseek-ai/dsh').replace(/\/+$/, '');
const checkOnly = !!args.checkOnly;
const nodeExe = args.node ? resolve(args.node) : process.execPath;

function log(msg) {
  console.log(msg);
}
function progress(state, extra) {
  console.log('PROGRESS ' + JSON.stringify({ state, ...extra }));
}

// ---------- 版本比较（支持 x.y.z 与 -rc.n 等预发布） ----------
function parseVer(v) {
  const m = String(v || '').trim().match(/^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?/);
  if (!m) return null;
  return { major: +m[1], minor: +m[2], patch: +m[3], pre: m[4] || '' };
}
// 返回 1 / -1 / 0；解析失败返回 null
function cmpVer(a, b) {
  const A = parseVer(a);
  const B = parseVer(b);
  if (!A || !B) return null;
  if (A.major !== B.major) return A.major > B.major ? 1 : -1;
  if (A.minor !== B.minor) return A.minor > B.minor ? 1 : -1;
  if (A.patch !== B.patch) return A.patch > B.patch ? 1 : -1;
  // 预发布规则：正式版 > rc；同为预发布按字典序比较
  if (A.pre === B.pre) return 0;
  if (!A.pre) return 1;
  if (!B.pre) return -1;
  return A.pre > B.pre ? 1 : -1;
}

// ---------- 已安装版本 ----------
function installedVersion(dshDir) {
  const pkg = join(dshDir, 'node_modules', '@deepseek-ai', 'dsh', 'package.json');
  try {
    const v = JSON.parse(readFileSync(pkg, 'utf8')).version;
    return typeof v === 'string' ? v : null;
  } catch {
    return null;
  }
}

// ---------- npm registry 最新版本 ----------
async function latestVersion() {
  const res = await fetch(`${registryBase}/latest`, { redirect: 'follow' });
  if (!res.ok) throw new Error(`registry HTTP ${res.status} (${registryBase})`);
  const json = await res.json();
  if (!json || typeof json.version !== 'string') throw new Error('registry 响应缺少 version 字段');
  return json.version;
}

// ---------- 定位 npm CLI ----------
// 优先 node 可执行文件所在发行目录自带的 npm（内置后端保证存在），
// 其次常见安装位置，最后 PATH 上的 npm/npm.cmd。
function findNpm(node) {
  const candidates = [];
  const exeDir = dirname(node);
  // Windows 发行包/标准安装：<node>\node_modules\npm\bin\npm-cli.js
  // macOS/Linux 发行包：<node>/../lib/node_modules/npm/bin/npm-cli.js
  candidates.push(
    join(exeDir, 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    join(exeDir, '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    join(exeDir, 'npm-cli.js'),
  );
  if (process.platform !== 'win32') {
    candidates.push(
      '/usr/local/lib/node_modules/npm/bin/npm-cli.js',
      '/opt/homebrew/lib/node_modules/npm/bin/npm-cli.js',
      '/usr/lib/node_modules/npm/bin/npm-cli.js',
    );
  } else if (process.env.ProgramFiles) {
    candidates.push(
      join(process.env.ProgramFiles, 'nodejs', 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    );
  }
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  // 兜底：PATH 上的 npm（GUI 应用 PATH 可能很精简，可能找不到）
  return process.platform === 'win32' ? 'npm.cmd' : 'npm';
}

function isScriptPath(p) {
  return p.includes('/') || p.includes('\\') || p.endsWith('.js');
}

// 按目标平台裁剪 dsh 安装目录（与 scripts/bundle-backend.mjs 的 pruneBackendPlatform 保持同步，
// 但此处不裁剪内置 Node 发行目录——那是随应用打包的资源，修改会破坏 .app 签名）：
//  1) node-pty 只保留当前平台 prebuilds（他平台二进制各 28-30MB）
//  2) node-pty 编译源码（prebuilds 存在时不会走 gyp 编译）
//  3) 大体积 sourcemap
function pruneDshInstall(dshDir) {
  const platform = process.platform; // win32 | darwin
  const arch = process.arch; // x64 | arm64
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
    if (!existsSync(p)) return;
    removed += measure(p);
    rmSync(p, { recursive: true, force: true });
  };

  const pty = join(dshDir, 'node_modules', 'node-pty');
  if (existsSync(pty)) {
    const pre = join(pty, 'prebuilds');
    if (existsSync(pre)) {
      const want = `${platform}-${arch}`;
      for (const d of readdirSync(pre)) {
        if (d !== want) prune(join(pre, d));
      }
    }
    prune(join(pty, 'src'));
    prune(join(pty, 'deps'));
    prune(join(pty, 'third_party'));
  }

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

  const mb = (removed / 1024 / 1024).toFixed(1);
  if (removed > 0) log(`[裁剪] 移除 ${mb} MB 冗余文件（他平台 prebuilds/sourcemap）`);
}

function runNpm(node, npmCli, argsArr, cwd) {
  const r = isScriptPath(npmCli)
    ? spawnSync(node, [npmCli, ...argsArr], { stdio: 'inherit', cwd })
    : spawnSync(npmCli, argsArr, { stdio: 'inherit', cwd, shell: process.platform === 'win32' });
  if (r.status !== 0) throw new Error(`npm ${argsArr.join(' ')} 退出码 ${r.status ?? '未知'}`);
}

// npm 11+ 有 allowScripts 机制（默认跳过未批准安装脚本，原生包需要 approve + rebuild）
function npmMajor(node, npmCli) {
  const r = isScriptPath(npmCli)
    ? spawnSync(node, [npmCli, '--version'], { encoding: 'utf8' })
    : spawnSync(npmCli, ['--version'], { encoding: 'utf8', shell: process.platform === 'win32' });
  const m = parseInt((r.stdout || '').trim().split('.')[0], 10);
  return Number.isFinite(m) ? m : 0;
}

// ---------- 主流程 ----------
async function main() {
  if (!target) throw new Error('缺少 --target 参数');
  const dshDir = join(target, 'dsh');
  const current = installedVersion(dshDir);
  progress(checkOnly ? 'checking' : 'fetching', { current: current ?? null });

  let latest;
  try {
    latest = await latestVersion();
  } catch (e) {
    if (checkOnly) {
      // 离线/网络错误：仅检查时视为“未完成”，由客户端决定继续启动
      console.log(
        'RESULT ' +
          JSON.stringify({
            ok: false,
            checked: true,
            current: current ?? null,
            latest: null,
            update_available: false,
            updated: false,
            error: String((e && e.message) || e),
          }),
      );
      return;
    }
    throw e;
  }

  const updateAvailable = current === null || (cmpVer(latest, current) ?? -1) > 0;
  if (checkOnly) {
    console.log(
      'RESULT ' +
        JSON.stringify({
          ok: true,
          checked: true,
          current: current ?? null,
          latest,
          update_available: updateAvailable,
          updated: false,
        }),
    );
    return;
  }
  if (!updateAvailable) {
    log(`DeepSeek Harness 已是最新版本 v${current}（registry: v${latest}）`);
    console.log(
      'RESULT ' +
        JSON.stringify({
          ok: true,
          checked: true,
          current,
          latest,
          update_available: false,
          updated: false,
        }),
    );
    return;
  }

  if (current === null) {
    log(`[首次安装] 联网获取最新 DeepSeek Harness 后端 v${latest} → ${dshDir}`);
  } else {
    log(`[更新] DeepSeek Harness ${current} → ${latest}`);
  }
  progress('installing', { current: current ?? null, latest });

  mkdirSync(dshDir, { recursive: true });
  const npmCli = findNpm(nodeExe);
  // 使用应用数据目录内的独立 npm 缓存：
  // 避免用户全局缓存（~/.npm）里的 root 属主文件、并发写冲突等历史问题导致安装失败
  const npmCache = join(target, '.npm-cache');
  mkdirSync(npmCache, { recursive: true });
  log(`使用 npm: ${npmCli}（node: ${nodeExe}）`);

  // 把 node 所在目录放进 PATH：原生包（koffi/node-pty 等）的安装脚本会直接调用 `node`，
  // 而 GUI 客户端的环境 PATH 通常不包含内置 node 目录，缺了会 127 失败
  const nodeBinDir = dirname(nodeExe);
  if (nodeBinDir && !(process.env.PATH || '').split(delimiter).includes(nodeBinDir)) {
    process.env.PATH = nodeBinDir + delimiter + (process.env.PATH || '');
  }

  // 在安装前先尝试读取已有的 allowScripts 白名单（防止 install 覆盖/重写 package.json）
  const fallbackAllowScripts = [
    '@deepseek-ai/dsh-subprocess-local',
    '@google/genai',
    'koffi',
    'node-pty',
    'protobufjs',
  ];

  function stripPkgVersion(spec) {
    const s = String(spec || '').trim();
    const lastAt = s.lastIndexOf('@');
    return lastAt > 0 ? s.slice(0, lastAt) : s;
  }

  let preAllowScripts = [];
  try {
    const existingPkg = JSON.parse(readFileSync(join(dshDir, 'package.json'), 'utf8'));
    if (existingPkg.allowScripts && typeof existingPkg.allowScripts === 'object') {
      preAllowScripts = Object.keys(existingPkg.allowScripts);
    }
  } catch {
    /* 首次安装时 package.json 可能尚未生成 */
  }

  const npmInstallArgs = [
    'install',
    `@deepseek-ai/dsh@${latest}`,
    '--cache',
    npmCache,
    '--prefix',
    dshDir,
    '--omit=dev',
    '--no-audit',
    '--no-fund',
    '--no-package-lock',
  ];
  runNpm(nodeExe, npmCli, npmInstallArgs);
  if (npmMajor(nodeExe, npmCli) >= 11) {
    let postAllowScripts = [];
    try {
      const postPkg = JSON.parse(readFileSync(join(dshDir, 'package.json'), 'utf8'));
      if (postPkg.allowScripts && typeof postPkg.allowScripts === 'object') {
        postAllowScripts = Object.keys(postPkg.allowScripts);
      }
    } catch {
      /* ignore */
    }
    const rawList = preAllowScripts.concat(postAllowScripts);
    const normalized = rawList.map(stripPkgVersion).filter(Boolean);
    const combined = Array.from(new Set(normalized));
    const scriptsToApprove = combined.length > 0 ? combined : fallbackAllowScripts;
    if (scriptsToApprove.length > 0) {
      try {
        runNpm(
          nodeExe,
          npmCli,
          ['approve-scripts', '--cache', npmCache, '--no-allow-scripts-pin', ...scriptsToApprove],
          dshDir,
        );
      } catch (err) {
        log(`[警告] npm approve-scripts 执行异常（可能影响原生模块编译）: ${err.message || err}`);
      }
      try {
        runNpm(nodeExe, npmCli, ['rebuild', '--cache', npmCache], dshDir);
      } catch (err) {
        log(`[警告] npm rebuild 执行异常（可能影响原生模块编译）: ${err.message || err}`);
      }
    }
  }

  // 安装完成后裁剪他平台/调试类冗余文件
  pruneDshInstall(dshDir);

  // 记录元信息（与打包脚本 bundle-info.json 同构，供诊断）
  writeFileSync(
    join(target, 'bundle-info.json'),
    JSON.stringify(
      {
        dsh: latest,
        node: process.version,
        platform: process.platform,
        arch: process.arch,
        installed_at: new Date().toISOString(),
        source: 'runtime-update',
      },
      null,
      2,
    ),
  );
  log(`[完成] DeepSeek Harness 后端已更新至 v${latest}`);
  console.log(
    'RESULT ' +
      JSON.stringify({
        ok: true,
        checked: true,
        current: current ?? null,
        latest,
        update_available: true,
        updated: true,
      }),
  );
}

main().catch((e) => {
  console.log(
    'RESULT ' +
      JSON.stringify({
        ok: false,
        checked: true,
        error: String((e && e.message) || e),
      }),
  );
  process.exitCode = 1;
});
