<#
.SYNOPSIS
  一键构建、部署并测试 quectel-webui 到 Android 设备

.DESCRIPTION
  完整流程：
  1. 交叉编译 armv7 release 二进制
  2. ADB 杀掉设备上旧 webui 进程
  3. 推送新二进制到 /opt/bin/
  4. 设置执行权限并运行（12 秒超时测试）

  支持跳过构建、跳过杀进程、自定义超时等选项。

.PARAMETER Timeout
  设备端运行测试的超时秒数，默认 12

.PARAMETER SkipBuild
  跳过 cargo build 步骤，直接部署已有的二进制

.PARAMETER SkipKill
  跳过杀进程步骤，直接推送并运行

.PARAMETER Device
  指定 ADB 设备序列号（多设备时使用）

.EXAMPLE
  # 完整流程
  ./scripts/deploy-test.ps1

  # 只推送不运行（快速更新）
  ./scripts/deploy-test.ps1 -SkipKill -Timeout 0

  # 指定设备
  ./scripts/deploy-test.ps1 -Device "0123456789abcdef"

  # 只部署已有的二进制
  ./scripts/deploy-test.ps1 -SkipBuild

  # 手动在设备后台常驻
  adb shell "/opt/bin/quectel-webui &"
#>

param(
    [int]    $Timeout   = 12,
    [switch] $SkipBuild,
    [switch] $SkipKill,
    [string] $Device    = ""
)

$Target     = "armv7-unknown-linux-musleabihf"
$BinaryName = "quectel-webui"
$RemotePath = "/opt/bin/$BinaryName"
$BinaryPath = "target/$Target/release/$BinaryName"

$AdbArgs    = @()
if ($Device) { $AdbArgs = @("-s", $Device) }

function Write-Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "  ✓ $msg" -ForegroundColor Green }
function Write-Warn($msg) { Write-Host "  ⚠ $msg" -ForegroundColor Yellow }
function Write-Err($msg)  { Write-Host "  ✗ $msg" -ForegroundColor Red }

# ---- 前置检查 ----
Write-Step "检查依赖"

$hasAdb = Get-Command adb -ErrorAction SilentlyContinue
if (-not $hasAdb) {
    Write-Err "adb 未安装或不在 PATH 中。请安装 Android Debug Bridge。"
    exit 1
}
Write-Ok "adb 可用"

# ---- Step 1: 交叉编译 ----
if (-not $SkipBuild) {
    Write-Step "交叉编译 ($Target / release)"

    $targetInstalled = rustup target list --installed 2>$null |
        Select-String -Pattern "^$target " -Quiet
    if (-not $targetInstalled) {
        Write-Warn "目标 $target 未安装，正在添加..."
        rustup target add $target
        if ($LASTEXITCODE -ne 0) {
            Write-Err "添加目标 $target 失败"
            exit 1
        }
    }

    cargo build --target $Target --release
    if ($LASTEXITCODE -ne 0) {
        Write-Err "编译失败"
        exit 1
    }
    Write-Ok "编译完成: $BinaryPath"
} else {
    Write-Step "跳过构建"
}

# ---- 检查二进制存在 ----
if (-not (Test-Path $BinaryPath)) {
    Write-Err "找不到二进制: $BinaryPath （请先构建或取消 -SkipBuild）"
    exit 1
}

# ---- Step 2: 检查 ADB 设备 ----
Write-Step "检查 ADB 设备"

$devices = if ($Device) {
    adb -s $Device get-state 2>$null
} else {
    adb devices
}
if ($LASTEXITCODE -ne 0) {
    Write-Err "ADB 设备连接失败，请检查 USB / 网络 ADB"
    exit 1
}
Write-Ok "设备就绪"

# ---- Step 3: 杀掉旧进程 ----
if (-not $SkipKill) {
    Write-Step "杀掉设备上的旧 webui 进程"
    adb @AdbArgs shell "pkill -9 -f $BinaryName" 2>$null
    Write-Ok "已清理旧进程"
} else {
    Write-Step "跳过杀进程"
}

# ---- Step 4: 推送二进制 ----
Write-Step "推送二进制到 $RemotePath"
adb @AdbArgs push "$BinaryPath" "$RemotePath"
if ($LASTEXITCODE -ne 0) {
    Write-Err "推送失败"
    exit 1
}
Write-Ok "推送完成"

# ---- Step 5: 设置权限并运行测试 ----
Write-Step "设置权限并运行（超时 ${Timeout}s）"

$remoteCmd = "chmod 755 $RemotePath"
if ($Timeout -gt 0) {
    $remoteCmd += " && RUST_BACKTRACE=1 timeout $Timeout $RemotePath"
} else {
    # Timeout=0 只推送不运行
    Write-Ok "推送完成（未运行，Timeout=0）"
    exit 0
}

adb @AdbArgs shell $remoteCmd
$exitCode = $LASTEXITCODE

Write-Host "`n" -NoNewline
Write-Host "════════════════════════════════════════" -ForegroundColor DarkGray
Write-Host "  ADB shell 退出码: $exitCode" -ForegroundColor DarkGray
Write-Host "  （124 = timeout 正常退出，0 = 进程自行退出）" -ForegroundColor DarkGray
Write-Host "════════════════════════════════════════" -ForegroundColor DarkGray

if ($exitCode -eq 124 -or $exitCode -eq 0) {
    Write-Ok "部署测试完成"
}
else {
    Write-Warn "进程异常退出（代码: $exitCode），请检查设备端日志"
    Write-Host "  查看日志: adb shell logcat | Select-String webui" -ForegroundColor Gray
}
