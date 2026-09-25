use std::env;
use std::net::SocketAddr;

use sha2::{Digest, Sha256};

use crate::auth::hash_password_sha256;
use crate::logger::push_log;

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";

/// 环境变量统一加载后的运行期配置。
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub serial_path: String,
    pub jwt_secret: String,
    pub admin_username: String,
    pub admin_pass_sha: String,
    pub enable_https: bool,
    pub enable_http_redirect: bool,
    pub http_port: u16,
    pub https_port: u16,
    pub http_addr: SocketAddr,
    pub https_addr: SocketAddr,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let serial_path =
            env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
        push_log(
            "INFO",
            "System",
            &format!("正在初始化串口设备: {}", serial_path),
        );

        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let mut hasher = Sha256::new();
            hasher.update(format!("jwt_rand_{}_{}", nanos, pid));
            let key = hex::encode(hasher.finalize());
            push_log(
                "WARN",
                "Auth",
                "未检测到 JWT_SECRET 环境变量，已在内存中生成随机密钥（零文件落盘）",
            );
            key
        });

        let admin_password = env::var("WEBUI_PASSWORD").unwrap_or_else(|_| "admin123".to_string());
        let admin_username = env::var("WEBUI_USERNAME").unwrap_or_else(|_| "admin".to_string());
        let admin_pass_sha = hash_password_sha256(&admin_password);

        push_log(
            "INFO",
            "System",
            "密码哈希算法: SHA256 (质询-响应)",
        );

        let enable_https = env::var("ENABLE_HTTPS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);

        let enable_http_redirect = env::var("HTTP_REDIRECT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);

        let http_port: u16 = env::var("HTTP_PORT")
            .or_else(|_| env::var("PORT"))
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(80);

        let https_port: u16 = env::var("HTTPS_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(443);

        let http_addr: SocketAddr = format!("0.0.0.0:{}", http_port).parse().unwrap();
        let https_addr: SocketAddr = format!("0.0.0.0:{}", https_port).parse().unwrap();

        Self {
            serial_path,
            jwt_secret,
            admin_username,
            admin_pass_sha,
            enable_https,
            enable_http_redirect,
            http_port,
            https_port,
            http_addr,
            https_addr,
        }
    }
}
