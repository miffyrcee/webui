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
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Components, System};
use tokio::sync::{broadcast, mpsc, oneshot};

// 从环境变量读取串口设备路径，默认 /dev/at_mdm0
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
    serial_path: String,
    actor_tx: mpsc::Sender<AtRequest>,
}

#[derive(Serialize, Deserialize, Debug)]
struct WsCommand {
    action: String,
    payload: Option<String>,
}

#[derive(Serialize, Default, Debug)]
struct TelemetryData {
    temperature: String,
    sim_status: String,
    signal_percentage: String,
    internet_connection: String,
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
    let serial_path =
        env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
    println!("🔌 使用串口设备: {}", serial_path);

    let (tx, _) = broadcast::channel(100);
    let (actor_tx, actor_rx) = mpsc::channel(32);

    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        serial_path: serial_path.clone(),
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

/// 同步发送 AT 命令并读取响应（阻塞）
fn send_at_command_sync(file: &mut File, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();
    let full_cmd = format!("{}\r\n", cmd);

    file.write_all(full_cmd.as_bytes())
        .map_err(|e| format!("写失败: {}", e))?;
    file.flush().map_err(|e| format!("flush失败: {}", e))?;

    let mut buf = [0u8; 1024];
    let mut response = String::new();

    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                response.push_str(&chunk);
                if response.contains("\r\nOK\r\n") || response.contains("\r\nERROR\r\n") {
                    break;
                }
            }
            Err(e) => return Err(format!("读错误: {}", e)),
        }
    }

    let elapsed = start.elapsed();
    if response.contains("ERROR") {
        println!("⚠️ [AT] {} 返回 ERROR ({}ms)", cmd, elapsed.as_millis());
    } else {
        println!(
            "✅ [AT] {} 成功 ({}ms):\n{}",
            cmd,
            elapsed.as_millis(),
            response
        );
    }
    Ok(response)
}

/// 异步执行 AT 命令（通过 spawn_blocking 运行同步读写）
async fn execute_at_command(path: &str, cmd: &str, timeout_ms: u64) -> String {
    let path = path.to_string();
    let cmd = cmd.to_string();
    let result = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| format!("打开设备失败: {}", e))?;
            send_at_command_sync(&mut file, &cmd)
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(resp))) => resp,
        Ok(Ok(Err(e))) => format!("ERROR: {}", e),
        Ok(Err(e)) => format!("ERROR: JoinError: {}", e),
        Err(_) => "ERROR: Timeout (spawn)".to_string(),
    }
}

async fn hardware_polling_actor(state: Arc<AppState>, mut actor_rx: mpsc::Receiver<AtRequest>) {
    let mut sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();
    let path = state.serial_path.clone();

    // 当前轮询间隔（秒），默认 3 秒
    let mut interval_secs = 3;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // 第一次立即触发

    loop {
        tokio::select! {
            Some(req) = actor_rx.recv() => {
                match req.action.as_str() {
                    "manual_at" => {
                        let cmd = req.payload.unwrap_or_default();
                        println!("👨‍💻 用户手动执行 AT: {}", cmd);
                        let response = execute_at_command(&path, &cmd, 2000).await;
                        let _ = req.resp_tx.send(response);
                    }
                    "set_interval" => {
                        if let Some(payload) = req.payload {
                            if let Ok(secs) = payload.parse::<u64>() {
                                let new_secs = secs.max(3); // 最小 3 秒
                                if new_secs != interval_secs {
                                    interval_secs = new_secs;
                                    interval = tokio::time::interval(Duration::from_secs(interval_secs));
                                    interval.tick().await; // 跳过立即触发，保持原有节奏
                                    println!("🔄 轮询间隔已动态调整为 {} 秒", interval_secs);
                                }
                            }
                        }
                        let _ = req.resp_tx.send("OK".to_string());
                    }
                    _ => {
                        let _ = req.resp_tx.send("Unknown action".to_string());
                    }
                }
            }

            _ = interval.tick() => {
                let active_clients = state.tx.receiver_count();
                if active_clients > 0 {
                    let start_poll = std::time::Instant::now();
                    println!("📡 开始周期性硬件轮询 (当前在线客户端: {})", active_clients);

                    let cpin_res = execute_at_command(&path, "AT+CPIN?", 500).await;
                    let qeng_res = execute_at_command(&path, "AT+QENG=\"servingcell\"", 2000).await;
                    let gpad_res = execute_at_command(&path, "AT+CGPADDR", 500).await;

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

                    // 获取 CPU 温度
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
