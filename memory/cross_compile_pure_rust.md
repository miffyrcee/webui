---
name: 纯 Rust 交叉编译约束
description: armv7-unknown-linux-musleabihf 交叉编译要求所有依赖为纯 Rust，无 C 代码依赖
type: reference
---

armv7-unknown-linux-musleabihf 交叉编译约束：
- 使用 rust-lld 链接器，无外部 C 编译器（arm-linux-musleabihf-gcc 不可用）
- 所有依赖必须是纯 Rust crate，不能依赖 ring、openssl 等含 C 代码的 crate
- jsonwebtoken 依赖 ring（C 代码）→ 使用手写 HS256 JWT（hmac + sha2 + base64）
- argon2 纯 Rust，可正常编译
- anyhow 纯 Rust，可正常编译

**Why:** 项目在 Windows 上交叉编译，无 ARM C 工具链可用。rust-lld 只能链接 LLVM IR/ Rust 原生目标文件，不能编译 C 源码。

**How to apply:** 添加新依赖前先用 `cargo tree` 检查是否引入 ring/openssl/cc 等 C 依赖。必须使用纯 Rust 替代方案。
