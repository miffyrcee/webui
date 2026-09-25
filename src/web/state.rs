//! 共享应用状态与活跃视图 RAII Guard。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::{broadcast, mpsc, watch};

use crate::actor::action::AtRequest;
use crate::actor::telemetry::GlobalTelemetry;
use crate::config::AppConfig;

/// 安全递减活跃视图计数器（防止下溢翻转为 usize::MAX）
pub fn safe_dec_active_views(atomic: &AtomicUsize) {
    let _ = atomic.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |val| {
        Some(val.saturating_sub(1))
    });
}

/// RAII Guard：保证 handle_ws 退出时自动递减 active_views，防止 Panic/Cancel 计数泄露
pub struct ActiveViewGuard {
    pub state: Arc<AppState>,
    pub is_active: Arc<AtomicBool>,
}

impl ActiveViewGuard {
    /// 新建 Guard：active_views +1，返回 Guard 和共享的活跃标志
    pub fn new(state: Arc<AppState>) -> (Self, Arc<AtomicBool>) {
        state.active_views.fetch_add(1, Ordering::SeqCst);
        let is_active = Arc::new(AtomicBool::new(true));
        (
            Self {
                state: state.clone(),
                is_active: is_active.clone(),
            },
            is_active,
        )
    }
}

impl Drop for ActiveViewGuard {
    fn drop(&mut self) {
        if self.is_active.load(Ordering::SeqCst) {
            safe_dec_active_views(&self.state.active_views);
        }
    }
}

pub struct AppState {
    /// 广播预序列化的 JSON 文本 Arc，避免每个客户端重复序列化
    pub tx: broadcast::Sender<Arc<String>>,
    /// 扁平、高优先级的单一串行指令通道
    pub command_tx: mpsc::Sender<AtRequest>,
    pub active_views: AtomicUsize,
    /// 全局遥测 watch 接收端 — 读取为 0 锁
    pub telemetry_rx: watch::Receiver<Arc<GlobalTelemetry>>,
    /// JWT 签名密钥
    pub jwt_secret: String,
    /// 管理员密码的 SHA256 哈希（用于质询-响应协议）
    pub admin_pass_sha: String,
    /// 管理员用户名
    pub admin_username: String,
}

impl AppState {
    pub fn new(
        config: &AppConfig,
    ) -> (
        Arc<Self>,
        mpsc::Receiver<AtRequest>,
        watch::Sender<Arc<GlobalTelemetry>>,
    ) {
        let (tx, _) = broadcast::channel(100);
        let (command_tx, command_rx) = mpsc::channel(32);
        let (telemetry_tx, telemetry_rx) = watch::channel(Arc::new(GlobalTelemetry::default()));

        let state = Arc::new(Self {
            tx,
            command_tx,
            active_views: AtomicUsize::new(0),
            telemetry_rx,
            jwt_secret: config.jwt_secret.clone(),
            admin_pass_sha: config.admin_pass_sha.clone(),
            admin_username: config.admin_username.clone(),
        });

        (state, command_rx, telemetry_tx)
    }
}
