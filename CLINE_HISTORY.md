# Cline AI Change History

## 2026-05-29 00:19 — 修复串口打开失败问题
- **问题**: 代码使用 `tokio::fs::OpenOptions` 直接打开串口设备文件，未配置串口参数（波特率、数据位等），导致 `Not a typewriter` 错误
- **修改内容**:
  - 新增 `open_serial()` 函数，使用 `tokio_serial` 正确配置 115200 8N1
  - `fetch_static_info()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `hardware_polling_actor()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `send_at_command_async()` 参数类型 `&mut File` → `&mut SerialStream`
- **涉及文件**:
  - `src/main.rs`

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
