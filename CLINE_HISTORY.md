
## 2026-06-05 00:17 — 优化串口写入和读取逻辑
- **修改内容**:
  - `SerialHandle::write_all()`: 增加了写入重试机制，处理 `WouldBlock` 和 `EBUSY` 错误，避免在非阻塞设备（如 SMD 通道）上写入永久阻塞。
  - `SerialHandle::read()`: 增加了对 `WouldBlock` 错误的特殊处理，将其视为读取到 0 字节，而非直接返回错误。
  - `send_at_command_inner()`: 简化了 AT 命令响应的结束符检查逻辑，不再使用复杂的 `windows` 匹配，改为直接检查 `ends_with`。
- **涉及文件**:
  - `src/main.rs`