//! 安全鉴权：HS256 JWT 编解码、无状态 HMAC Nonce 与密码 SHA256 哈希（纯 Rust，零 C 依赖）。

use axum::http::{HeaderMap, header};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: u64,
    pub iat: u64,
}

/// 从 Cookie 请求头解析 JWT Token
pub fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|cookie| {
            let mut parts = cookie.trim().splitn(2, '=');
            let name = parts.next()?;
            let value = parts.next()?;
            if name == "auth_token" {
                Some(value.to_string())
            } else {
                None
            }
        })
}

/// HS256 JWT 编码（纯 Rust，无 C 依赖）
pub fn jwt_encode(claims: &Claims, secret: &str) -> Result<String, String> {
    let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
    let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header.to_string());
    let payload = serde_json::to_string(claims).map_err(|e| e.to_string())?;
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{}.{}", header_b64, payload_b64);

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    let sig_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    Ok(format!("{}.{}", signing_input, sig_b64))
}

/// HS256 JWT 解码与验证（纯 Rust，无 C 依赖）
pub fn jwt_decode(token: &str, secret: &str) -> Result<Claims, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("无效的 JWT 令牌格式".to_string());
    }

    let signing_input = format!("{}.{}", parts[0], parts[1]);

    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| "JWT 签名验证失败".to_string())?;

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let claims: Claims = serde_json::from_slice(&payload_bytes).map_err(|e| e.to_string())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if claims.exp < now {
        return Err("JWT 令牌已过期".to_string());
    }

    Ok(claims)
}

/// 验证 JWT Token 是否有效
pub fn is_authenticated(headers: &HeaderMap, jwt_secret: &str) -> bool {
    if let Some(token) = extract_token_from_headers(headers) {
        jwt_decode(&token, jwt_secret).is_ok()
    } else {
        false
    }
}

/// 对密码进行 SHA256 哈希（用于质询-响应协议）
pub fn hash_password_sha256(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}

/// 生成无状态 HMAC Nonce（零内存开销）
pub fn generate_stateless_nonce(secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(now.to_string().as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    format!("{}.{}", now, sig)
}

/// 校验无状态 HMAC Nonce（签名校验 + 60秒过期，允许 5 秒向前时钟漂移容差）
pub fn verify_stateless_nonce(nonce: &str, secret: &str) -> bool {
    let parts: Vec<&str> = nonce.split('.').collect();
    if parts.len() != 2 {
        return false;
    }
    let ts: u64 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // 容忍客户端时钟快 5 秒（向前漂移），最大有效期 60 秒
    if ts > now + 5 || now.saturating_sub(ts) > 60 {
        return false;
    }
    let sig_bytes = match hex::decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(parts[0].as_bytes());
    mac.verify_slice(&sig_bytes).is_ok()
}
