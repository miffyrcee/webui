use askama::Template; // 恢复使用原生 Askama 宏
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, sleep};

// 声明 Askama 视图模型。绑定的字段名称必须与 HTML 模板中的 {{ 变量 }} 一一对应[cite: 1]
#[derive(Template)]
#[template(path = "dist_index.html")] // 自动寻址项目根目录下的 templates/dist_index.html
struct IndexTemplate {
    firmware_version: &'static str,
}

// 为 Askama 模板实现 Axum 的 IntoResponse 接口，确保 handler 可以直接返回模板实例
impl IntoResponse for IndexTemplate {
    fn into_response(self) -> Response {
        match self.render() {
            Ok(html) => axum::response::Html(html).into_response(),
            Err(err) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Template error: {}", err),
            )
                .into_response(),
        }
    }
}

// 共享的底层上下文状态
struct AppState {
    tx: broadcast::Sender<String>,    // 状态广播管道
    active_clients: Arc<AtomicUsize>, // 实时在线的 WS 客户端计数器
    serial_lock: Arc<Mutex<()>>,      // 串行总线排他同步锁
}

#[derive(Serialize, Deserialize, Debug)]
struct WsCommand {
    action: String,
    payload: Option<String>,
}

#[tokio::main]
async fn main() {
    // 1. 初始化并发通信及状态总线
    let (tx, _rx) = broadcast::channel(100);
    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        active_clients: Arc::new(AtomicUsize::new(0)),
        serial_lock: Arc::new(Mutex::new(())), // 确保同一时刻只有一个 AT 质询占用物理串口
    });

    // 2. 启动基带模组硬件轮询的后台微型守护进程 (Actor)
    let state_clone = app_state.clone();
    tokio::spawn(async move {
        hardware_polling_actor(state_clone).await;
    });

    // 3. 构建高并发路由生态
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .with_state(app_state);

    // 4. 绑定端口，启动物理服务
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("⚡ RG520N-EU 工业级单文件微服务已就绪: http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}

// 首页处理器：直接实例化模板。渲染工作完全由 Askama 在 Rust 内部以内存指针计算形式极速完成
async fn index_handler() -> impl IntoResponse {
    IndexTemplate {
        firmware_version: "RM520NGLAAR03A03M4G_BETA",
    }
}

// 硬件状态无状态轮询逻辑
async fn hardware_polling_actor(state: Arc<AppState>) {
    loop {
        // 自适应边界控制：若没有任何前端网页在线，则立即挂起物理串口质询，归零硬件开销
        if state.active_clients.load(Ordering::Relaxed) == 0 {
            sleep(Duration::from_millis(1500)).await;
            continue;
        }

        // 竞争串行独占锁
        let _lock = state.serial_lock.lock().await;

        // 模拟向移远模组发送 AT+QENG="servingcell" 等指令后解析出的遥测数据[cite: 1]
        let mock_telemetry = serde_json::json!({
            "type": "realtime",
            "cpu": 6.5,
            "temperature": 39.5,
            "signal": "-74dBm",
            "band": "N78"
        });

        let _ = state.tx.send(mock_telemetry.to_string());
        drop(_lock); // 释放锁，允许手动 AT 指令插队竞争

        sleep(Duration::from_millis(2000)).await; // 保持 2 秒一周期的高频刷新
    }
}

// WebSocket 握手路由升级
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

// 核心全双工 WebSocket 通信处理
async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    state.active_clients.fetch_add(1, Ordering::Relaxed); // 客户端注册成功，原子加 1
    let mut rx = state.tx.subscribe();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // 任务 A：异步广播。将 Actor 搜集到的实时遥测状态泵向前端
    let mut send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                // 处理广播延迟错误，防止单客户端卡顿导致整个发送任务中断
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 任务 B：异步接收。监听前端下发的手动 AT 或锁频配置
    let state_clone = state.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(msg) = ws_receiver.next().await {
            if let Ok(msg) = msg {
                match msg {
                    Message::Text(text) => {
                        if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                            // 执行敏感串口写入时，强制竞争串行独占锁
                            let _lock = state_clone.serial_lock.lock().await;
                            match cmd.action.as_str() {
                                "manual_at" => {
                                    println!("物理串口接收到手动 AT 穿透指令: {:?}", cmd.payload);
                                }
                                "lock_band" => {
                                    println!("物理串口接收到核心频段重署策略: {:?}", cmd.payload);
                                }
                                _ => {}
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            } else {
                break;
            }
        }
    });

    // 熔断保护：任何一方因断开连接退出，双端任务立刻协同销毁
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
    state.active_clients.fetch_sub(1, Ordering::Relaxed); // 客户端退出，原子减 1
}
