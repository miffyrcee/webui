
## 2026-06-05 00:17 — 优化串口写入和读取逻辑
- **修改内容**:
  - `SerialHandle::write_all()`: 增加了写入重试机制，处理 `WouldBlock` 和 `EBUSY` 错误，避免在非阻塞设备（如 SMD 通道）上写入永久阻塞。
  - `SerialHandle::read()`: 增加了对 `WouldBlock` 错误的特殊处理，将其视为读取到 0 字节，而非直接返回错误。
  - `send_at_command_inner()`: 简化了 AT 命令响应的结束符检查逻辑，不再使用复杂的 `windows` 匹配，改为直接检查 `ends_with`。
- **涉及文件**:
  - `src/main.rs`

## 2026-06-05 00:48 — 将串口通信替换为 atcmd 命令行工具
- **修改内容**:
  - 移除所有串口直接读写代码（`SerialHandle` 枚举、`open_serial()`、`send_at_command_inner()`、`send_at_command_async()`），以及 `DEFAULT_SERIAL_PORT` 常量
  - 新增 `atcmd_exec()` 函数，通过 `tokio::process::Command` 调用模块内置的 `atcmd` 命令行工具执行 AT 命令
  - 新增 `check_atcmd_available()` 函数，启动时自动检测 `atcmd` 工具是否可用（先 `which atcmd`，再执行 `atcmd AT` 验证）
  - `main()` 不再读取 `AT_SERIAL_PORT` 环境变量，改为启动时检测 `atcmd` 可用性，不可用时自动降级为模拟模式
  - `fetch_static_info()` 和 `hardware_polling_actor()` 参数从 `serial_path: &str` / `serial: Option<SerialHandle>` 改为 `atcmd_available: bool`
  - 移除了 `std::env`、`tokio::io::{AsyncReadExt, AsyncWriteExt}`、`tokio::time::timeout` 等不再需要的导入
  - 所有 AT 命令发送点统一使用 `atcmd_exec(cmd)` 替代原来的 `send_at_command_inner(&mut serial, cmd)`
  - 模拟模式逻辑保持不变，当 `atcmd_available == false` 时返回预定义模拟数据
- **涉及文件**:
  - `src/main.rs`
