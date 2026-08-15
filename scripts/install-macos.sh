#!/usr/bin/env bash
# DeepRein macOS 安装/修复脚本
#
# 用途：把 DeepRein.app 安装到 /Applications，并解除下载隔离（quarantine）、
# 刷新 Finder/Dock 图标缓存。用于修复以下症状：
#   - Applications 中图标显示问号
#   - 双击无反应 / 提示「已损坏，无法打开」/「无法验证开发者」
#
# 用法：
#   ./scripts/install-macos.sh [DeepRein.app 路径]
#   不带参数时自动从以下位置选择最新可用的应用包：
#     1) 已挂载的 DMG（/Volumes/DeepRein*/DeepRein.app 等）
#     2) 本地 release 构建（src-tauri/target/*/release/bundle/macos/DeepRein.app）
set -euo pipefail

info() { printf '\033[1;34m[安装]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[警告]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[错误]\033[0m %s\n' "$*" >&2; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_DEST="/Applications/DeepRein.app"

# 应用包完整性检查：必须有 Info.plist 与主程序；内置后端若存在则 Node 不能是 0 字节
# （0 字节说明 bundle-backend.mjs 未完成，装上也无法拉起 Harness）。
valid_app() {
  local app="$1"
  [[ -f "$app/Contents/Info.plist" ]] || return 1
  [[ -x "$app/Contents/MacOS/deeprein" ]] || return 1
  local node="$app/Contents/Resources/_up_/backend/node/bin/node"
  if [[ -e "$node" ]]; then
    if [[ ! -s "$node" ]]; then
      warn "跳过 ${app}：内置 Node 为 0 字节（后端打包不完整）"
      return 1
    fi
  fi
  return 0
}

SRC="${1:-}"
if [[ -z "$SRC" ]]; then
  # 按修改时间从新到旧扫描候选，取第一个完整可用的包
  while IFS= read -r candidate; do
    if valid_app "$candidate"; then
      SRC="$candidate"
      break
    fi
  done < <(
    for c in \
      /Volumes/DeepRein*/DeepRein.app \
      /Volumes/deeprein*/deeprein.app \
      "$REPO_ROOT/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/DeepRein.app"; do
      [[ -d "$c" ]] && printf '%s\t%s\n' "$(stat -f '%m' "$c")" "$c"
    done | sort -rn | cut -f2-
  )
fi

[[ -n "$SRC" ]] || die "未找到可用的 DeepRein.app（请先构建或挂载 DMG，或显式传入 .app 路径）"
[[ -d "$SRC" ]] || die "源应用不存在：$SRC"
valid_app "$SRC" || die "源应用不完整：$SRC"

# 清理旧的损坏安装（含旧小写名与更新替换失败的残留），这是修复问号图标的关键
for old in /Applications/DeepRein.app /Applications/deeprein.app; do
  if [[ -e "$old" ]]; then
    info "移除旧的 ${old}（损坏/失效副本）"
    rm -rf -- "$old"
  fi
done

info "复制 $SRC → $APP_DEST"
ditto "$SRC" "$APP_DEST"

info "解除下载隔离（quarantine）"
xattr -cr "$APP_DEST" 2>/dev/null || true

if codesign --verify --deep --strict "$APP_DEST" 2>/dev/null; then
  info "代码签名校验通过"
else
  warn "代码签名校验失败；本项目为 ad-hoc 签名，被 Gatekeeper 拒绝属预期，解除隔离后即可运行"
fi

info "刷新 Finder/Dock 图标缓存"
killall Finder 2>/dev/null || true
killall Dock 2>/dev/null || true

info "完成：$APP_DEST"
echo "现在可从「应用程序」双击启动。若仍提示无法验证，再执行："
echo "  xattr -cr /Applications/DeepRein.app"
