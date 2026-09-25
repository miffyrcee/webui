//! HTTP & WebSocket Web 服务层。

pub mod assets;
pub mod handlers;
pub mod serve;
pub mod state;
pub mod ws;

use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;

use self::state::AppState;

pub fn build_router(app_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(handlers::index_handler))
        .route("/login", get(handlers::login_get_handler))
        .route("/api/get_nonce", get(handlers::get_nonce_handler))
        .route("/api/login", post(handlers::login_post_handler))
        .route("/api/logout", post(handlers::logout_post_handler))
        .route("/ws", get(ws::ws_handler))
        .fallback(assets::static_handler)
        .with_state(app_state)
}
