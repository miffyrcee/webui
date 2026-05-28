# Cline AI Change History

## 2026-05-29 01:32 — 移除 tokio-serial 依赖
- **修改内容**: 完全移除 `tokio-serial` crate 的使用
  - 删除 `Cargo.toml` 中的 `tokio-serial = "5.4.5"` 依赖
  - 删除 `SerialHandle::Tty` 变体，只保留 `Raw(tokio::fs::File)`
  - 删除 `skip_tty_mode()` 函数
  - 简化 `open_serial()` 只使用 `tokio::fs::File` 直接打开设备（SMD/socat PTY 等非TTY设备）
  - 删除 `drain_serial_buffer()`、`send_at_command_async()` 中所有 Tty 模式匹配分支
  - `send_at_command_async()` 中 SMD 200ms 延迟始终执行（不再条件判断）
- **原因**: 实际设备使用 `/dev/smd11`（SMD 通道）或 socat PTY 桥接设备（`/dev/ttyIN`），都不是标准 TTY 串口。`tokio_serial` 的 `tcsetattr` 反而会破坏 socat 的 raw 配置，且 SMD 通道也不支持 TTY ioctl。所有设备都通过 Raw 文件模式工作正常
- **涉及文件**:
  - `Cargo.toml`
  - `src/main.rs`

## 2026-05-29 01:27 — 验证 /dev/smd11 SMD 通道直连正常
- **验证内容**: `cat /dev/smd11` 测试确认 `/dev/smd11` 为可直接 AT 通信的 SMD 通道
- **结果**: ATI 正确返回 Quectel RG520N-EU (Revision: RG520NEUDCR01A02M4G_TP)
- **说明**: 代码已配置 `/dev/smd11` 为默认串口，且 `open_serial()` 的 TTY→Raw 回退策略正确处理 SMD 设备。无需额外修改
- **涉及文件**:
  - 无需代码修改（仅本文档记录）

## 2026-05-29 01:16 — 修改默认串口设备为 /dev/smd11
- **修改内容**: 将 `DEFAULT_SERIAL_PORT` 从 `/dev/ttyIN` 改为 `/dev/smd11`
- **原因**: 设备默认使用 SMD 通道 `/dev/smd11` 进行 AT 通信，无需 socat PTY 桥接
- **涉及文件**:
  - `src/main.rs`

## 2026-05-29 00:19 — 修复串口打开失败问题
- **问题**: 代码使用 `tokio::fs::OpenOptions` 直接打开串口设备文件，未配置串口参数（波特率、数据位等），导致 `Not a typewriter` 错误
- **修改内容**:
  - 新增 `open_serial()` 函数，使用 `tokio_serial` 正确配置 115200 8N1
  - `fetch_static_info()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `hardware_polling_actor()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `send_at_command_async()` 参数类型 `&mut File` → `&mut SerialStream`
- **涉及文件**:
  - `src/main.rs`

## 2026-05-29 01:12 — ADB 实际调试：发现 AT 通道为 socat PTY 桥接 /dev/ttyIN
- **问题**: SMD 通道（smd7/smd8/smd11/smd21/smd22）和 ttyMSM0/ttyGS0 全部无 AT 响应，程序卡在读取超时
- **调试过程**:
  - `adb shell "ls -la /dev/smd* /dev/tty*"` → 列出所有串口设备
  - `adb shell "ps | grep -iE 'ril|modem|qmux|qmi|at|radio'"` → 发现有 `socat-at-bridge` 进程：
    `socat-armel-static pty,link=/dev/ttyIN,raw,echo=0 pty,link=/dev/ttyOUT,raw,echo=1`
  - `adb shell "echo -ne 'AT\r\n' > /dev/ttyIN && sleep 0.3 && timeout 2 cat /dev/ttyOUT"` → 确认 /dev/ttyIN 返回 AT 响应（FW: RG520NEUDCR01A02M4G_TP）
- **修复内容**:
  - 默认串口从 `/dev/ttyMSM0` 改为 `/dev/ttyIN`（socat PTY 桥接 AT 通道）
  - 新增 `skip_tty_mode()` 函数：socat PTY 桥接设备（ttyIN/ttyOUT）不能用 `tokio_serial` 打开（tcsetattr 会破坏 socat 的 raw 配置），必须跳过 TTY 模式直接用 Raw 文件模式
  - `open_serial()` 增加 TTY 跳过逻辑，先判断是否 socat 设备，再决定用哪种模式
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-05-29 00:58 — SMD 通道通信修复 + Skill 文档更新
- **问题**: `/dev/smd11` (Qualcomm SMD) Raw 模式下 AT 命令返回空或无响应
- **修复内容**:
  - 新增 `drain_serial_buffer()` 函数，启动时清空串口残留数据
  - `fetch_static_info()` 中对 Raw 模式增加 ATE0 初始化握手（关闭回显）
  - `hardware_polling_actor()` 中增加 SMD 初始化握手（drain + ATE0）
  - `send_at_command_async()` 中 0 字节读取不再直接 break，改为连续 3 次后才退出（SMD 通道 0 字节不代表 EOF）
- **Skill 文档更新**:
  - `.clinerules` 重写了 Run Commands 节，拆分为 Cross-compilation (armhf)、ADB Deployment、Run Commands 三个独立章节
  - 新增 armv7 musl 交叉编译文档（target setup、build 命令、产物路径）
  - 新增 ADB 部署完整指南（前置条件、常用命令、一键构建部署、只推送、手动运行、日志查看、串口排查）
- **涉及文件**:
  - `src/main.rs`
  - `.clinerules`
  - `CLINE_HISTORY.md`
  - `Cargo.toml`

## 2026-05-29 00:28 — 创建 .clinerules 和 .clineignore
- **描述**: 为项目创建 Cline 遵守规则文件和忽略文件清单
- **涉及文件**:
  - `.clinerules`
  - `.clineignore`

## 2026-05-29 00:31 — 添加 AI 修改历史记录规则
- **描述**: 在 `.clinerules` 中添加 `AI Change History` 节，要求每次 AI 修改代码必须追加记录到 `CLINE_HISTORY.md`
- **涉及文件**:
  - `.clinerules`
  - `CLINE_HISTORY.md`

## 2026-05-29 00:36 — 修复串口卡死（日志停在"串口设备: /dev/smd11"无下文）
- **问题**: 代码使用 `tokio::fs::File`（`OpenOptions`）打开串口设备 `/dev/smd11`，但由于未配置波特率等串口参数，`read()` 调用永久阻塞，导致程序卡在 `fetch_static_info`
- **根因**: `tokio::fs::File` 是普通文件 I/O，操作串口设备时不设置波特率、数据位、停止位等参数，系统内核无法正确读取串口数据，`read` 调用无限等待
- **修改内容**:
  - 新增 `open_serial()` 函数，使用 `tokio_serial::SerialStream` 并配置 115200 波特率
  - `fetch_static_info()` 改用 `open_serial()` 临时打开串口，完成后主动 `drop` 关闭（供 `hardware_polling_actor` 后续独立重开）
  - `hardware_polling_actor()` 改用 `open_serial()` 打开独立串口连接
  - `send_at_command_async()` 参数类型从 `&mut File` 改为 `&mut SerialStream`
  - 移除不再使用的 `tokio::fs::{File, OpenOptions}` 和 `tokio::io` 中不再需要的导入
- **涉及文件**:
  - `src/main.rs`
