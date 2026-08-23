---
name: AT 命令队列与去重机制
description: AT 命令队列管理器 (AtCommandQueue)，合并相同请求避免重复执行，支持优先级
type: project
---

# AT 命令队列

在 `main.rs:380` 引入 `AtCommandQueue`，作为全局静态队列 (`AT_CMD_QUEUE`)：

- **去重**：相同 AT 命令文本并发请求时，仅执行一次，结果广播给所有等待者
- **优先级**：轮询循环中通过 `actor_rx.is_empty()` 检查——用户请求待处理时跳过本轮 AT 命令轮询
- **底层互斥**：`send_at_command_inner` 保留全局 `AT_CMD_LOCK`，确保同一时刻只有一个 atcmd_rs 进程

## 使用范围

| 场景 | 函数 | 说明 |
|------|------|------|
| 轮询 | `send_at_command_dedup` | 6 个遥测命令全部去重 |
| 手动 AT | `send_at_command_dedup` | ManualAt 使用队列 |
| send_at_get_line | `send_at_command_dedup` | 间接使用队列 |
| 状态变更命令 | `send_at_command_inner` 直接调用 | SetApn/SendSms/Reboot 等不经过队列 |

## 文件

定义在 `src/main.rs` 第 380-440 行。
