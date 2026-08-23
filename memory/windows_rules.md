---
name: Windows 适配规则
description: .clinerules 已适配 Windows 环境，串口路径、构建脚本、部署命令使用 PowerShell/CMD 格式
type: project
---

.clinerules 中的 Linux 特有内容已适配为 Windows 格式：
- **串口路径**: 默认 `/dev/ttyIN` 改为 `COM3`，可通过 `$env:AT_SERIAL_PORT` (PowerShell) 或 `set AT_SERIAL_PORT=` (CMD) 覆盖
- **构建脚本**: bash `&&` 链式命令改为 PowerShell 反引号换行 + 分号
- **错误抑制**: `2>/dev/null` 改为 `2>$null`
- **环境变量**: `VAR=value cmd` 改为 `$env:VAR="value"; cmd` (PowerShell) 或 `set VAR=value && cmd` (CMD)
- **交叉编译**: 注明 Windows 上 musl 工具链需通过 WSL 2 或 mingw 搭建
- **串口排查**: 新增 Windows 本地串口排查方式（设备管理器、mode 命令）
- **新增 WSL/Git Bash 章节**: 提供在这些环境下的兼容用法

**Why**: 开发环境从 Linux 迁移到 Windows，需要使项目文档中的命令可直接在 Windows 终端执行
**How to apply**: 在 Windows PowerShell 中直接复制使用 `.clinerules` 中的命令，无需手动转换
