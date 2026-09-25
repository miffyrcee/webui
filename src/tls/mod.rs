//! 安全传输层：优先加载外部证书，缺失时用 certkit 纯内存签发 ECC P-256 自签名证书。

use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use axum_server::tls_rustls::RustlsConfig;
use std::env;
use std::path::Path;

use crate::logger::push_log;

/// 加载 TLS 证书配置：
/// 1. 优先读取 SSL_CERT_PATH / SSL_KEY_PATH 指定的文件；
/// 2. 未找到文件时，用 certkit 纯内存签发 ECC P-256 自签名证书（零 C 依赖，开箱即用）。
pub async fn load_or_generate_tls_config() -> Result<RustlsConfig, Box<dyn std::error::Error>> {
    let cert_path = env::var("SSL_CERT_PATH").unwrap_or_else(|_| "cert.pem".to_string());
    let key_path = env::var("SSL_KEY_PATH").unwrap_or_else(|_| "key.pem".to_string());

    if Path::new(&cert_path).exists() && Path::new(&key_path).exists() {
        push_log(
            "INFO",
            "TLS",
            &format!("从文件加载 SSL 证书: {} / {}", cert_path, key_path),
        );
        return Ok(RustlsConfig::from_pem_file(cert_path, key_path).await?);
    }

    push_log(
        "WARN",
        "TLS",
        "未检测到外部 SSL 证书，正在通过 RustCrypto 内存动态签发证书 (开箱即用)...",
    );

    // 纯 Rust 生成 ECC P-256 密钥对
    let key_pair = certkit::key::KeyPair::generate_ecdsa_p256();

    // SAN 覆盖常见网关 IP 与 localhost，避免浏览器提示主机名不匹配
    let san = certkit::cert::extensions::SubjectAltName {
        dns_names: vec!["localhost".to_string()],
        ip_addresses: vec![
            "127.0.0.1".parse().unwrap(),
            "192.168.1.1".parse().unwrap(),
            "192.168.0.1".parse().unwrap(),
            "192.168.8.1".parse().unwrap(),
            "192.168.100.1".parse().unwrap(),
        ],
        email_addresses: Vec::new(),
    };

    let subject = certkit::cert::params::DistinguishedName::builder()
        .common_name("RM520N 5G WebUI".to_string())
        .organization("Argon Cellular".to_string())
        .build();

    let cert_info = certkit::cert::params::CertificateParams::builder()
        .subject(subject)
        .subject_public_key(certkit::key::PublicKey::from_key_pair(&key_pair))
        .extensions(vec![certkit::cert::params::ExtensionParam::from_extension(
            san, false,
        )?])
        .build();

    let cert = certkit::cert::Certificate::new_self_signed(&cert_info, &key_pair)?;

    let cert_pem = cert.to_pem()?;
    let key_pem = key_pair.encode_private_key_pem()?;

    let config = RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await?;
    push_log("INFO", "TLS", "内存自签名证书已就绪 (ECC P-256, 零 C 依赖)");
    Ok(config)
}

/// 创建 HTTP -> HTTPS 自动重定向 Router（Axum 0.8 兼容）
pub fn create_http_redirect_app(https_port: u16) -> Router {
    Router::new().fallback(move |headers: HeaderMap, uri: axum::http::Uri| async move {
        let host_header = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");

        // 剥离请求头里的旧端口号
        let hostname = host_header.split(':').next().unwrap_or(host_header);

        let target_url = if https_port == 443 {
            format!("https://{}{}", hostname, uri)
        } else {
            format!("https://{}:{}{}", hostname, https_port, uri)
        };

        // axum 的 Redirect::permanent 在 0.8 中为 308（保留请求方法），
        // 此处按网关惯例显式返回 301 Moved Permanently
        Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(header::LOCATION, target_url)
            .body(axum::body::Body::empty())
            .unwrap()
    })
}
