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