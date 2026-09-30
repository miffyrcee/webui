//! 前端静态资源：dist/ 由 Vite 产出**单文件** HTML（JS/CSS 已全部内联），
//! 这里用 `include_str!` 在编译期把两个入口直接嵌进二进制，运行时零文件系统依赖。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use std::sync::Arc;

use crate::auth::is_authenticated;
use crate::web::state::AppState;

/// Vite 构建产物（单文件，自包含）。路径相对于本文件，指向仓库根的 dist/。
pub const INDEX_HTML: &str = include_str!("../../dist/index.html");
pub const LOGIN_HTML: &str = include_str!("../../dist/login.html");

/// 兜底静态资源处理器：只有两个入口页面，其余路径一律 404。
///
/// 单文件化之后不再有 `/assets/*` 外链资源，故无需再处理任何子路径，
/// 也就不会出现"切片路由被删导致外链 404 白屏"的情况。
pub async fn static_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');

    match path {
        // 与 `/` 同等保护：未认证直接跳登录页
        "" | "index.html" => {
            if !is_authenticated(&headers, &state.jwt_secret) {
                Redirect::to("/login").into_response()
            } else {
                Html(INDEX_HTML).into_response()
            }
        }
        "login" | "login.html" => {
            if is_authenticated(&headers, &state.jwt_secret) {
                Redirect::to("/").into_response()
            } else {
                Html(LOGIN_HTML).into_response()
            }
        }
        _ => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}
