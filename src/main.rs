use askama::Template; // 恢复使用原生 Askama 宏
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use sysinfo::{Components, System};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::time::{Duration, sleep};

// 内部 Actor 通信协议：包含指令内容及用于回传结果的 oneshot 管道
struct AtRequest {
    action: String,
    payload: Option<String>,
    resp_tx: oneshot::Sender<String>,
}

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
    tx: broadcast::Sender<String>,     // 遥测数据广播管道
    active_clients: Arc<AtomicUsize>,  // 实时在线的 WS 客户端计数器
    serial_lock: Arc<Mutex<()>>,       // 串行总线排他同步锁
    actor_tx: mpsc::Sender<AtRequest>, // 发送给硬件 Actor 的指令通道
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
    let (actor_tx, actor_rx) = mpsc::channel(32);

    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        active_clients: Arc::new(AtomicUsize::new(0)),
        serial_lock: Arc::new(Mutex::new(())), // 确保同一时刻只有一个 AT 质询占用物理串口
        actor_tx,
    });

    // 2. 启动硬件交互 Actor (处理定时采样与 AT 质询)
    tokio::spawn({
        let state = app_state.clone();
        async move { hardware_polling_actor(state, actor_rx).await }
    });

    // 3. 构建高并发路由生态
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/style.css", get(style_handler))
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
async fn hardware_polling_actor(state: Arc<AppState>, mut actor_rx: mpsc::Receiver<AtRequest>) {
    let mut sys = System::new_all();
    let mut interval = tokio::time::interval(Duration::from_millis(2000));

    loop {
        tokio::select! {
            // 场景 A：处理来自 WebSocket 的即时 AT 质询请求
            Some(req) = actor_rx.recv() => {
                let _lock = state.serial_lock.lock().await;
                // 模拟物理串口写入与读取过程
                let response = match req.action.as_str() {
                    "manual_at" => format!("AT+OK: Received {}", req.payload.unwrap_or_default()),
                    "lock_band" => "BAND_LOCK_SUCCESS".to_string(),
                    _ => "UNKNOWN_ACTION".to_string(),
                };
                let _ = req.resp_tx.send(response);
            }

            // 场景 B：定时刷新硬件状态 (仅在有客户端在线时执行)
            _ = interval.tick() => {
                if state.active_clients.load(Ordering::Relaxed) > 0 {
                    let _lock = state.serial_lock.lock().await;

                    // 获取真实系统数据
                    sys.refresh_cpu_usage();
                    let cpu_usage = sys.global_cpu_usage(); // sysinfo 0.30+ 版本的正确方法

                    let components = Components::new_with_refreshed_list();
                    let temp = components.iter()
                        .find(|c| c.label().to_uppercase().contains("CPU") || c.label().to_uppercase().contains("PACKAGE"))
                        .map(|c| c.temperature())
                        .unwrap_or(Some(0.0));

                    let telemetry = serde_json::json!({
                        "type": "realtime",
                        "cpu": format!("{:.1}%", cpu_usage),
                        "temperature": temp.map_or("N/A".to_string(), |t| format!("{:.1}°C", t)),
                        "signal": "-74dBm",
                        "band": "N78"
                    });

                    let _ = state.tx.send(telemetry.to_string());
                }
            }
        }
    }
}

// WebSocket 握手路由升级
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

// 核心全双工 WebSocket 通信处理
async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    state.active_clients.fetch_add(1, Ordering::Relaxed); // 客户端注册成功，原子加 1

    let mut broadcast_rx = state.tx.subscribe();
    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // 任务 A：发送端。融合广播流与 oneshot 响应流
    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // 接收全局硬件广播
                Ok(msg) = broadcast_rx.recv() => {
                    if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                // 接收针对当前客户端请求的专属回复
                Some(reply) = local_rx.recv() => {
                    if ws_sender.send(Message::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // 任务 B：异步接收。监听前端下发的手动 AT 或锁频配置
    let mut recv_task = tokio::spawn({
        let state = state.clone();
        async move {
            while let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
                if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                    let (resp_tx, resp_rx) = oneshot::channel();

                    // 将请求投递给 Actor
                    let _ = state
                        .actor_tx
                        .send(AtRequest {
                            action: cmd.action,
                            payload: cmd.payload,
                            resp_tx,
                        })
                        .await;

                    // 等待 oneshot 质询结果并回传给发送任务
                    if let Ok(reply) = resp_rx.await {
                        let result_json = serde_json::json!({ "type": "at_res", "data": reply });
                        let _ = local_tx.send(result_json.to_string()).await;
                    }
                }
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

async fn style_handler() -> impl IntoResponse {
    // 在编译期直接把 build.rs 生成的 style.css 加载进内存静态区
    let css_content = include_str!("../templates/style.css");

    // 构建 HTTP 响应，并强制给浏览器打上 text/css 的 Content-Type 标签
    // 否则现代浏览器会因为 MIME 类型不匹配而拒绝解析样式
    Response::builder()
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .body(axum::body::Body::from(css_content))
        .unwrap()
}
