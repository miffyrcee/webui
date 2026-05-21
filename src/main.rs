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
use tokio::time::sleep;

// 从环境变量读取串口设备路径，默认 /dev/at_mdm0
const DEFAULT_SERIAL_PORT: &str = "/dev/at_mdm0";

#[derive(Debug)]
enum AtAction {
    ManualAt(String),
    SetInterval(u64),
}

struct AtRequest {
    action: AtAction,
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
        actor_tx,
    });

    tokio::spawn({
        let state = app_state.clone();
        async move { hardware_polling_actor(state, actor_rx, serial_path).await }
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

/// 同步发送 AT 命令并读取响应（阻塞，复用 File）
fn send_at_command_sync(file: &mut File, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();
    let full_cmd = format!("{}\r\n", cmd);

    file.write_all(full_cmd.as_bytes())
        .map_err(|e| format!("写失败: {}", e))?;
    file.flush().map_err(|e| format!("flush失败: {}", e))?;

    let mut buf = [0u8; 4096];
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

/// 解析合并命令的响应，提取各部分（使用字符串查找，更鲁棒）
fn parse_combined_response(raw: &str) -> (String, String, String) {
    let mut cpin = String::new();
    let mut qeng = String::new();
    let mut cgpaddr = String::new();

    // 1. 提取 +CPIN: 块
    if let Some(start) = raw.find("+CPIN:") {
        let remaining = &raw[start..];
        // 找到下一个命令或结束
        let end = remaining
            .find("+QENG:")
            .or_else(|| remaining.find("+CGPADDR:"))
            .unwrap_or(remaining.len());
        let block = &remaining[..end];
        // 截取到 OK 或 ERROR
        if let Some(ok_pos) = block.find("\r\nOK\r\n") {
            cpin = block[..ok_pos + 6].to_string();
        } else if let Some(err_pos) = block.find("\r\nERROR\r\n") {
            cpin = block[..err_pos + 8].to_string();
        } else {
            cpin = block.to_string();
        }
    }

    // 2. 提取 +QENG: 块
    if let Some(start) = raw.find("+QENG:") {
        let remaining = &raw[start..];
        let end = remaining.find("+CGPADDR:").unwrap_or(remaining.len());
        let block = &remaining[..end];
        if let Some(ok_pos) = block.find("\r\nOK\r\n") {
            qeng = block[..ok_pos + 6].to_string();
        } else if let Some(err_pos) = block.find("\r\nERROR\r\n") {
            qeng = block[..err_pos + 8].to_string();
        } else {
            qeng = block.to_string();
        }
    }

    // 3. 提取 +CGPADDR: 块（可能有多行）
    if let Some(start) = raw.find("+CGPADDR:") {
        let block = &raw[start..];
        if let Some(ok_pos) = block.find("\r\nOK\r\n") {
            cgpaddr = block[..ok_pos + 6].to_string();
        } else if let Some(err_pos) = block.find("\r\nERROR\r\n") {
            cgpaddr = block[..err_pos + 8].to_string();
        } else {
            cgpaddr = block.to_string();
        }
    }

    (cpin, qeng, cgpaddr)
}

fn parse_qeng(qeng_res: &str, telemetry: &mut TelemetryData) {
    if let Some(line) = qeng_res
        .lines()
        .find(|l| l.contains("+QENG: \"servingcell\""))
    {
        let line = line.trim();
        let parts: Vec<String> = line
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_string())
            .collect();
        if parts.len() >= 15 {
            telemetry.network_mode = format!("{} {}", parts[2], parts[3]);
            telemetry.mccmnc = format!("{}{}", parts[4], parts[5]);
            telemetry.cell_id = parts[6].clone();
            telemetry.enb_id = parts[7].clone();
            telemetry.tac = parts[8].clone();
            telemetry.bands = format!("NR5G BAND {}", parts[10]);
            telemetry.bandwidth = format!("{} MHz", parts[11]);
            telemetry.earfcn = parts[9].clone();
            telemetry.pci = parts[7].clone();

            let rsrp: i32 = parts[12].parse().unwrap_or(-140);
            let rsrq: i32 = parts[13].parse().unwrap_or(-20);
            let sinr: i32 = parts[14].parse().unwrap_or(-20);

            let rsrp_pct = ((rsrp + 140) as f32 / 96.0 * 100.0).clamp(0.0, 100.0) as i32;
            let rsrq_pct = ((rsrq + 20) as f32 / 17.0 * 100.0).clamp(0.0, 100.0) as i32;
            let sinr_pct = ((sinr + 20) as f32 / 50.0 * 100.0).clamp(0.0, 100.0) as i32;

            telemetry.ss_rsrp = format!("{} / {}%", rsrp, rsrp_pct);
            telemetry.ss_rsrq = format!("{} / {}%", rsrq, rsrq_pct);
            telemetry.sinr = format!("{} / {}%", sinr, sinr_pct);
            telemetry.signal_percentage = format!("{}%", rsrp_pct);
            telemetry.assessment = if rsrp > -80 && sinr > 20 {
                "Excellent".to_string()
            } else {
                "Good".to_string()
            };
        } else {
            println!("⚠️ QENG 字段数不足: {} (需要 >=15)", parts.len());
        }
    } else {
        println!("⚠️ 未找到 +QENG 响应");
    }
}

/// 解析 +CGPADDR 响应
fn parse_cgpaddr(gpad_res: &str, telemetry: &mut TelemetryData) {
    for line in gpad_res.lines() {
        if line.contains("+CGPADDR:") {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                let ipv4 = parts[1].trim_matches('"').trim();
                if !ipv4.is_empty() && ipv4 != "0.0.0.0" && telemetry.ipv4.is_empty() {
                    telemetry.ipv4 = ipv4.to_string();
                }
            }
            if parts.len() >= 3 {
                let ipv6 = parts[2].trim_matches('"').trim();
                if !ipv6.is_empty() && !ipv6.starts_with("0.0.0.0") && telemetry.ipv6.is_empty() {
                    telemetry.ipv6 = ipv6.to_string();
                }
            }
        }
    }
    if telemetry.ipv4.is_empty() {
        telemetry.ipv4 = "--".to_string();
    }
    if telemetry.ipv6.is_empty() {
        telemetry.ipv6 = "--".to_string();
    }
}

async fn hardware_polling_actor(
    state: Arc<AppState>,
    mut actor_rx: mpsc::Receiver<AtRequest>,
    serial_path: String,
) {
    let mut sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();

    // 打开串口文件（一次性，复用）
    let mut serial_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&serial_path)
    {
        Ok(f) => {
            println!("✅ 串口已打开: {}", serial_path);
            Some(f)
        }
        Err(e) => {
            eprintln!("❌ 打开串口失败: {}，将运行在模拟模式", e);
            None
        }
    };

    let mut interval_secs = 3;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await;

    loop {
        tokio::select! {
            Some(req) = actor_rx.recv() => {
                match req.action {
                    AtAction::ManualAt(cmd) => {
                        println!("👨‍💻 用户手动执行 AT: {}", cmd);
                        let response = if let Some(file) = &mut serial_file {
                            match send_at_command_sync(file, &cmd) {
                                Ok(resp) => resp,
                                Err(e) => format!("ERROR: {}", e),
                            }
                        } else {
                            sleep(Duration::from_millis(50)).await;
                            "OK\r\n".to_string()
                        };
                        let _ = req.resp_tx.send(response);
                    }
                    AtAction::SetInterval(secs) => {
                        let new_secs = secs.max(3);
                        if new_secs != interval_secs {
                            interval_secs = new_secs;
                            interval = tokio::time::interval(Duration::from_secs(interval_secs));
                            interval.tick().await;
                            println!("🔄 轮询间隔已动态调整为 {} 秒", interval_secs);
                        }
                        let _ = req.resp_tx.send("OK".to_string());
                    }
                }
            }

            _ = interval.tick() => {
                let active_clients = state.tx.receiver_count();
                if active_clients > 0 {
                    let start_poll = std::time::Instant::now();
                    println!("📡 开始周期性硬件轮询 (当前在线客户端: {})", active_clients);

                    let (cpin_res, qeng_res, gpad_res) = if let Some(file) = &mut serial_file {
                        let combined = "AT+CPIN?;+QENG=\"servingcell\";+CGPADDR";
                        match send_at_command_sync(file, combined) {
                            Ok(resp) => parse_combined_response(&resp),
                            Err(e) => {
                                println!("⚠️ 合并命令失败: {}，降级到单独发送", e);
                                let cpin = send_at_command_sync(file, "AT+CPIN?").unwrap_or_default();
                                let qeng = send_at_command_sync(file, "AT+QENG=\"servingcell\"").unwrap_or_default();
                                let gpad = send_at_command_sync(file, "AT+CGPADDR").unwrap_or_default();
                                (cpin, qeng, gpad)
                            }
                        }
                    } else {
                        sleep(Duration::from_millis(50)).await;
                        (
                            "+CPIN: READY\r\nOK\r\n".to_string(),
                            "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-64,-11,22,1,-\r\nOK\r\n".to_string(),
                            "+CGPADDR: 1,\"10.202.165.254\",\"2409::1\"\r\nOK\r\n".to_string(),
                        )
                    };

                    let sim_status = if cpin_res.contains("READY") { "Active" } else { "No Card / Locked" };

                    let mut telemetry = TelemetryData::default();
                    telemetry.sim_status = sim_status.to_string();
                    telemetry.active_sim = "SIM 1".to_string();
                    telemetry.network_provider = "CHN-UNICOM".to_string();
                    telemetry.apn = "3gnet".to_string();
                    telemetry.traffic_stats = "N/A".to_string();

                    parse_qeng(&qeng_res, &mut telemetry);
                    parse_cgpaddr(&gpad_res, &mut telemetry);

                    sys.refresh_cpu_all();
                    components.refresh(false);
                    let cpu_temp = components.iter()
                        .find(|c| c.label().to_uppercase().contains("CPU") || c.label().to_uppercase().contains("PACKAGE"))
                        .map_or(46.0, |c| c.temperature().unwrap_or(46.0));

                    telemetry.temperature = format!("{:.0} °C", cpu_temp);
                    telemetry.internet_connection = if telemetry.ipv4 != "--" { "Connected".to_string() } else { "Disconnected".to_string() };

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
                    let action = match cmd.action.as_str() {
                        "manual_at" => AtAction::ManualAt(cmd.payload.unwrap_or_default()),
                        "set_interval" => {
                            let secs = cmd.payload.and_then(|p| p.parse().ok()).unwrap_or(3);
                            AtAction::SetInterval(secs)
                        }
                        _ => continue,
                    };
                    let _ = state.actor_tx.send(AtRequest { action, resp_tx }).await;
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
