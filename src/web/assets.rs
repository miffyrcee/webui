//! 前端静态资源：dist/ 由 Vite 构建产出，编译期整体嵌入二进制。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use std::sync::Arc;

use crate::auth::is_authenticated;
use crate::web::state::AppState;

/// `dist/` 下的全部产物（index.html / login.html / assets/*）
#[derive(rust_embed::RustEmbed)]
#[folder = "dist/"]
pub struct Asset;

/// 读取嵌入资源并补齐 Content-Type 与缓存策略；未命中返回 None
pub fn embedded_file(path: &str) -> Option<Response<axum::body::Body>> {
    let file = Asset::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    // Vite 产物自带内容哈希，可长期强缓存；HTML 入口保持协商缓存以跟随版本更新
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache_control)
        .body(axum::body::Body::from(file.data.into_owned()))
        .ok()
}

/// 兜底静态资源处理器：命中嵌入资源则返回，否则 404
pub async fn static_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // index.html 是唯一可被直接访问的 HTML 入口，必须与 `/` 享有同样的认证保护
    if path == "index.html" && !is_authenticated(&headers, &state.jwt_secret) {
        return Redirect::to("/login").into_response();
    }

    match embedded_file(path) {
        Some(response) => response,
        None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}
