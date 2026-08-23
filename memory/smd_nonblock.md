---
name: SMD 设备不支持 O_NONBLOCK
description: Qualcomm SMD 字符设备不兼容非阻塞模式，必须用其他方案处理阻塞读问题
type: reference
---

Qualcomm SMD (Shared Memory) 字符设备不支持 `O_NONBLOCK` 标志。尝试以非阻塞模式打开（`custom_flags(libc::O_NONBLOCK)`）会导致：
- 设备打开成功，但 `read()` 行为不可控/不支持预期
- 无法通过 `O_NONBLOCK` + `WouldBlock` 实现非阻塞轮询

**Why:** SMD 驱动层在 Qualcomm 平台上的实现不兼容标准的 POSIX 非阻塞 I/O 模型。

**How to apply:** 解决 SMD 阻塞读问题时，不能依赖 `O_NONBLOCK`，需要使用其他方案，例如：
- 专用线程 + 通道（channel）封装串口读写，避免占用 tokio 线程池
- 使用 `tokio::spawn_blocking` + 取消机制
- 在设备驱动层面解决问题
