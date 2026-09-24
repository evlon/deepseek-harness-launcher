#!/usr/bin/env bash
# sign-launcher.sh — 给 deepseek-harness-launcher.exe 打 Authenticode 代码签名（自签内网证书）
#
# 用途：为未签名的 exe 加 Authenticode 签名，去掉 Defender「未知发布者 / 无签名」
#       这两个红旗，降低启发式（!ml）误报概率；内网同事机器若统一部署了
#       本根 CA 到「受信任的发布者」，则签名会被信任。
#
# 用法：
#   COD_SIGN_PFX=/path/to/codesign.pfx COD_SIGN_PW=密码 \
#     bash scripts/sign-launcher.sh [exe 路径]
#
#   - 缺省 exe = src-tauri/target/release/deepseek-harness-launcher.exe
#   - COD_SIGN_PFX / COD_SIGN_PW 必填；未提供时跳过签名并提示
#
# 证书套件生成（一次性，证书放在仓库外，绝不入库）：
#   见 docs/代码签名-自签方案.md
#
# 说明：自签签名 ≠ 消除 Defender 误报（那是 EV/OV 证书 + 下载信誉的事），
#       但它是有意义的第一步，详见文档。

set -euo pipefail

# ⚠️ 关键：MSYS/git-bash 会把 signtool 的 /pa /fd /tr 等单斜杠参数误当成路径转换
#   （实测 /pa → C:/Program Files/Git/pa，/fd 被吞 → 报 "No file digest algorithm
#   specified"）。必须设 MSYS_NO_PATHCONV=1 禁用参数路径转换，否则签名静默失败。
export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

EXE="${1:-$REPO_ROOT/src-tauri/target/release/deepseek-harness-launcher.exe}"

if [ ! -f "$EXE" ]; then
  echo "✗ 未找到 exe：$EXE" >&2
  echo "  先构建：cd src-tauri && cargo build --release" >&2
  exit 1
fi

if [ -z "${COD_SIGN_PFX:-}" ] || [ -z "${COD_SIGN_PW:-}" ]; then
  echo "ℹ 未设置 COD_SIGN_PFX / COD_SIGN_PW，跳过签名（构建不受影响）。" >&2
  echo "  如需签名：COD_SIGN_PFX=/e/ai-works/.launcher-signing/codesign.pfx \\" >&2
  echo "              COD_SIGN_PW=ICT@2026 bash scripts/sign-launcher.sh" >&2
  exit 0
fi

SIGNTOOL="/c/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/signtool.exe"
if [ ! -f "$SIGNTOOL" ]; then
  echo "✗ 未找到 signtool：$SIGNTOOL" >&2
  echo "  请安装 Windows 10 SDK（含 Windows SDK Signing Tools）" >&2
  exit 1
fi

# 转 Windows 路径（PFX 给 signtool 用）
PFX_WIN="$(cygpath -w "$COD_SIGN_PFX" 2>/dev/null || echo "$COD_SIGN_PFX")"
EXE_WIN="$(cygpath -w "$EXE" 2>/dev/null || echo "$EXE")"

echo "==> 签名：$(basename "$EXE")"
echo "    PFX   : $PFX_WIN"

# 先看当前签名状态
echo "==> 签名前状态："
"$SIGNTOOL" verify /pa "$EXE_WIN" 2>&1 || true

# 正式签名：SHA256 摘要 + RFC3161 时间戳
#   /fd SHA256 /td SHA256  现代默认，避免旧 SHA1 被 Windows 拒绝
#   /tr /td 时间戳服务器：内网无外网时可用 /t（旧式）或省略 /tr（不盖章，SmartScreen 会提示"无法验证时间戳"）
#   这里用公网 DigiCert 时间戳；若构建机无外网，去掉 /tr /td 两行即可。
echo "==> 执行签名…"
"$SIGNTOOL" sign \
  /f "$PFX_WIN" \
  /p "$COD_SIGN_PW" \
  /fd SHA256 \
  /tr http://timestamp.digicert.com \
  /td SHA256 \
  "$EXE_WIN"

echo "==> 签名后验证："
"$SIGNTOOL" verify /pa "$EXE_WIN" 2>&1

echo ""
echo "✓ 完成。签名后可用 Get-AuthenticodeSignature 确认 Status=Valid、Signer=CN=DeepSeek Harness Launcher"
