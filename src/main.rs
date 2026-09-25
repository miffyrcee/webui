//! 进程引导：环境装配、状态接线与端口监听。

use std::sync::Arc;

use quectel_webui::{
    actor::hardware_task,
    backend::{HardwareBackend, MockBackend, RealBackend},
    config::AppConfig,
    logger::{init_log_worker, log_worker_task, push_log},
    web::{build_router, serve::serve_web, state::AppState},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 注册纯 Rust 的 RustCrypto 为全局 TLS Provider（关键：避免拉取 aws-lc-rs/ring C 依赖）
    rustls_rustcrypto::provider()
        .install_default()
        .expect("Failed to install RustCrypto provider");

    let log_rx = init_log_worker();
    tokio::spawn(log_worker_task(log_rx));

    let config = AppConfig::from_env();
    let (app_state, command_rx, telemetry_tx) = AppState::new(&config);

    // 根据串口设备是否存在选择后端
    let backend: Arc<dyn HardwareBackend> = if std::path::Path::new(&config.serial_path).exists() {
        Arc::new(RealBackend::new(config.serial_path.clone()).await)
    } else {
        push_log("WARN", "System", "未检测到串口设备，已自动开启模拟测试模式 (MockBackend)");
        Arc::new(MockBackend)
    };
    let device_name = backend.device_name().to_string();
    push_log("INFO", "System", &format!("检测到设备: {}", device_name));

    // 启动硬件后台轮询与指令消费任务（内含静态信息初始化）
    tokio::spawn({
        let (backend, telemetry_tx, state) =
            (backend.clone(), telemetry_tx.clone(), app_state.clone());
        async move { hardware_task(command_rx, backend, telemetry_tx, state).await }
    });

    // 组装路由并按配置启动监听（HTTPS / HTTP 重定向 / 纯 HTTP 回退）
    serve_web(build_router(app_state), &config, &device_name).await;

    Ok(())
}
