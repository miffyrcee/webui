---
name: 默认编译目标
description: quectel-webui 项目使用 armv7-unknown-linux-musleabihf 交叉编译
type: project
---

项目使用 `cargo build --target armv7-unknown-linux-musleabihf` 作为默认编译方式。

**Why:** 项目是运行在 ARMv7 架构的嵌入式设备上，使用 musl libc 提供静态链接。

**How to apply:** 用户说"编译"时，应使用 `cargo build --target armv7-unknown-linux-musleabihf` 而不是默认的 `cargo build`。
