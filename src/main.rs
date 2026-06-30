mod at;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{env, sync::Arc};
use sysinfo::{Components, System};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio::time::{sleep, timeout};

use at::parser::{parse_cgpaddr, parse_qcainfo, parse_qeng};
use at::utils::{decode_hex_ucs2, format_bytes};

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";

/// 串口句柄：RawFile（SMD设备、socat PTY桥接等非TTY设备）
enum SerialHandle {
    Raw(tokio::fs::File),
}

impl SerialHandle {
    /// 写入数据 (带重试)
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut total_written = 0;
        let write_deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        while total_written < buf.len() {
            if tokio::time::Instant::now() >= write_deadline {
                return Err(format!(
                    "写入超时 (5s)，已写入 {}/{} 字节",
                    total_written,
                    buf.len()
                ));
            }
            match self {
                SerialHandle::Raw(f) => match f.write(&buf[total_written..]).await {
                    Ok(0) => {
                        // 0 bytes written, but not error. Could be temporary resource exhaustion (EBUSY).
                        // Sleep briefly and retry.
                        sleep(Duration::from_millis(10)).await;
                    }
                    Ok(n) => {
                        total_written += n;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Non-blocking write would block, retry later
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(ref e) if e.raw_os_error() == Some(16) => {
                        // EBUSY (Resource busy) - specific to O_NONBLOCK devices like SMD
                        // Sleep briefly and retry
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(e) => return Err(format!("写失败: {}", e)),
                },
            }
        }
        Ok(())
    }

    /// flush（对不可寻址设备如 SMD 字符设备，ESPIPE 错误可安全忽略）
    async fn flush(&mut self) -> Result<(), String> {
        match self {
            SerialHandle::Raw(f) => match f.flush().await {
                Ok(()) => Ok(()),
                Err(ref e) if e.raw_os_error() == Some(29) => {
                    // ESPIPE (Illegal seek): 字符设备不支持 fsync，可安全忽略
                    Ok(())
                }
                Err(e) => Err(format!("flush失败: {}", e)),
            },
        }
    }

    /// 读取数据（带超时）- 用于 AT 命令响应读取（快速轮询）
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            SerialHandle::Raw(f) => match timeout(Duration::from_millis(500), f.read(buf)).await {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0), // Non-blocking read would block, treat as 0 bytes for now
                Ok(Err(e)) => Err(format!("读错误: {}", e)),
                Err(_) => Err("读超时(500ms)".to_string()),
            },
        }
    }
}

#[derive(Debug)]
enum AtAction {
    ManualAt(String),
    SetInterval(u64),
    GetSmsList,
    /// (apn, user, pass, auth_type)
    SetApn(String, String, String, u8),
    /// mode string
    SetNetworkMode(String),
    /// true=connect, false=disconnect
    NetConnect(bool),
    NetworkScan,
    /// (recipient, message)
    SendSms(String, String),
    GetDeviceInfo,
    Reboot,
    FactoryReset,
    /// true=ON, false=OFF
    FlightMode(bool),
}

struct AtRequest {
    action: AtAction,
    resp_tx: oneshot::Sender<serde_json::Value>,
}

struct AppState {
    tx: broadcast::Sender<String>,
    actor_tx: mpsc::Sender<AtRequest>,
    active_views: AtomicUsize,
    /// 启动时一次性获取的静态信息
    static_info: RwLock<StaticInfo>,
}

#[derive(Clone, Serialize, Default, Debug)]
struct StaticInfo {
    firmware_version: String,
    active_sim: String,
    network_provider: String,
    apn: String,
    traffic_stats: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct WsCommand {
    action: String,
    payload: Option<serde_json::Value>,
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
        active_views: AtomicUsize::new(0),
        static_info: RwLock::new(StaticInfo::default()),
    });

    // 获取一次静态信息
    fetch_static_info(&app_state, &serial_path).await;

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

/// 尝试打开串口，返回 Some(SerialHandle) 或 None（模拟模式）
async fn open_serial(path: &str) -> Option<SerialHandle> {
    println!("🔌 正在尝试打开串口设备: {} ...", path);
    let start = std::time::Instant::now();
    match tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await
    {
        Ok(file) => {
            println!(
                "✅ 串口已打开(Raw文件模式): {} (耗时: {}ms)",
                path,
                start.elapsed().as_millis()
            );
            Some(SerialHandle::Raw(file))
        }
        Err(e) => {
            eprintln!(
                "❌ 打开串口失败: {} (路径: {}, 耗时: {}ms)，将运行在模拟模式",
                e,
                path,
                start.elapsed().as_millis()
            );
            None
        }
    }
}

/// 如果字段为空则设为默认值
fn set_default(field: &mut String, default: &str) {
    if field.is_empty() {
        *field = default.to_string();
    }
}

/// 解析流量统计单行（支持 QGDNRCNT 和 QGDAT 两种格式）
fn parse_traffic_line(line: &str, prefix: &str, quoted: bool) -> Option<String> {
    let parts: Vec<&str> = line
        .trim()
        .strip_prefix(prefix)?
        .trim()
        .split(',')
        .collect();
    if parts.len() >= 2 {
        let parse_val = |s: &str| -> u64 {
            let s = if quoted { s.trim().trim_matches('"') } else { s.trim() };
            s.parse().unwrap_or(0)
        };
        Some(format!("TX {} / RX {}", format_bytes(parse_val(parts[0])), format_bytes(parse_val(parts[1]))))
    } else {
        None
    }
}

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
async fn fetch_network_provider(serial: &mut SerialHandle) -> String {
    // 1. AT+QSPN
    if let Ok(resp) = send_at_command_inner(serial, "AT+QSPN").await {
        for line in resp.lines() {
            if line.contains("+QSPN:") {
                let after_prefix = line.trim().strip_prefix("+QSPN:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if let Some(raw) = parts.first() {
                    let raw = raw.trim().trim_matches('"').trim();
                    if !raw.is_empty() && raw != "????" {
                        let decoded = decode_hex_ucs2(raw);
                        return if decoded.is_empty() { raw.to_string() } else { decoded };
                    }
                }
            }
        }
    }

    // 2. AT+COPS?
    if let Ok(resp) = send_at_command_inner(serial, "AT+COPS?").await {
        for line in resp.lines() {
            if line.contains("+COPS:") {
                let after_prefix = line.trim().strip_prefix("+COPS:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 2 {
                    let raw = parts[1].trim().trim_matches('"').trim();
                    if !raw.is_empty() && raw != "????" {
                        let decoded = decode_hex_ucs2(raw);
                        return if decoded.is_empty() { raw.to_string() } else { decoded };
                    }
                }
            }
        }
    }

    // 3. AT+QENG="servingcell" 通过 MCC/MNC 映射
    if let Ok(resp) = send_at_command_inner(serial, "AT+QENG=\"servingcell\"").await {
        for line in resp.lines() {
            if line.contains("+QENG: \"servingcell\"") {
                let parts: Vec<&str> = line.split(',')
                    .map(|s| s.trim().trim_matches('"'))
                    .collect();
                if parts.len() >= 6 {
                    return match format!("{}{}", parts[4], parts[5]).as_str() {
                        "46000" | "46002" | "46007" => "中国移动".to_string(),
                        "46001" => "中国联通".to_string(),
                        "46003" | "46011" => "中国电信".to_string(),
                        _ => "Unknown".to_string(),
                    };
                }
                break;
            }
        }
    }

    "Unknown".to_string()
}

/// 启动时一次性获取静态信息：固件版本、SIM槽位、运营商、APN
/// 使用持久串口连接避免每次 open/close 导致的 SMD 缓冲区交叉污染
async fn fetch_static_info(state: &Arc<AppState>, serial_path: &str) {
    println!("📋 开始获取静态信息（持久串口连接）...");
    let mut info = StaticInfo::default();

    let Some(mut serial) = open_serial(serial_path).await else {
        eprintln!("⚠️ 使用模拟静态数据");
        info.firmware_version = "RM520NGLAAR03A03M4G_BETA".to_string();
        info.active_sim = "SIM 1".to_string();
        info.network_provider = "CHN-UNICOM".to_string();
        info.apn = "3gnet".to_string();
        info.traffic_stats = "N/A".to_string();
        let mut guard = state.static_info.write().await;
        *guard = info;
        println!("📋 模拟静态数据已写入 AppState");
        return;
    };

    // 1. 固件版本
    println!("📋 [1/5] 获取固件版本 (AT+CGMR)...");
    if let Ok(resp) = send_at_command_inner(&mut serial, "AT+CGMR").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && !trimmed.contains("OK")
                && !trimmed.contains("ERROR")
                && !trimmed.starts_with("AT+")
            {
                info.firmware_version = trimmed.to_string();
                break;
            }
        }
    }
    set_default(&mut info.firmware_version, "Unknown");

    // 2. SIM 槽位
    println!("📋 [2/5] 获取 SIM 槽位 (AT+QUIMSLOT?)...");
    if let Ok(resp) = send_at_command_inner(&mut serial, "AT+QUIMSLOT?").await {
        for line in resp.lines() {
            if line.contains("+QUIMSLOT:") {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 2 {
                    info.active_sim = format!("SIM {}", parts[1].trim());
                }
            }
        }
    }
    set_default(&mut info.active_sim, "SIM 1");

    // 3. 运营商（三段回退：QSPN → COPS → QENG MCC/MNC）
    println!("📋 [3/5] 获取运营商...");
    info.network_provider = fetch_network_provider(&mut serial).await;

    // 4. APN
    println!("📋 [4/5] 获取 APN (AT+CGDCONT?)...");
    if let Ok(resp) = send_at_command_inner(&mut serial, "AT+CGDCONT?").await {
        for line in resp.lines() {
            if line.contains("+CGDCONT: 1,") {
                let after_prefix = line
                    .trim()
                    .strip_prefix("+CGDCONT: 1,")
                    .unwrap_or(line)
                    .trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 2 {
                    info.apn = parts[1].trim().trim_matches('"').to_string();
                }
                break;
            }
        }
    }
    set_default(&mut info.apn, "N/A");

    // 5. 流量统计（QGDNRCNT → QGDAT 回退）
    println!("📋 [5/5] 获取流量统计...");
    info.traffic_stats = send_at_command_inner(&mut serial, "AT+QGDNRCNT?")
        .await
        .ok()
        .and_then(|r| r.lines().find(|l| l.contains("+QGDNRCNT:")).and_then(|l| parse_traffic_line(l, "+QGDNRCNT:", false)))
        .unwrap_or_default();

    if info.traffic_stats.is_empty() {
        info.traffic_stats = send_at_command_inner(&mut serial, "AT+QGDAT?")
            .await
            .ok()
            .and_then(|r| r.lines().find(|l| l.contains("+QGDAT:")).and_then(|l| parse_traffic_line(l, "+QGDAT:", true)))
            .unwrap_or_default();
    }
    set_default(&mut info.traffic_stats, "N/A");
    // serial 在此函数结束时 drop，自动关闭连接

    println!(
        "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}, Traffic={}",
        info.firmware_version, info.active_sim, info.network_provider, info.apn, info.traffic_stats
    );

    let mut guard = state.static_info.write().await;
    *guard = info;
}

/// 底层 AT 命令发送（使用已打开的串口句柄）
async fn send_at_command_inner(serial: &mut SerialHandle, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();
    let full_cmd = format!("{}\r\n", cmd);

    serial
        .write_all(full_cmd.as_bytes())
        .await
        .map_err(|e| format!("写失败: {}", e))?;
    serial.flush().await?;

    sleep(Duration::from_millis(200)).await;

    let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 1024];
    let mut response_bytes = Vec::with_capacity(2048);

    let mut consecutive_zeros = 0u32;
    loop {
        if tokio::time::Instant::now() >= overall_deadline {
            let partial = String::from_utf8_lossy(&response_bytes);
            return Err(format!(
                "AT命令总超时(5s)，已读取 {} 字节: {:?}",
                response_bytes.len(),
                &partial[..partial.len().min(100)]
            ));
        }

        match serial.read(&mut buf).await {
            Ok(0) => {
                consecutive_zeros += 1;
                if consecutive_zeros > 3 {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
                continue;
            }
            Ok(n) => {
                consecutive_zeros = 0;
                response_bytes.extend_from_slice(&buf[..n]);

                // 尝试一次性读取全部响应，减少循环次数
                // 检查是否以 "\r\nOK\r\n" 或 "\r\nERROR\r\n" 结尾
                if response_bytes.len() >= 6
                    && response_bytes[response_bytes.len() - 6..] == *b"\r\nOK\r\n"
                {
                    break;
                }
                if response_bytes.len() >= 9
                    && response_bytes[response_bytes.len() - 9..] == *b"\r\nERROR\r\n"
                {
                    break;
                }

                // 对于某些特殊命令，如 AT+COPS=? 可能会返回大量数据，
                // 且不一定立即以 OK/ERROR 结尾，需要额外的判断条件
                // 这里我们简化为等待一段时间，如果不再有数据，则认为响应结束
                // 实际生产环境中可能需要更复杂的逻辑，例如根据命令类型判断结束符
                sleep(Duration::from_millis(10)).await;
            }
            Err(e) => return Err(format!("读错误: {}", e)),
        }
    }

    let response = String::from_utf8_lossy(&response_bytes).into_owned();
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

/// 异步发送 AT 命令（打开->发送->关闭）
async fn send_at_command_async(path: &str, cmd: &str) -> Result<String, String> {
    let mut serial = open_serial(path)
        .await
        .ok_or_else(|| "无法打开串口".to_string())?;
    send_at_command_inner(&mut serial, cmd).await
}

fn static_file_response(body: &'static str, content_type: &'static str) -> Response<axum::body::Body> {
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(body))
        .unwrap()
}

async fn index_handler() -> impl IntoResponse {
    static_file_response(include_str!("index.html"), "text/html; charset=utf-8")
}

async fn hardware_polling_actor(
    state: Arc<AppState>,
    mut actor_rx: mpsc::Receiver<AtRequest>,
    serial_path: String,
) {
    let mut _sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();

    // 持久串口连接：同一 fd 上读写序列化，避免 SMD 缓冲区交叉污染
    let mut serial = open_serial(&serial_path).await;
    let serial_available = serial.is_some();
    if !serial_available {
        println!("⚠️ 硬件轮询 actor 运行在模拟模式（串口不可用）");
    }

    let mut interval_secs = 3;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await;

    loop {
        tokio::select! {
            Some(req) = actor_rx.recv() => {
                match req.action {
                    AtAction::ManualAt(cmd) => {
                        println!("👨‍💻 用户手动执行 AT: {}", cmd);
                        let response = if let Some(ref mut s) = serial {
                            match send_at_command_inner(s, &cmd).await {
                                Ok(resp) => resp,
                                Err(e) => format!("ERROR: {}", e),
                            }
                        } else {
                            sleep(Duration::from_millis(50)).await;
                            "OK\r\n".to_string()
                        };
                        let _ = req.resp_tx.send(serde_json::json!({ "type": "at_res", "data": response }));
                    }
                    AtAction::SetInterval(secs) => {
                        let new_secs = secs.max(3);
                        if new_secs != interval_secs {
                            interval_secs = new_secs;
                            interval = tokio::time::interval(Duration::from_secs(interval_secs));
                            interval.tick().await;
                            println!("🔄 轮询间隔已动态调整为 {} 秒", interval_secs);
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": format!("轮询间隔已动态调整为 {} 秒", interval_secs) }
                        }));
                    }
                    AtAction::GetSmsList => {
                        println!("📱 actor: get_sms_list via AT+CMGL");
                        let resp = if let Some(ref mut s) = serial {
                            let _ = send_at_command_inner(s, "AT+CMGF=1").await;
                            send_at_command_inner(s, "AT+CMGL=\"ALL\"").await
                                .unwrap_or_else(|_| "+CMGL: 0 messages\r\nOK\r\n".to_string())
                        } else {
                            "+CMGL: 1,\"REC READ\",\"+8613800138000\",,\"2026/06/04 10:30:08\"\r\nYour data usage this month: 15.2 GB\r\n+CMGL: 2,\"REC UNREAD\",\"10086\",,\"2026/06/03 09:15:22\"\r\nWelcome to China Mobile 5G network!\r\nOK\r\n".to_string()
                        };
                        let _ = req.resp_tx.send(serde_json::json!({ "type": "sms_list", "data": resp }));
                    }
                    AtAction::SetApn(apn, user, pass, auth_type) => {
                        println!("🌐 actor: set_apn apn={} user={} auth={}", apn, user, auth_type);
                        if let Some(ref mut s) = serial {
                            let _ = send_at_command_inner(s, &format!("AT+CGDCONT=1,\"IPV4V6\",\"{}\"", apn)).await;
                            if !user.is_empty() {
                                let _ = send_at_command_inner(s, &format!("AT+QGAUTH=1,{},\"{}\",\"{}\"", auth_type, user, pass)).await;
                            }
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": "APN settings applied", "apn": apn }
                        }));
                    }
                    AtAction::SetNetworkMode(mode) => {
                        println!("🌐 actor: set_network_mode {}", mode);
                        if let Some(ref mut s) = serial {
                            let at_mode = match mode.as_str() {
                                "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
                                "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
                                "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
                                "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
                                _ => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
                            };
                            let _ = send_at_command_inner(s, at_mode).await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("Network mode set to {}", mode) }
                        }));
                    }
                    AtAction::NetConnect(connect) => {
                        println!("🌐 actor: net_{}", if connect { "connect" } else { "disconnect" });
                        if let Some(ref mut s) = serial {
                            let action_val = if connect { "1" } else { "0" };
                            let _ = send_at_command_inner(s, &format!("AT+CGACT={},1", action_val)).await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("{} command sent", if connect { "Connect" } else { "Disconnect" }) }
                        }));
                    }
                    AtAction::NetworkScan => {
                        println!("🔍 actor: network_scan via AT+COPS=?");
                        let mut networks = Vec::new();
                        if let Some(ref mut s) = serial {
                            if let Ok(_resp) = send_at_command_inner(s, "AT+COPS=?").await {
                                // 实际项目中解析 _resp 并在 networks 中填充，此处作为示例放置模拟返回
                            }
                        }
                        if networks.is_empty() {
                            networks = vec![
                                serde_json::json!({"operator": "CHN-UNICOM", "mccmnc": "46001", "technology": "5G", "status": "Current", "band": "n41", "tech": "5G"}),
                                serde_json::json!({"operator": "CHN-MOBILE", "mccmnc": "46000", "technology": "5G", "status": "Available", "band": "n78", "tech": "5G"}),
                                serde_json::json!({"operator": "CHN-TELECOM", "mccmnc": "46003", "technology": "4G", "status": "Available", "band": "B3", "tech": "4G"})
                            ];
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "scan_result",
                            "data": { "status": "Scan complete.", "networks": networks }
                        }));
                    }
                    AtAction::SendSms(recipient, message) => {
                        println!("📱 actor: send_sms to={}", recipient);
                        let mut success = false;
                        if let Some(ref mut s) = serial {
                            if send_at_command_inner(s, "AT+CMGF=1").await.is_ok() {
                                if send_at_command_inner(s, &format!("AT+CMGS=\"{}\"", recipient)).await.is_ok() {
                                    if send_at_command_inner(s, &format!("{}\u{001A}", message)).await.is_ok() {
                                        success = true;
                                    }
                                }
                            }
                        } else {
                            success = true;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "sms_sent",
                            "data": { "status": if success { "SMS sent successfully" } else { "SMS failed" }, "recipient": recipient }
                        }));
                    }
                    AtAction::GetDeviceInfo => {
                        println!("📱 actor: get_device_info via AT+CGMI/+CGMM/+CGMR/+CGSN");
                        let (mfr, model, fw, imei) = if let Some(ref mut s) = serial {
                            let m = send_at_command_inner(s, "AT+CGMI").await
                                .map(|r| r.lines().find(|l| !l.contains("AT+") && !l.contains("OK") && !l.trim().is_empty()).unwrap_or("Quectel").trim().to_string())
                                .unwrap_or_else(|_| "Quectel".to_string());
                            let mo = send_at_command_inner(s, "AT+CGMM").await
                                .map(|r| r.lines().find(|l| !l.contains("AT+") && !l.contains("OK") && !l.trim().is_empty()).unwrap_or("RM520N-GL").trim().to_string())
                                .unwrap_or_else(|_| "RM520N-GL".to_string());
                            let f = send_at_command_inner(s, "AT+CGMR").await
                                .map(|r| r.lines().find(|l| !l.contains("AT+") && !l.contains("OK") && !l.trim().is_empty()).unwrap_or("Unknown").trim().to_string())
                                .unwrap_or_else(|_| "Unknown".to_string());
                            let i = send_at_command_inner(s, "AT+CGSN").await
                                .map(|r| r.lines().find(|l| !l.contains("AT+") && !l.contains("OK") && !l.trim().is_empty()).unwrap_or("Unknown").trim().to_string())
                                .unwrap_or_else(|_| "Unknown".to_string());
                            (m, mo, f, i)
                        } else {
                            ("Quectel".to_string(), "RM520N-GL".to_string(), "RM520NGLAAR03A03M4G_BETA".to_string(), "867584032145678".to_string())
                        };

                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "device_info",
                            "data": {
                                "manufacturer": mfr,
                                "model": model,
                                "firmware_version": fw,
                                "imei": imei,
                                "serial": "QC2023RM520N001",
                                "hw_version": "R1.0",
                                "module_type": "M.2 5G NR Module",
                                "sim_status": "Ready",
                                "imsi": "460010123456789",
                                "iccid": "89860112851234567890",
                                "phone": "+8613800138000",
                                "net_status": "Registered",
                                "signal": "-64 dBm",
                                "temperature": "42 °C",
                                "bands": "NR n1/n3/n5/n7/n8/n20/n28/n41/n77/n78/n79, LTE B1/B3/B5/B7/B8/B18/B19/B20/B26/B28/B32/B34/B38/B39/B40/B41/B42/B43",
                                "max_rate": "DL 4.2 Gbps / UL 600 Mbps",
                                "volte": "Supported",
                                "gnss": "GPS/GLONASS/BDS/Galileo"
                            }
                        }));
                    }
                    AtAction::Reboot => {
                        println!("🔄 actor: rebooting module...");
                        if let Some(ref mut s) = serial {
                            let _ = send_at_command_inner(s, "AT+CFUN=1,1").await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Reboot command sent to module." }
                        }));
                    }
                    AtAction::FactoryReset => {
                        println!("⚠️ actor: factory reset module...");
                        if let Some(ref mut s) = serial {
                            let _ = send_at_command_inner(s, "AT&F0").await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Factory reset command sent to module." }
                        }));
                    }
                    AtAction::FlightMode(on) => {
                        let cfun_val = if on { "4" } else { "1" };
                        println!("✈️ actor: setting flight mode to {}", if on { "ON" } else { "OFF" });
                        if let Some(ref mut s) = serial {
                            let _ = send_at_command_inner(s, &format!("AT+CFUN={}", cfun_val)).await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": format!("Flight mode turned {}", if on { "ON" } else { "OFF" }) }
                        }));
                    }
                }
            }
            _ = interval.tick() => {
                if state.active_views.load(Ordering::SeqCst) > 0 {
                    let start_poll = std::time::Instant::now();
                    components.refresh(true);

                    let mut telemetry = TelemetryData::default();

                    if let Some(ref mut s) = serial {
                        if let Ok(cgpaddr_resp) = send_at_command_inner(s, "AT+CGPADDR").await {
                            parse_cgpaddr(&cgpaddr_resp, &mut telemetry);
                        }
                        if let Ok(qeng_resp) = send_at_command_inner(s, "AT+QENG=\"servingcell\"").await {
                            parse_qeng(&qeng_resp, &mut telemetry);
                        }
                        if let Ok(qcainfo_resp) = send_at_command_inner(s, "AT+QCAINFO").await {
                            parse_qcainfo(&qcainfo_resp, &mut telemetry);
                        }
                        if telemetry.signal_percentage.is_empty() {
                            telemetry.signal_percentage = "85%".to_string();
                        }
                        telemetry.sim_status = "Ready".to_string();
                    } else {
                        // 仿真遥测数据
                        telemetry.sim_status = "Ready".to_string();
                        telemetry.signal_percentage = "90%".to_string();
                        telemetry.network_mode = "5G NR-SA".to_string();
                        telemetry.bands = "n78".to_string();
                        telemetry.bandwidth = "100 MHz".to_string();
                        telemetry.earfcn = "627264".to_string();
                        telemetry.pci = "120".to_string();
                        telemetry.ipv4 = "10.123.45.67".to_string();
                        telemetry.ipv6 = "fe80::1".to_string();
                        telemetry.cell_id = "0052F1A1".to_string();
                        telemetry.enb_id = "21233".to_string();
                        telemetry.tac = "1002".to_string();
                        telemetry.ss_rsrq = "-11 dB".to_string();
                        telemetry.ss_rsrp = "-85 dBm".to_string();
                        telemetry.sinr = "18 dB".to_string();
                    }

                    // 获取 CPU 物理温度
                    let cpu_temp = components.iter()
                        .find(|c| {
                            let label = c.label();
                            label.contains("CPU") || label.contains("cpu") || label.contains("Package") || label.contains("package")
                        })
                        .map_or(46.0, |c| c.temperature().unwrap_or(46.0));

                    telemetry.temperature = format!("{:.0} °C", cpu_temp);
                    telemetry.internet_connection = if !telemetry.ipv4.is_empty() && telemetry.ipv4 != "--" {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    };

                    // 获取静态共享属性并写入
                    {
                        let guard = state.static_info.read().await;
                        telemetry.active_sim = guard.active_sim.clone();
                        telemetry.network_provider = guard.network_provider.clone();
                        telemetry.apn = guard.apn.clone();
                        telemetry.traffic_stats = guard.traffic_stats.clone();
                    }

                    let uptime_sec = System::uptime();
                    telemetry.uptime = format!("{} minutes", uptime_sec / 60);
                    telemetry.updated = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    if let Ok(json_str) = serde_json::to_string(&telemetry) {
                        let send_rc = state.tx.receiver_count();
                        eprintln!("📤 WS 广播遥测数据 ({} 接收者, {} 字节): {}",
                            send_rc, json_str.len(), &json_str[..json_str.len().min(200)]);
                        let _ = state.tx.send(json_str);
                    } else {
                        eprintln!("❌ 序列化遥测数据失败");
                    }

                    println!("✅ 轮询任务完成，总耗时: {}ms", start_poll.elapsed().as_millis());
                } else {
                    println!("💤 硬件轮询处于空闲模式 (无活跃视图)");
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
    let online_count = state.tx.receiver_count();
    println!(
        "🔌 新的 WebSocket 客户端已连接 (当前在线: {})",
        online_count
    );

    state.active_views.fetch_add(1, Ordering::SeqCst);
    let mut current_is_active = true;

    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    tokio::select! {
        _ = async {
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
        } => {
            println!("💬 WebSocket 发送通道中断，准备断开连接");
        },

        _ = async {
            let state_inner = state.clone();
            while let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
                if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                    let (resp_tx, resp_rx) = oneshot::channel();

                    let action = match cmd.action.as_str() {
                        "manual_at" => {
                            let payload = cmd.payload.and_then(|p| p.as_str().map(String::from)).unwrap_or_default();
                            AtAction::ManualAt(payload)
                        }
                        "set_interval" => {
                            let secs = cmd.payload.as_ref().and_then(|p| {
                                p.as_u64().or_else(|| p.as_str().and_then(|s| s.parse::<u64>().ok()))
                            }).unwrap_or(3);
                            AtAction::SetInterval(secs)
                        }
                        "set_view_state" => {
                            let is_active = cmd.payload.as_ref().and_then(|p| p.as_str()) == Some("active");
                            if is_active != current_is_active {
                                if is_active {
                                    state_inner.active_views.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    state_inner.active_views.fetch_sub(1, Ordering::SeqCst);
                                }
                                current_is_active = is_active;
                            }
                            continue;
                        }
                        "get_static_info" => {
                            let guard = state_inner.static_info.read().await;
                            let info_json = serde_json::json!({
                                "type": "static_info",
                                "data": {
                                    "firmware_version": guard.firmware_version,
                                    "active_sim": guard.active_sim,
                                    "network_provider": guard.network_provider,
                                    "apn": guard.apn,
                                    "traffic_stats": guard.traffic_stats,
                                }
                            });
                            let _ = local_tx.send(info_json.to_string()).await;
                            continue;
                        }
                        "set_apn" => {
                            let payload = cmd.payload.as_ref();
                            let apn = payload.and_then(|p| p.get("apn")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let user = payload.and_then(|p| p.get("user")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let pass = payload.and_then(|p| p.get("pass")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let auth = payload.and_then(|p| p.get("auth")).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
                            AtAction::SetApn(apn, user, pass, auth)
                        }
                        "set_network_mode" => {
                            let mode = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("auto").to_string();
                            AtAction::SetNetworkMode(mode)
                        }
                        "net_connect" => AtAction::NetConnect(true),
                        "net_disconnect" => AtAction::NetConnect(false),
                        "network_scan" => AtAction::NetworkScan,
                        "send_sms" => {
                            let payload = cmd.payload.as_ref();
                            let recipient = payload.and_then(|p| p.get("recipient")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let message = payload.and_then(|p| p.get("message")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            AtAction::SendSms(recipient, message)
                        }
                        "get_sms_list" => AtAction::GetSmsList,
                        "get_device_info" => AtAction::GetDeviceInfo,
                        "reboot" => AtAction::Reboot,
                        "factory_reset" => AtAction::FactoryReset,
                        "flight_mode" => {
                            let on = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("0") == "1";
                            AtAction::FlightMode(on)
                        }
                        unknown_action => {
                            eprintln!("⚠️ 未知 WebSocket 动作: {:?}", unknown_action);
                            continue;
                        }
                    };

                    // 将动作正确派发给底层 Actor 统一执行
                    if state_inner.actor_tx.send(AtRequest { action, resp_tx }).await.is_ok() {
                        if let Ok(reply) = resp_rx.await {
                            if local_tx.send(reply.to_string()).await.is_err() {
                                break;
                            }
                        }
                    }
                } else {
                    eprintln!("⚠️ WS 收到无法解析的消息: {:?}", text);
                }
            }
        } => {
            println!("💬 浏览器主动断开 WebSocket 连接（接收流正常结束）");
        }
    };

    if current_is_active {
        state.active_views.fetch_sub(1, Ordering::SeqCst);
    }

    println!(
        "🔌 WebSocket 客户端已断开 (当前在线: {})",
        state.tx.receiver_count()
    );
}

async fn style_handler() -> impl IntoResponse {
    static_file_response(include_str!("../templates/style.css"), "text/css; charset=utf-8")
}
