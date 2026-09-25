//! 质询-响应认证 Handlers。

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::{
    Claims, generate_stateless_nonce, hash_password_sha256, is_authenticated, jwt_encode,
    verify_stateless_nonce,
};
use crate::logger::push_log;
use crate::web::{assets::embedded_file, state::AppState};

pub struct AppError(pub anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "success": false,
            "error": self.0.to_string(),
        }));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[derive(Deserialize)]
pub struct LoginPayload {
    #[allow(dead_code)]
    pub username: String,
    pub nonce: String,
    pub response: String,
}

#[derive(Serialize)]
pub struct NonceResponse {
    pub nonce: String,
}

/// 从请求头中提取客户端 IP
pub fn client_ip(headers: &HeaderMap) -> String {
    if let Some(val) = headers.get("x-forwarded-for") {
        if let Ok(val) = val.to_str() {
            if let Some(ip) = val.split(',').next() {
                return ip.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

/// 获取一次性动态随机数（Nonce）：GET /api/get_nonce
pub async fn get_nonce_handler(State(state): State<Arc<AppState>>) -> Json<NonceResponse> {
    let nonce = generate_stateless_nonce(&state.jwt_secret);
    Json(NonceResponse { nonce })
}

pub async fn index_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !is_authenticated(&headers, &state.jwt_secret) {
        return Ok(Redirect::to("/login").into_response());
    }
    embedded_file("index.html").ok_or_else(|| AppError(anyhow::anyhow!("嵌入资源缺失: index.html")))
}

pub async fn login_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if is_authenticated(&headers, &state.jwt_secret) {
        return Ok(Redirect::to("/").into_response());
    }
    embedded_file("login.html").ok_or_else(|| AppError(anyhow::anyhow!("嵌入资源缺失: login.html")))
}

pub async fn login_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<LoginPayload>,
) -> Response {
    let ip = client_ip(&headers);

    // 无状态校验 Nonce（无需锁，无需 HashMap 查找）
    if !verify_stateless_nonce(&payload.nonce, &state.jwt_secret) {
        push_log(
            "WARN",
            "Auth",
            &format!(
                "登录失败(随机数无效或过期): 用户 '{}' 从 {}",
                payload.username, ip
            ),
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"success": false, "msg": "随机数失效，请刷新页面重试"})),
        )
            .into_response();
    }

    // SHA256(nonce + username + SHA256(admin_password))
    let expected_response = hash_password_sha256(&format!(
        "{}{}{}",
        payload.nonce, state.admin_username, state.admin_pass_sha
    ));

    if payload.response.eq_ignore_ascii_case(&expected_response) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let claims = Claims {
            sub: "admin".to_string(),
            exp: now + 86400 * 7,
            iat: now,
        };

        let token = match jwt_encode(&claims, &state.jwt_secret) {
            Ok(t) => t,
            Err(e) => {
                push_log("ERROR", "Auth", &format!("JWT 签名生成失败: {}", e));
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"success": false, "msg": "服务器内部错误，登录失败"})),
                )
                    .into_response();
            }
        };

        push_log("INFO", "Auth", &format!("管理员登录成功 (来源: {})", ip));

        let cookie = format!(
            "auth_token={}; HttpOnly; Secure; Path=/; SameSite=Lax; Max-Age=604800",
            token
        );

        Response::builder()
            .status(StatusCode::OK)
            .header(header::SET_COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                r#"{"success":true,"msg":"登录成功"}"#,
            ))
            .unwrap()
    } else {
        push_log(
            "WARN",
            "Auth",
            &format!(
                "登录失败(密码错误): 用户 '{}' 从 {} 尝试",
                payload.username, ip
            ),
        );
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"success": false, "msg": "密码错误，请重试"})),
        )
            .into_response()
    }
}

pub async fn logout_post_handler() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::SET_COOKIE,
            "auth_token=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0",
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(r#"{"success":true}"#))
        .unwrap()
}
