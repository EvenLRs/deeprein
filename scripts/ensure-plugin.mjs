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
  rmSync,
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

// 已退役/被新包取代的旧插件清单：在更新时自动从 profile 中卸载并从 bundles 列表移除，避免 loader entry id 冲突
const RETIRED_PACKAGES = [
  { name: '@deeprein/dsh-messaging', supersededBy: 'dsh-messaging' },
];

function pruneRetiredPackages(profileDir, pnpmCli, nodeExe) {
  const manifestPath = join(profileDir, 'package.json');
  if (!existsSync(manifestPath)) return [];
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  } catch {
    return [];
  }

  const removed = [];
  const deps = manifest.dependencies || {};
  const bundles = Array.isArray(manifest.dsh?.profile?.bundles) ? manifest.dsh.profile.bundles : [];
  let manifestChanged = false;

  for (const item of RETIRED_PACKAGES) {
    const isPresentInDeps = Object.prototype.hasOwnProperty.call(deps, item.name);
    const isPresentInBundles = bundles.includes(item.name);
    const isInstalled = Boolean(installedVersion(profileDir, item.name));

    if (!isPresentInDeps && !isPresentInBundles && !isInstalled) {
      continue;
    }

    // 守卫：只有当取代它的新包确实已安装在 profile 时才执行移除，避免功能真空
    if (item.supersededBy && !installedVersion(profileDir, item.supersededBy)) {
      log(`[退役插件] 检测到 ${item.name}，但取代包 ${item.supersededBy} 尚未安装，延后移除`);
      continue;
    }

    log(`[退役插件] 正在移除已被取代的旧插件：${item.name}（取代包：${item.supersededBy || 'none'}）`);

    // 注意：pnpm remove 不支持 --ignore-scripts（那是 pnpm add 的选项），传了会直接报
    // "Unknown option: 'ignore-scripts'" 而整条命令失败。remove 本身不跑安装脚本，无需该选项。
    // 另：remove 要求依赖仍在 package.json 中，故必须在下面的 manifest 手术之前执行。
    if (pnpmCli && existsSync(pnpmCli) && isPresentInDeps) {
      const removeRun = spawnSync(
        nodeExe,
        [pnpmCli, '-C', profileDir, 'remove', item.name],
        { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' },
      );
      if (removeRun.stdout) log(removeRun.stdout.trimEnd());
      if (removeRun.stderr && removeRun.status !== 0) log(removeRun.stderr.trimEnd());
    }

    // 重新读取或直接更新 manifest（防止 pnpm remove 修改了其他字段）
    try {
      const currentManifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
      if (currentManifest.dependencies && currentManifest.dependencies[item.name]) {
        delete currentManifest.dependencies[item.name];
        manifestChanged = true;
      }
      if (Array.isArray(currentManifest.dsh?.profile?.bundles)) {
        const idx = currentManifest.dsh.profile.bundles.indexOf(item.name);
        if (idx >= 0) {
          currentManifest.dsh.profile.bundles.splice(idx, 1);
          manifestChanged = true;
        }
      }
      if (manifestChanged) {
        writeFileSync(manifestPath, JSON.stringify(currentManifest, null, 2) + '\n');
      }
    } catch {
      /* ignore */
    }

    // 兜底删除残留目录：pnpm remove 不可用或失败（文件锁、EPERM）时，manifest 已清理但
    // node_modules 里仍留着旧包。此时若不删，installedVersion 会一直判定「已安装」，
    // 导致每次启动都重复走一遍移除流程、始终返回 installed=true，跳过逻辑永不生效。
    // 该残留目录对 loader 无害（reconcileBundles 只遍历 manifest 列出的插件，不会把它加回
    // bundles），但会拖慢每次启动，故一并清掉以保证幂等。
    try {
      const staleDir = join(profileDir, 'node_modules', item.name);
      if (existsSync(staleDir)) {
        rmSync(staleDir, { recursive: true, force: true });
      }
    } catch (err) {
      log(`[退役插件] 残留目录清理失败（不影响加载）：${err?.message ?? err}`);
    }

    removed.push(item.name);
  }

  return removed;
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
  const retiredRemoved = pruneRetiredPackages(profile, pnpmCli, nodeExe);
  const reconciled = reconcileBundles(profile, plugins);
  if (!missing.length && !reconciled && !retiredRemoved.length) {
    log('Harness 插件均已安装且版本一致，跳过');
    result({
      ok: true,
      profile_missing: false,
      installed: false,
      removed: retiredRemoved,
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
    // pnpm add 之后再次 pruneRetiredPackages + reconcile，确保 bundles 列表包含新装声明且不含退役包
    pruneRetiredPackages(profile, pnpmCli, nodeExe);
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
        { installed_at: new Date().toISOString(), plugins: installed, removed: retiredRemoved },
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
    removed: retiredRemoved,
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
