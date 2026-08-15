// 运行时把随壳打包的 Harness 插件安装进 dsh web profile（纯 Node 内置模块，无第三方依赖）。
// 由桌面客户端在进入 Harness 前调用：
//   node ensure-plugin.mjs --profile <web profile 目录> --manifest <manifest.json> \
//        --pnpm <pnpm.cjs> --node <node 可执行文件> --target <应用数据目录>
//
// 行为：
//   - profile 不存在（后端尚未首启）→ RESULT { ok:true, profile_missing:true }，由客户端延后处理；
//   - 全部插件已安装且版本一致 → 不修改任何东西，RESULT { ok:true, installed:false }；
//   - 有缺失/版本不符 → tarball 复制进 profile/.deeprein/plugins 并 pnpm add，
//     RESULT { ok:true, installed:true, plugins:[{name,version}] }。
//
// 输出约定：stdout 普通行为进度（同时写入 <target>/plugin.log），最后一行 RESULT <json>。
import { spawnSync } from 'node:child_process';
import {
  appendFileSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';

// ---------- 参数解析 ----------
function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
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
    if (!args.target) args.target = a;
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const profile = args.profile ? resolve(args.profile) : null;
const manifestPath = args.manifest ? resolve(args.manifest) : null;
const pnpmCli = args.pnpm ? resolve(args.pnpm) : null;
const nodeExe = args.node ? resolve(args.node) : process.execPath;
const target = args.target ? resolve(args.target) : null;

function result(obj) {
  console.log('RESULT ' + JSON.stringify(obj));
}

function log(msg) {
  console.log(msg);
  if (target) {
    try {
      mkdirSync(target, { recursive: true });
      appendFileSync(join(target, 'plugin.log'), `[${new Date().toISOString()}] ${msg}\n`);
    } catch {
      /* 日志失败不阻塞主流程 */
    }
  }
}

function installedVersion(profileDir, name) {
  const pkg = join(profileDir, 'node_modules', name, 'package.json');
  try {
    return JSON.parse(readFileSync(pkg, 'utf8')).version ?? null;
  } catch {
    return null;
  }
}

// dsh 的 `dsh plugin add` 除了 pnpm add 之外还会 reconcile `dsh.profile.bundles`：
// 依赖里声明了 dsh.bundle.patch 的包会被追加进 bundles 列表，dsh 启动时才把它作为
// profile 层加载。裸 pnpm add 不会做这一步，这里补齐同等语义。
function reconcileBundles(profileDir, manifestPlugins) {
  const manifestPath = join(profileDir, 'package.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const bundles = Array.isArray(manifest.dsh?.profile?.bundles) ? manifest.dsh.profile.bundles : [];
  let changed = false;
  for (const p of manifestPlugins) {
    const pkgPath = join(profileDir, 'node_modules', p.name, 'package.json');
    let declaresBundle = false;
    try {
      declaresBundle = JSON.parse(readFileSync(pkgPath, 'utf8')).dsh?.bundle?.patch !== undefined;
    } catch {
      /* 未安装成功的跳过 */
    }
    if (declaresBundle && !bundles.includes(p.name)) {
      bundles.push(p.name);
      changed = true;
    }
  }
  if (!changed) return false;
  manifest.dsh = { ...(manifest.dsh ?? {}), profile: { ...(manifest.dsh?.profile ?? {}), bundles } };
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
  return true;
}

try {
  if (!profile || !manifestPath) throw new Error('缺少 --profile / --manifest 参数');
  if (!existsSync(join(profile, 'package.json'))) {
    // 后端还没首启、profile 尚未生成；不代劳创建（dsh 首启会写默认 bundles），由客户端延后
    result({ ok: true, profile_missing: true, installed: false });
    process.exit(0);
  }

  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const plugins = Array.isArray(manifest.plugins) ? manifest.plugins : [];
  if (!plugins.length) {
    log('插件清单为空，跳过');
    result({ ok: true, profile_missing: false, installed: false });
    process.exit(0);
  }

  const missing = plugins.filter((p) => installedVersion(profile, p.name) !== p.version);
  const reconciled = reconcileBundles(profile, plugins);
  if (!missing.length && !reconciled) {
    log('Harness 插件均已安装且版本一致，跳过');
    result({
      ok: true,
      profile_missing: false,
      installed: false,
      plugins: plugins.map((p) => ({ name: p.name, version: p.version })),
    });
    process.exit(0);
  }

  if (missing.length) {
    if (!pnpmCli || !existsSync(pnpmCli)) {
      throw new Error(`未找到随包分发的 pnpm 运行时：${pnpmCli ?? '(未指定)'}`);
    }

    const stageDir = join(profile, '.deeprein', 'plugins');
    mkdirSync(stageDir, { recursive: true });
    const tarballs = [];
    for (const p of missing) {
      const src = join(dirname(manifestPath), p.tarball);
      if (!existsSync(src)) throw new Error(`插件包缺失：${src}`);
      const dst = join(stageDir, p.tarball);
      copyFileSync(src, dst);
      tarballs.push(dst);
    }

    log(`[安装] pnpm: ${pnpmCli}`);
    log(`[安装] 目标 profile: ${profile}`);
    log(`[安装] 待安装：${missing.map((p) => `${p.name}@${p.version}`).join(', ')}`);
    // --ignore-scripts：插件生命周期脚本无需在此执行（如 playwright 浏览器下载走外部 executablePath）
    const run = spawnSync(
      nodeExe,
      [pnpmCli, '-C', profile, 'add', ...tarballs, '--ignore-scripts', '--prefer-offline'],
      { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' },
    );
    if (run.stdout) log(run.stdout.trimEnd());
    if (run.stderr) log(run.stderr.trimEnd());
    if (run.status !== 0) {
      throw new Error(`pnpm add 退出码 ${run.status ?? '未知'}`);
    }
    // pnpm add 之后再次 reconcile，确保 bundles 列表包含本次新装的 bundle 声明
    reconcileBundles(profile, plugins);
  }

  const installed = plugins.map((p) => ({
    name: p.name,
    version: installedVersion(profile, p.name),
    expected: p.version,
  }));
  const bad = installed.filter((x) => x.version !== x.expected);
  if (bad.length) throw new Error('安装后版本校验失败：' + JSON.stringify(bad));

  if (target) {
    mkdirSync(target, { recursive: true });
    writeFileSync(
      join(target, 'plugin-info.json'),
      JSON.stringify(
        { installed_at: new Date().toISOString(), plugins: installed },
        null,
        2,
      ),
    );
  }
  log('[完成] Harness 插件已装入 web profile');
  result({
    ok: true,
    profile_missing: false,
    installed: true,
    plugins: installed.map((x) => ({ name: x.name, version: x.version })),
  });
} catch (e) {
  result({
    ok: false,
    profile_missing: false,
    installed: false,
    error: String((e && e.message) || e),
  });
  process.exitCode = 1;
}
