//! Web 服务启动编排：HTTPS 主服务 + HTTP 重定向 + 纯 HTTP 回退。

use axum::Router;

use crate::config::AppConfig;
use crate::logger::push_log;
use crate::tls::{create_http_redirect_app, load_or_generate_tls_config};

/// 按配置启动监听：
/// - `enable_https` 关闭时走纯 HTTP；
/// - TLS 初始化失败时自动回退纯 HTTP；
/// - HTTPS 可用且端口不冲突时，后台另起 HTTP -> HTTPS 301 重定向监听。
pub async fn serve_web(app: Router, config: &AppConfig, device_name: &str) {
    // 纯 HTTP 模式
    if !config.enable_https {
        let listener = tokio::net::TcpListener::bind(config.http_addr).await.unwrap();
        push_log(
            "INFO",
            "System",
            &format!(
                "{} WebUI 服务已在 http://{} 监听 (纯 HTTP 模式)",
                device_name, config.http_addr
            ),
        );
        axum::serve(listener, app).await.unwrap();
        return;
    }

    let tls_config = match load_or_generate_tls_config().await {
        Ok(cfg) => cfg,
        Err(e) => {
            push_log(
                "ERROR",
                "TLS",
                &format!("TLS 初始化失败 ({})，回退到纯 HTTP 模式", e),
            );
            let listener = tokio::net::TcpListener::bind(config.http_addr).await.unwrap();
            push_log(
                "INFO",
                "System",
                &format!("{} WebUI 服务已在 http://{} 监听", device_name, config.http_addr),
            );
            axum::serve(listener, app).await.unwrap();
            return;
        }
    };

    // 开启 HTTP -> HTTPS 重定向，且两端口不冲突时后台启动重定向监听
    if config.enable_http_redirect && config.http_port != config.https_port {
        let (http_addr, http_port, https_port) =
            (config.http_addr, config.http_port, config.https_port);
        let redirect_app = create_http_redirect_app(https_port);
        tokio::spawn(async move {
            match tokio::net::TcpListener::bind(http_addr).await {
                Ok(listener) => {
                    push_log(
                        "INFO",
                        "System",
                        &format!(
                            "🔀 HTTP 自动重定向服务已在 http://{} 监听 (-> https://...:{})",
                            http_addr, https_port
                        ),
                    );
                    let _ = axum::serve(listener, redirect_app).await;
                }
                Err(_) => {
                    push_log(
                        "WARN",
                        "System",
                        &format!("HTTP 重定向端口 {} 绑定失败，跳过重定向服务", http_port),
                    );
                }
            }
        });
    }

    push_log(
        "INFO",
        "System",
        &format!(
            "🔒 {} WebUI HTTPS 服务已在 https://{} 启动",
            device_name, config.https_addr
        ),
    );
    axum_server::bind_rustls(config.https_addr, tls_config)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .unwrap();
}
