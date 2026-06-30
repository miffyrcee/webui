#!/usr/bin/env bash
# scripts/deploy-test.sh — 一键构建部署测试 quectel-webui（适用于 Git Bash / WSL）
#
# 用法:
#   ./scripts/deploy-test.sh                  # 完整流程
#   ./scripts/deploy-test.sh -t 30            # 自定义超时
#   ./scripts/deploy-test.sh -s               # 跳过构建
#   ./scripts/deploy-test.sh -k               # 跳过杀进程
#   ./scripts/deploy-test.sh -d "SERIAL"      # 指定 ADB 设备
#   ./scripts/deploy-test.sh -p               # 只推送不运行
#
# 设备端手动运行:
#   adb shell "/opt/bin/quectel-webui &"

set -euo pipefail

TARGET="armv7-unknown-linux-musleabihf"
BINARY_NAME="quectel-webui"
REMOTE_PATH="/opt/bin/${BINARY_NAME}"
BINARY_PATH="target/${TARGET}/release/${BINARY_NAME}"

TIMEOUT=12
SKIP_BUILD=false
SKIP_KILL=false
DEVICE=""
PUSH_ONLY=false

while getopts "t:skd:p" opt; do
    case "$opt" in
        t) TIMEOUT="$OPTARG" ;;
        s) SKIP_BUILD=true ;;
        k) SKIP_KILL=true ;;
        d) DEVICE="$OPTARG" ;;
        p) PUSH_ONLY=true ;;
        *) echo "用法: $0 [-t timeout] [-s] [-k] [-d device] [-p]"; exit 1 ;;
    esac
done

ADB_EXTRA=()
if [ -n "$DEVICE" ]; then
    ADB_EXTRA=(-s "$DEVICE")
fi

info()  { echo "==> $*"; }
ok()    { echo "  ✓ $*"; }
warn()  { echo "  ⚠ $*"; }
err()   { echo "  ✗ $*"; }

# ---- 前置检查 ----
info "检查依赖"
if ! command -v adb &>/dev/null; then
    err "adb 未安装或不在 PATH 中"
    exit 1
fi
ok "adb 可用"

# ---- Step 1: 交叉编译 ----
if [ "$SKIP_BUILD" = false ]; then
    info "交叉编译 (${TARGET} / release)"
    if ! rustup target list --installed 2>/dev/null | grep -q "^${TARGET}$"; then
        warn "目标 ${TARGET} 未安装，正在添加..."
        rustup target add "$TARGET"
    fi
    cargo build --target "$TARGET" --release
    ok "编译完成: ${BINARY_PATH}"
else
    info "跳过构建"
fi

# ---- 检查二进制 ----
if [ ! -f "$BINARY_PATH" ]; then
    err "找不到二进制: ${BINARY_PATH}（请先构建或取消 -s）"
    exit 1
fi

# ---- Step 2: 检查 ADB 设备 ----
info "检查 ADB 设备"
if [ -n "$DEVICE" ]; then
    adb "${ADB_EXTRA[@]}" get-state &>/dev/null || {
        err "ADB 设备连接失败"
        exit 1
    }
else
    adb devices &>/dev/null || {
        err "ADB 不可用"
        exit 1
    }
fi
ok "设备就绪"

# ---- Step 3: 杀掉旧进程 ----
if [ "$SKIP_KILL" = false ]; then
    info "杀掉设备上的旧 webui 进程"
    adb "${ADB_EXTRA[@]}" shell "pkill -9 -f ${BINARY_NAME}" 2>/dev/null || true
    ok "已清理旧进程"
else
    info "跳过杀进程"
fi

# ---- Step 4: 推送 ----
info "推送二进制到 ${REMOTE_PATH}"
adb "${ADB_EXTRA[@]}" push "$BINARY_PATH" "$REMOTE_PATH"
ok "推送完成"

# ---- Step 5: 运行测试 ----
if [ "$PUSH_ONLY" = true ] || [ "$TIMEOUT" -le 0 ]; then
    ok "推送完成（未运行）"
    exit 0
fi

info "设置权限并运行（超时 ${TIMEOUT}s）"
set +e
adb "${ADB_EXTRA[@]}" shell "chmod 755 ${REMOTE_PATH} && RUST_BACKTRACE=1 timeout ${TIMEOUT} ${REMOTE_PATH}"
EXIT_CODE=$?
set -e

echo ""
echo "════════════════════════════════════════"
echo "  ADB shell 退出码: ${EXIT_CODE}"
echo "  （124 = timeout 正常退出，0 = 进程自行退出）"
echo "════════════════════════════════════════"

if [ "$EXIT_CODE" -eq 124 ] || [ "$EXIT_CODE" -eq 0 ]; then
    ok "部署测试完成"
else
    warn "进程异常退出（代码: ${EXIT_CODE}），请检查设备端日志"
    echo "  查看日志: adb shell logcat | grep webui"
fi
