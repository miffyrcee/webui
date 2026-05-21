use askama::Template;
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
use std::env;
use std::sync::Arc;
use sysinfo::{Components, System};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::{Duration, sleep};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

// 从环境变量读取串口路径，默认 /dev/at_mdm0
const DEFAULT_SERIAL_PORT: &str = "/dev/at_mdm0";

struct AtRequest {
    action: String,
    payload: Option<String>,
    resp_tx: oneshot::Sender<String>,
}

#[derive(Template)]
#[template(path = "dist_index.html")]
struct IndexTemplate {
    firmware_version: &'static str,
}

impl IntoResponse for IndexTemplate {
    fn into_response(self) -> Response {
        match self.render() {
            Ok(html) => axum::response::Html(html).into_response(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Template error: {}", err),
            )
                .into_response(),
        }
    }
}

struct AppState {
    tx: broadcast::Sender<String>,
    serial_port: Arc<Mutex<Option<SerialStream>>>, // 真正的异步串口
    actor_tx: mpsc::Sender<AtRequest>,
}

#[derive(Serialize, Deserialize, Debug)]
struct WsCommand {
    action: String,
    payload: Option<String>,
}

// 统一遥测数据模型
#[derive(Serialize, Default, Debug)]
struct TelemetryData {
    temperature: String,
    sim_status: String,
    signal_percentage: String,
    internet_connection: String,

    // 网络信息
    active_sim: String,
    network_provider: String,
    mccmnc: String,
    apn: String,
    network_mode: String,
    bands: String,
    bandwidth: String,
    earfcn: String,
    pci: String,
    ipv4: String,
    ipv6: String,
    uptime: String,

    // 信号信息
    assessment: String,
    traffic_stats: String,
    cell_id: String,
    enb_id: String,
    tac: String,
    ss_rsrq: String,
    ss_rsrp: String,
    sinr: String,
    updated: String,
}

#[tokio::main]
async fn main() {
    // 读取串口路径（环境变量或默认值）
    let serial_path = env::var("AT_SERIAL_PORT")
        .unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
    println!("🔌 使用串口设备: {}", serial_path);

    let (tx, _) = broadcast::channel(100);
    let (actor_tx, actor_rx) = mpsc::channel(32);

    // 打开串口（如果失败则进入模拟模式）
    let serial_stream = open_serial_port(&serial_path).await;
    if serial_stream.is_none() {
        eprintln!("⚠️ 串口打开失败，系统将以模拟模式运行（无实际硬件通信）");
    }

    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        serial_port: Arc::new(Mutex::new(serial_stream)),
        actor_tx,
    });

    tokio::spawn({
        let state = app_state.clone();
        async move { hardware_polling_actor(state, actor_rx).await }
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/style.css", get(style_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("⚡ 移远 5G 终端 WebUI 服务端已就绪: http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn index_handler() -> impl IntoResponse {
    IndexTemplate {
        firmware_version: "RM520NGLAAR03A03M4G_BETA",
    }
}

/// 打开并配置串口（115200 8N1）
async fn open_serial_port(path: &str) -> Option<SerialStream> {
    match tokio_serial::new(path, 115200)
        .data_bits(tokio_serial::DataBits::Eight)
        .stop_bits(tokio_serial::StopBits::One)
        .parity(tokio_serial::Parity::None)
        .flow_control(tokio_serial::FlowControl::None)
        .open_native_async()
    {
        Ok(port) => {
            println!("✅ 成功打开串口: {}", path);
            Some(port)
        }
        Err(e) => {
            eprintln!("❌ 打开串口 {} 失败: {}", path, e);
            None
        }
    }
}

/// 发送 AT 命令并读取响应（带超时）
async fn send_at_command_raw(
    port: &mut SerialStream,
    cmd: &str,
    timeout_ms: u64,
) -> Result<String, String> {
    let full_cmd = format!("{}\r\n", cmd);
    println!("📤 [AT] 发送: {}", cmd);

    port.write_all(full_cmd.as_bytes())
        .await
        .map_err(|e| format!("写失败: {}", e))?;
    port.flush().await.map_err(|e| format!("flush失败: {}", e))?;

    let mut buf = vec![0u8; 1024];
    let mut response = String::new();

    let read_future = async {
        loop {
            match port.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    response.push_str(&chunk);
                    if response.contains("\r\nOK\r\n") || response.contains("\r\nERROR\r\n") {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("读错误: {}", e);
                    break;
                }
            }
        }
        response
    };

    match tokio::time::timeout(Duration::from_millis(timeout_ms), read_future).await {
        Ok(resp) => {
            if resp.contains("ERROR") {
                println!("⚠️ [AT] {} 返回 ERROR", cmd);
            } else {
                println!("✅ [AT] {} 成功:\n{}", cmd, resp);
            }
            Ok(resp)
        }
        Err(_) => {
            println!("❌ [AT] {} 超时 ({}ms)", cmd, timeout_ms);
            Err("Timeout".to_string())
        }
    }
}

/// 执行 AT 命令（根据串口是否存在选择真实通信或模拟）
async fn execute_at_command(
    port_opt: &mut Option<SerialStream>,
    cmd: &str,
    timeout_ms: u64,
) -> String {
    if let Some(port) = port_opt {
        match send_at_command_raw(port, cmd, timeout_ms).await {
            Ok(resp) => resp,
            Err(e) => format!("ERROR: {}", e),
        }
    } else {
        // 模拟模式：返回预定义数据
        sleep(Duration::from_millis(50)).await;
        match cmd {
            "AT+CPIN?" => "+CPIN: READY\r\n\r\nOK\r\n".to_string(),
            "AT+QENG=\"servingcell\"" => {
                "+QENG: \"servingcell\",\"NOCONN\",\"5G-SA\",\"TDD\",460,00,39074C001,59798720,7471151,41,100,-62,-11,22\r\n\r\nOK\r\n".to_string()
            }
            "AT+CGPADDR" => {
                "+CGPADDR: 1,\"10.15.13.9\",\"2409:8970:a05:c21:bc94:7cd8:1735:604f\"\r\n\r\nOK\r\n".to_string()
            }
            _ => "OK\r\n".to_string(),
        }
    }
}

async fn hardware_polling_actor(state: Arc<AppState>, mut actor_rx: mpsc::Receiver<AtRequest>) {
    let mut sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();
    let mut interval = tokio::time::interval(Duration::from_millis(3000));

    loop {
        tokio::select! {
            Some(req) = actor_rx.recv() => {
                let mut serial_guard = state.serial_port.lock().await;
                let response = if req.action == "manual_at" {
                    let cmd = req.payload.unwrap_or_default();
                    println!("👨‍💻 用户手动执行 AT: {}", cmd);
                    execute_at_command(&mut *serial_guard, &cmd, 2000).await
                } else {
                    "OK".to_string()
                };
                let _ = req.resp_tx.send(response);
            }

            _ = interval.tick() => {
                let active_clients = state.tx.receiver_count();
                if active_clients > 0 {
                    let start_poll = std::time::Instant::now();
                    let mut serial_guard = state.serial_port.lock().await;
                    println!("📡 开始周期性硬件轮询 (当前在线客户端: {})", active_clients);

                    let cpin_res = execute_at_command(&mut *serial_guard, "AT+CPIN?", 500).await;
                    let qeng_res = execute_at_command(&mut *serial_guard, "AT+QENG=\"servingcell\"", 2000).await;
                    let gpad_res = execute_at_command(&mut *serial_guard, "AT+CGPADDR", 500).await;

                    let sim_status = if cpin_res.contains("READY") { "Active" } else { "No Card / Locked" };

                    let mut telemetry = TelemetryData::default();
                    telemetry.sim_status = sim_status.to_string();
                    telemetry.active_sim = "SIM 1".to_string();
                    telemetry.network_provider = "4E2D56FD79FB52A8".to_string();
                    telemetry.apn = "apn-here-inside-of-quotes".to_string();
                    telemetry.traffic_stats = "244 MB DL / 60 MB UL".to_string();

                    // 解析 +QENG 响应
                    if let Some(line) = qeng_res.lines().find(|l| l.contains("+QENG: \"servingcell\"")) {
                        let cleaned = line.replace("\"", "");
                        let parts: Vec<&str> = cleaned.split(',').map(|s| s.trim()).collect();
                        if parts.len() >= 14 {
                            telemetry.network_mode = format!("{} {}", parts[2], parts[3]);
                            telemetry.mccmnc = format!("{}{}", parts[4], parts[5]);
                            telemetry.cell_id = format!("Short 01(1), Long {}(15308472321)", parts[6]);
                            telemetry.enb_id = parts[7].to_string();
                            telemetry.tac = format!("{} (72002F)", parts[8]);
                            telemetry.bands = format!("NR5G BAND {}, NR5G BAND 28", parts[9]);
                            telemetry.bandwidth = format!("NR {} MHz", parts[10]);
                            telemetry.earfcn = "504990, 156490".to_string();
                            telemetry.pci = "751, 297".to_string();

                            let rsrp: i32 = parts[11].parse().unwrap_or(-140);
                            let rsrq: i32 = parts[12].parse().unwrap_or(-40);
                            let sinr: i32 = parts[13].parse().unwrap_or(0);

                            let rsrp_pct = ((rsrp + 140) as f32 / 100.0 * 100.0).clamp(0.0, 100.0) as i32;
                            let rsrq_pct = ((rsrq + 40) as f32 / 45.0 * 100.0).clamp(0.0, 100.0) as i32;
                            let sinr_pct = ((sinr + 20) as f32 / 55.0 * 100.0).clamp(0.0, 100.0) as i32;

                            telemetry.ss_rsrp = format!("{} / {}%", rsrp, rsrp_pct);
                            telemetry.ss_rsrq = format!("{} / {}%", rsrq, rsrq_pct);
                            telemetry.sinr = format!("{} / {}%", sinr, sinr_pct);
                            telemetry.signal_percentage = format!("{}%", rsrp_pct);
                            telemetry.assessment = if rsrp > -80 && sinr > 20 { "Excellent".to_string() } else { "Good".to_string() };
                        }
                    }

                    // 解析 +CGPADDR 获取 IP
                    for line in gpad_res.lines() {
                        if line.contains("+CGPADDR:") {
                            let parts: Vec<&str> = line.split(',').collect();
                            if parts.len() >= 2 {
                                telemetry.ipv4 = parts[1].replace("\"", "").trim().to_string();
                            }
                            if parts.len() >= 3 {
                                telemetry.ipv6 = parts[2].replace("\"", "").trim().to_string();
                            }
                        }
                    }

                    // 获取 CPU 温度（模拟或真实）
                    sys.refresh_cpu_all();
                    components.refresh(false);
                    let cpu_temp = components.iter()
                        .find(|c| c.label().to_uppercase().contains("CPU") || c.label().to_uppercase().contains("PACKAGE"))
                        .map_or(46.0, |c| c.temperature().unwrap_or(46.0));

                    telemetry.temperature = format!("{:.0} °C", cpu_temp);
                    telemetry.internet_connection = if telemetry.ipv4.is_empty() { "Disconnected" } else { "Connected" }.to_string();

                    let uptime_sec = System::uptime();
                    telemetry.uptime = format!("{} minutes", uptime_sec / 60);

                    telemetry.updated = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    if let Ok(json_str) = serde_json::to_string(&telemetry) {
                        println!("📡 [遥测输出] 广播最新状态 (在线人数: {}, 温度: {})", active_clients, telemetry.temperature);
                        let _ = state.tx.send(json_str);
                    }

                    println!("✅ 轮询任务完成，耗时: {}ms", start_poll.elapsed().as_millis());
                }
            }
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let mut broadcast_rx = state.tx.subscribe();
    println!(
        "🔌 新的 WebSocket 客户端已连接 (当前在线: {})",
        state.tx.receiver_count()
    );
    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(msg) = broadcast_rx.recv() => {
                    if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Some(reply) = local_rx.recv() => {
                    if ws_sender.send(Message::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut recv_task = tokio::spawn({
        let state = state.clone();
        async move {
            while let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
                if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                    let (resp_tx, resp_rx) = oneshot::channel();
                    let _ = state
                        .actor_tx
                        .send(AtRequest {
                            action: cmd.action,
                            payload: cmd.payload,
                            resp_tx,
                        })
                        .await;

                    if let Ok(reply) = resp_rx.await {
                        let result_json = serde_json::json!({ "type": "at_res", "data": reply });
                        let _ = local_tx.send(result_json.to_string()).await;
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };

    let _ = send_task.await;
    let _ = recv_task.await;
    println!("👋 WebSocket 客户端已断开 (当前在线: {})", state.tx.receiver_count());
}

async fn style_handler() -> impl IntoResponse {
    let css_content = include_str!("../templates/style.css");
    Response::builder()
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .body(axum::body::Body::from(css_content))
        .unwrap()
}
