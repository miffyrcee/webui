//! Quectel 5G 模组 Web 控制面板 — 库根模块。
//!
//! 业务、驱动与协议实现全部收敛于此库中，`src/main.rs` 仅负责进程引导。

pub mod actor;
pub mod at;
pub mod auth;
pub mod backend;
pub mod config;
pub mod device;
pub mod logger;
pub mod sys;
pub mod tls;
pub mod web;
