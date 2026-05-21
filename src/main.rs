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
use std::sync::Arc;
use sysinfo::{Components, System};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::time::{Duration, sleep};

// 配置内置共享内存接口
const SERIAL_PORT: &str = "/dev/smd11";

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
    serial_port: Arc<Mutex<Option<tokio::fs::File>>>, // 替换为安全的异步物理文件流
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
    let (tx, _) = broadcast::channel(100);
    let (actor_tx, actor_rx) = mpsc::channel(32);

    // 使用标准的文件读写方式打开虚拟通道，绕开串口波特率协商，彻底解决 "Not a typewriter" 冲突
    let serial_stream = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(SERIAL_PORT)
    {
        Ok(std_file) => {
            println!("✅ 成功打开移远模块内置通道: {}", SERIAL_PORT);
            // 转换为 tokio 异步文件接口
            Some(tokio::fs::File::from_std(std_file))
        }
        Err(e) => {
            eprintln!(
                "⚠️ 接口 {} 打开失败 ({})，系统将运行于模拟器模式下。",
                SERIAL_PORT, e
            );
            None
        }
    };

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

// 模拟或真实执行文件流 AT 读写
async fn send_at_command_raw(serial: &mut tokio::fs::File, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();

    println!("📤 [串口输出] -> 发送命令: {}", cmd);
    serial
        .write_all(format!("{}\r\n", cmd).as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut buffer = vec![0; 1024];
    let mut response = String::new();

    // 设定 500ms 超时等待响应
    let read_future = async {
        loop {
            match serial.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    response.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    if response.contains("OK\r\n") || response.contains("ERROR\r\n") {
                        break;
                    }
                }
            }
        }
        response
    };

    match tokio::time::timeout(Duration::from_millis(500), read_future).await {
        Ok(res) => {
            let duration = start.elapsed();
            if res.contains("ERROR") {
                println!(
                    "⚠️  [串口输入] <-命令: {}, 响应时间: {},返回值 {}.",
                    cmd,
                    duration.as_millis(),
                    res.trim()
                );
            } else {
                println!(
                    "📥 [串口输入] <- 响应成功 ({}ms):\n{}",
                    duration.as_millis(),
                    res.trim()
                );
            }
            Ok(res)
        }
        Err(_) => Err("Timeout".to_string()),
    }
}

async fn execute_at_command(serial_opt: &mut Option<tokio::fs::File>, cmd: &str) -> String {
    if let Some(serial) = serial_opt {
        match send_at_command_raw(serial, cmd).await {
            Ok(res) => res,
            Err(e) => format!("ERROR: {}", e),
        }
    } else {
        sleep(Duration::from_millis(50)).await;
        match cmd {
            "AT+CPIN?" => "+CPIN: READY\r\n\r\nOK\r\n".to_string(),
            "AT+QENG=\"servingcell\"" => "+QENG: \"servingcell\",\"NOCONN\",\"5G-SA\",\"TDD\",460,00,39074C001,59798720,7471151,41,100,-62,-11,22\r\n\r\nOK\r\n".to_string(),
            "AT+CGPADDR" => "+CGPADDR: 1,\"10.15.13.9\",\"2409:8970:a05:c21:bc94:7cd8:1735:604f\"\r\n\r\nOK\r\n".to_string(),
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"46000\",\"LTE BAND 3\",1500\r\n+QNWINFO: \"5G NR\",\"46000\",\"NR5G BAND 41\",504990\r\n\r\nOK\r\n".to_string(),
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
                    execute_at_command(&mut *serial_guard, &cmd).await
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

                    let cpin_res = execute_at_command(&mut *serial_guard, "AT+CPIN?").await;
                    let qeng_res = execute_at_command(&mut *serial_guard, "AT+QENG=\"servingcell\"").await;
                    let gpad_res = execute_at_command(&mut *serial_guard, "AT+CGPADDR").await;

                    let sim_status = if cpin_res.contains("READY") { "Active" } else { "No Card / Locked" };

                    let mut telemetry = TelemetryData::default();
                    telemetry.sim_status = sim_status.to_string();
                    telemetry.active_sim = "SIM 1".to_string();
                    telemetry.network_provider = "4E2D56FD79FB52A8".to_string();
                    telemetry.apn = "apn-here-inside-of-quotes".to_string();
                    telemetry.traffic_stats = "244 MB DL / 60 MB UL".to_string();

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

                    sys.refresh_cpu_all();
                    components.refresh(false);
                    let cpu_temp = components.iter()
                        .find(|c| c.label().to_uppercase().contains("CPU") || c.label().to_uppercase().contains("PACKAGE"))
                        .map_or(46.0, |c| c.temperature().unwrap_or(46.0));

                    println!("🌡️ [温度输入] 硬件传感器采集: {:.1} °C", cpu_temp);
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
        "🔌 新的 WebSocket 客户端已连接 (ID: {:p}, 当前在线: {})",
        &socket,
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
    println!(
        "👋 WebSocket 客户端已断开 (当前在线: {})",
        state.tx.receiver_count()
    );
}

async fn style_handler() -> impl IntoResponse {
    let css_content = include_str!("../templates/style.css");
    Response::builder()
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .body(axum::body::Body::from(css_content))
        .unwrap()
}
