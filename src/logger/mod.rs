//! 模组闪存生命保护：内存环形滚动日志系统 (RAM Ring Buffer)。
//! 使用 mpsc 通道异步写入 + 查询，消除 Mutex 竞争。

use std::collections::VecDeque;
use std::sync::OnceLock;

use tokio::sync::{mpsc, oneshot};

pub enum LogCommand {
    Push(String),
    GetSnapshot(oneshot::Sender<Vec<String>>),
}

static LOG_TX: OnceLock<mpsc::Sender<LogCommand>> = OnceLock::new();

/// 初始化日志后台 Worker，返回 receiver 用于异步消费
pub fn init_log_worker() -> mpsc::Receiver<LogCommand> {
    let (tx, rx) = mpsc::channel(1024);
    LOG_TX.set(tx).ok();
    rx
}

/// 统一日志打印接口：内存存储 + 标准输出（不伤 Flash 寿命）
pub fn push_log(level: &str, module: &str, msg: &str) {
    let now = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
    let formatted = format!("[{}] [{}] [{}]: {}", now, level, module, msg);

    if level == "ERROR" {
        eprintln!("{}", formatted);
    } else {
        println!("{}", formatted);
    }

    if let Some(tx) = LOG_TX.get() {
        let _ = tx.try_send(LogCommand::Push(formatted)); // 非阻塞发送，满则丢弃（背压保护）
    }
}

/// 独立协程管理环形日志（零锁，自有 VecDeque）
pub async fn log_worker_task(mut rx: mpsc::Receiver<LogCommand>) {
    let mut logs: VecDeque<String> = VecDeque::with_capacity(100);
    while let Some(cmd) = rx.recv().await {
        match cmd {
            LogCommand::Push(msg) => {
                if logs.len() >= 100 {
                    logs.pop_front();
                }
                logs.push_back(msg);
            }
            LogCommand::GetSnapshot(tx) => {
                let _ = tx.send(logs.iter().cloned().collect());
            }
        }
    }
}

/// 获取日志快照（通过 oneshot 通道查询 log_worker_task，零锁）
pub async fn get_logs_snapshot() -> Vec<String> {
    let tx = LOG_TX.get().expect("log worker not initialized");
    let (resp_tx, resp_rx) = oneshot::channel();
    if tx.send(LogCommand::GetSnapshot(resp_tx)).await.is_ok() {
        resp_rx.await.unwrap_or_default()
    } else {
        Vec::new()
    }
}
