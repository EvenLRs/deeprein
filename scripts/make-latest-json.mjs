// 从 tauri build 的签名产物组装 updater 的 latest.json。
// 用法：
//   node scripts/make-latest-json.mjs --version 0.1.0 --tag v0.1.0 [--notes "..."] [--scan <目录>] [--out latest.json]
//
// --scan 目录（递归）下查找 *.sig 及其对应安装包：
//   macOS   : deeprein_<ver>_aarch64.app.tar.gz(.sig)  → darwin-aarch64
//   Windows : deeprein_<ver>_x64-setup.exe(.sig)       → windows-x86_64
// 产物 URL 指向 GitHub Releases：https://github.com/<repo>/releases/download/<tag>/<文件>
import { readFileSync, writeFileSync } from 'node:fs';
import { join, relative, dirname } from 'node:path';

const REPO = process.env.UPDATER_REPO || 'EvenLRs/deeprein';

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const m = a.match(/^--([^=]+)=(.*)$/);
    if (m) {
      args[m[1]] = m[2];
      continue;
    }
    if (a.startsWith('--') && i + 1 < argv.length) {
      args[a.slice(2)] = argv[i + 1];
      i++;
    }
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const version = args.version || null;
const tag = args.tag || (version ? `v${version}` : null);
if (!tag) throw new Error('缺少 --tag（发布标签，如 v0.1.0）');
if (!version) throw new Error('缺少 --version（应用版本，如 0.1.0）');

// 递归收集 .sig 文件
async function collectSigs(dir) {
  const { readdirSync, statSync } = await import('node:fs');
  const out = [];
  const walk = (d) => {
    for (const name of readdirSync(d)) {
      const p = join(d, name);
      if (statSync(p).isDirectory()) walk(p);
      else if (p.endsWith('.sig')) out.push(p);
    }
  };
  walk(dir);
  return out;
}

function platformOf(sigPath) {
  const s = sigPath.replaceAll('\\', '/');
  if (s.includes('aarch64-apple-darwin') || s.includes('macos')) return 'darwin-aarch64';
  if (s.includes('x86_64-pc-windows-msvc') || s.includes('windows')) return 'windows-x86_64';
  return null;
}

const scanRoot = args.scan || 'src-tauri/target';
const sigs = await collectSigs(scanRoot);
if (sigs.length === 0) {
  console.error(`[FAIL] 在 ${scanRoot} 下未找到 .sig 文件。`);
  console.error('请确认构建时设置了 TAURI_SIGNING_PRIVATE_KEY（tauri build 才会签名生成 .sig）。');
  process.exit(1);
}

const platforms = {};
for (const sig of sigs) {
  const platform = platformOf(sig);
  if (!platform) {
    console.warn(`[跳过] 无法识别平台的签名文件: ${sig}`);
    continue;
  }
  // 对应安装包 = 去掉 .sig 后缀（tauri bundler 的约定：<artifact>.sig 与 <artifact> 同目录）
  const artifact = sig.slice(0, -'.sig'.length);
  const artifactName = artifact.split('/').pop();
  const url = `https://github.com/${REPO}/releases/download/${tag}/${artifactName}`;
  const signature = readFileSync(sig, 'utf8').trim();
  if (platforms[platform]) {
    console.warn(`[覆盖] ${platform} 已有条目，后者生效: ${artifact}`);
  }
  platforms[platform] = { signature, url };
  console.log(`[OK] ${platform}: ${artifactName} (签名 ${signature.length} 字节)`);
}

const required = ['darwin-aarch64', 'windows-x86_64'];
for (const p of required) {
  if (!platforms[p]) {
    console.warn(`[警告] 缺少平台 ${p} 的更新包（双平台发布时请检查 CI 产物）`);
  }
}

const latest = {
  version,
  notes: args.notes || `deeprein ${version}`,
  pub_date: new Date().toISOString(),
  platforms,
};

const outPath = args.out || 'latest.json';
writeFileSync(outPath, JSON.stringify(latest, null, 2) + '\n');
console.log(`[OK] 已生成 ${outPath}`);
console.log(JSON.stringify(latest, null, 2));
