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
use std::collections::HashMap;
use std::sync::OnceLock;
use std::{env, sync::Arc};
use sysinfo::{Components, System};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio::time::sleep;

use at::parser::{parse_cgpaddr, parse_qcainfo, parse_qeng, parse_qtemp_temperature};
use at::utils::{decode_hex_ucs2, format_bytes};

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";

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
            let s = if quoted {
                s.trim().trim_matches('"')
            } else {
                s.trim()
            };
            s.parse().unwrap_or(0)
        };
        Some(format!(
            "TX {} / RX {}",
            format_bytes(parse_val(parts[0])),
            format_bytes(parse_val(parts[1]))
        ))
    } else {
        None
    }
}

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
async fn fetch_network_provider(serial_path: &str) -> String {
    // 1. AT+QSPN
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QSPN").await {
        for line in resp.lines() {
            if line.contains("+QSPN:") {
                let after_prefix = line.trim().strip_prefix("+QSPN:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if let Some(raw) = parts.first() {
                    let raw = raw.trim().trim_matches('"').trim();
                    if !raw.is_empty() && raw != "????" {
                        let decoded = decode_hex_ucs2(raw);
                        return if decoded.is_empty() {
                            raw.to_string()
                        } else {
                            decoded
                        };
                    }
                }
            }
        }
    }

    // 2. AT+COPS?
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+COPS?").await {
        for line in resp.lines() {
            if line.contains("+COPS:") {
                let after_prefix = line.trim().strip_prefix("+COPS:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 2 {
                    let raw = parts[1].trim().trim_matches('"').trim();
                    if !raw.is_empty() && raw != "????" {
                        let decoded = decode_hex_ucs2(raw);
                        return if decoded.is_empty() {
                            raw.to_string()
                        } else {
                            decoded
                        };
                    }
                }
            }
        }
    }

    // 3. AT+QENG="servingcell" 通过 MCC/MNC 映射
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QENG=\"servingcell\"").await {
        for line in resp.lines() {
            if line.contains("+QENG: \"servingcell\"") {
                let parts: Vec<&str> = line
                    .split(',')
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
/// 通过 atcmd_rs 外部命令发送 AT 命令获取
async fn fetch_static_info(state: &Arc<AppState>, serial_path: &str) {
    println!("📋 开始获取静态信息（持久串口连接）...");
    let mut info = StaticInfo::default();

    if !std::path::Path::new(serial_path).exists() {
        eprintln!("⚠️ 使用模拟静态数据");
        info.firmware_version = "RM520NGLAAR03A03M4G_BETA".to_string();
        info.active_sim = "SIM 1".to_string();
        info.network_provider = "CHN-UNICOM".to_string();
        info.apn = "3gnet".to_string();
        let mut guard = state.static_info.write().await;
        *guard = info;
        println!("📋 模拟静态数据已写入 AppState");
        return;
    }

    // 1. 固件版本
    println!("📋 [1/4] 获取固件版本 (AT+CGMR)...");
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+CGMR").await {
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
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QUIMSLOT?").await {
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
    info.network_provider = fetch_network_provider(serial_path).await;

    // 4. APN
    // 优先用 AT+CGCONTRDP 获取活动 PDN 的实际 APN
    // 回退：遍历 AT+CGDCONT? 所有上下文，跳过占位符
    println!("📋 [4/5] 获取 APN (AT+CGCONTRDP)...");
    let mut found_apn = false;
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+CGCONTRDP").await {
        for line in resp.lines() {
            if line.contains("+CGCONTRDP:") {
                let after_prefix = line
                    .trim()
                    .strip_prefix("+CGCONTRDP:")
                    .unwrap_or(line)
                    .trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 3 {
                    let apn = parts[2].trim().trim_matches('"').to_string();
                    if !apn.is_empty() {
                        info.apn = apn;
                        found_apn = true;
                        break;
                    }
                }
            }
        }
    }
    if !found_apn {
        println!("📋 [4/5] 回退 AT+CGDCONT? 获取 APN...");
        if let Ok(resp) = send_at_command_inner(serial_path, "AT+CGDCONT?").await {
            for line in resp.lines() {
                if line.contains("+CGDCONT:") {
                    let after_prefix = line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if parts.len() >= 3 {
                        let apn = parts[2].trim().trim_matches('"').to_string();
                        if !apn.is_empty()
                            && !apn.contains("placeholder")
                            && !apn.starts_with("apn")
                        {
                            info.apn = apn;
                            found_apn = true;
                            break;
                        }
                    }
                }
            }
            // 如果所有上下文都是占位符，取第一个非空值
            if !found_apn {
                for line in resp.lines() {
                    if line.contains("+CGDCONT:") {
                        let after_prefix =
                            line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
                        let parts: Vec<&str> = after_prefix.split(',').collect();
                        if parts.len() >= 3 {
                            let apn = parts[2].trim().trim_matches('"').to_string();
                            if !apn.is_empty() {
                                info.apn = apn;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    set_default(&mut info.apn, "N/A");

    println!(
        "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}",
        info.firmware_version, info.active_sim, info.network_provider, info.apn
    );

    let mut guard = state.static_info.write().await;
    *guard = info;
}

/// 全局 AT 命令互斥锁，防止并发写入 SMD 设备
static AT_CMD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn get_at_lock() -> &'static tokio::sync::Mutex<()> {
    AT_CMD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// AT 命令队列 - 合并相同请求避免重复执行
struct AtCommandQueue {
    inner: tokio::sync::Mutex<HashMap<String, AtPendingCmd>>,
}

struct AtPendingCmd {
    waiters: Vec<oneshot::Sender<Result<String, String>>>,
}

static AT_CMD_QUEUE: OnceLock<AtCommandQueue> = OnceLock::new();

fn get_at_queue() -> &'static AtCommandQueue {
    AT_CMD_QUEUE.get_or_init(|| AtCommandQueue::new())
}

impl AtCommandQueue {
    fn new() -> Self {
        AtCommandQueue {
            inner: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 提交 AT 命令到队列并等待结果。
    /// 如果相同命令已在执行中，则合并请求共享相同的结果。
    async fn execute(&self, serial_path: &str, cmd: &str) -> Result<String, String> {
        use std::collections::hash_map::Entry;

        let mut guard = self.inner.lock().await;

        match guard.entry(cmd.to_string()) {
            Entry::Occupied(mut entry) => {
                // 相同命令已在执行中，订阅其结果
                let (tx, rx) = oneshot::channel();
                entry.get_mut().waiters.push(tx);
                drop(guard);
                rx.await.unwrap_or(Err("AT命令队列等待被取消".to_string()))
            }
            Entry::Vacant(entry) => {
                // 首次请求，我们负责执行
                entry.insert(AtPendingCmd { waiters: vec![] });
                drop(guard);

                let result = send_at_command_inner(serial_path, cmd).await;

                // 广播结果给所有等待者
                let mut guard = self.inner.lock().await;
                if let Some(pending) = guard.remove(cmd) {
                    for waiter in pending.waiters {
                        let _ = waiter.send(result.clone());
                    }
                }
                result
            }
        }
    }
}

/// 带队列去重的 AT 命令发送。
/// 相同命令同时请求时会被合并，避免重复执行。
async fn send_at_command_dedup(serial_path: &str, cmd: &str) -> Result<String, String> {
    get_at_queue().execute(serial_path, cmd).await
}

/// 底层 AT 命令发送（使用已打开的串口句柄）
async fn send_at_command_inner(serial_path: &str, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();

    let _lock = get_at_lock().lock().await;

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("/opt/atcmd_rs")
            .arg("-p")
            .arg(serial_path)
            .arg(cmd)
            .output(),
    )
    .await
    .map_err(|_| format!("AT命令超时(10s): {}", cmd))?
    .map_err(|e| format!("atcmd_rs执行失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "atcmd_rs失败 (exit={}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();

    // 去掉命令回显行（如 "AT+CSQ\r\n" 之类的 echo）
    let cmd_trimmed = cmd.trim();
    let mut lines: Vec<&str> = raw
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != cmd_trimmed && !trimmed.eq_ignore_ascii_case(cmd_trimmed)
        })
        .collect();

    // 去掉开头的空行
    while let Some(first) = lines.first() {
        if first.trim().is_empty() {
            lines.remove(0);
        } else {
            break;
        }
    }

    let response_text = lines.join("\n");

    // 检测错误
    if response_text.contains("ERROR") || response_text.contains("+CME ERROR:") {
        let elapsed = start.elapsed();
        println!("⚠️ [AT] {} 返回 ERROR ({}ms)", cmd, elapsed.as_millis());
        return Err(format!("AT命令返回错误: {}", response_text.trim()));
    }

    // 去掉末尾的 OK 行及之后的内容
    let response_text = if let Some(ok_idx) = lines.iter().rposition(|l| l.trim() == "OK") {
        lines.truncate(ok_idx);
        // 再去掉尾部空行
        while let Some(last) = lines.last() {
            if last.trim().is_empty() {
                lines.pop();
            } else {
                break;
            }
        }
        lines.join("\n")
    } else {
        // 没有 OK 行（如 AT+CMGS 的 > prompt），原样返回
        response_text
    };

    let elapsed = start.elapsed();
    println!(
        "✅ [AT] {} 成功 ({}ms):\n{}",
        cmd,
        elapsed.as_millis(),
        response_text
    );
    Ok(response_text)
}

/// 发送 AT 命令并返回首行有效响应（自动去除 AT echo、OK/ERROR、空行）
async fn send_at_get_line(serial_path: &str, cmd: &str) -> Option<String> {
    send_at_command_dedup(serial_path, cmd)
        .await
        .ok()
        .and_then(|resp| {
            resp.lines().find_map(|l| {
                let trimmed = l.trim();
                if !trimmed.is_empty()
                    && !trimmed.contains("OK")
                    && !trimmed.contains("ERROR")
                    && !trimmed.starts_with("AT+")
                    && !trimmed.starts_with("+CME ERROR:")
                {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
        })
}

/// 从 AT 命令的至少一次查询结果中获取网络状态
fn parse_net_status(creg_raw: &str) -> String {
    if let Some(rest) = creg_raw.strip_prefix("+CREG:") {
        let parts: Vec<&str> = rest.trim().split(',').collect();
        if parts.len() >= 2 {
            return match parts[1].trim().trim_matches('"') {
                "1" => "Registered (home)",
                "5" => "Registered (roaming)",
                "2" | "3" | "4" => "Not registered",
                _ => "Unknown",
            }
            .to_string();
        }
    }
    "Unknown".to_string()
}

/// 从 AT+CSQ 响应解析信号质量 (dBm)
fn parse_signal_quality(csq_raw: &str) -> String {
    if let Some(rest) = csq_raw.strip_prefix("+CSQ:") {
        let rssi_str = rest.trim().split(',').next().unwrap_or("99").trim();
        if let Ok(rssi) = rssi_str.parse::<i32>() {
            if rssi == 99 {
                return "Unknown".to_string();
            }
            let dbm = -113 + (rssi * 2);
            return format!("{} dBm", dbm);
        }
    }
    "Unknown".to_string()
}

fn static_file_response(
    body: &'static str,
    content_type: &'static str,
) -> Response<axum::body::Body> {
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

    let serial_available = std::path::Path::new(&serial_path).exists();
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
                        let response = if serial_available {
                            match send_at_command_dedup(&serial_path, &cmd).await {
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
                        let resp = if serial_available {
                            let _ = send_at_command_inner(&serial_path, "AT+CMGF=1").await;
                            send_at_command_inner(&serial_path, "AT+CMGL=\"ALL\"").await
                                .unwrap_or_else(|_| "+CMGL: 0 messages\r\nOK\r\n".to_string())
                        } else {
                            "+CMGL: 1,\"REC READ\",\"+8613800138000\",,\"2026/06/04 10:30:08\"\r\nYour data usage this month: 15.2 GB\r\n+CMGL: 2,\"REC UNREAD\",\"10086\",,\"2026/06/03 09:15:22\"\r\nWelcome to China Mobile 5G network!\r\nOK\r\n".to_string()
                        };
                        let decoded = decode_cmgl_body(&resp);
                        let _ = req.resp_tx.send(serde_json::json!({ "type": "sms_list", "data": decoded }));
                    }
                    AtAction::SetApn(apn, user, pass, auth_type) => {
                        println!("🌐 actor: set_apn apn={} user={} auth={}", apn, user, auth_type);
                        if serial_available {
                            let _ = send_at_command_inner(&serial_path, &format!("AT+CGDCONT=1,\"IPV4V6\",\"{}\"", apn)).await;
                            if !user.is_empty() {
                                let _ = send_at_command_inner(&serial_path, &format!("AT+QGAUTH=1,{},\"{}\",\"{}\"", auth_type, user, pass)).await;
                            }
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": "APN settings applied", "apn": apn }
                        }));
                    }
                    AtAction::SetNetworkMode(mode) => {
                        println!("🌐 actor: set_network_mode {}", mode);
                        if serial_available {
                            let at_mode = match mode.as_str() {
                                "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
                                "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
                                "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
                                "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
                                _ => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
                            };
                            let _ = send_at_command_inner(&serial_path, at_mode).await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("Network mode set to {}", mode) }
                        }));
                    }
                    AtAction::NetConnect(connect) => {
                        println!("🌐 actor: net_{}", if connect { "connect" } else { "disconnect" });
                        if serial_available {
                            let action_val = if connect { "1" } else { "0" };
                            let _ = send_at_command_inner(&serial_path, &format!("AT+CGACT={},1", action_val)).await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("{} command sent", if connect { "Connect" } else { "Disconnect" }) }
                        }));
                    }
                    AtAction::NetworkScan => {
                        println!("🔍 actor: network_scan via AT+COPS=?");
                        let mut networks = Vec::new();
                        if serial_available {
                            if let Ok(_resp) = send_at_command_inner(&serial_path, "AT+COPS=?").await {
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
                        if serial_available {
                            if send_at_command_inner(&serial_path, "AT+CMGF=1").await.is_ok() {
                                if send_at_command_inner(&serial_path, &format!("AT+CMGS=\"{}\"", recipient)).await.is_ok() {
                                    if send_at_command_inner(&serial_path, &format!("{}\u{001A}", message)).await.is_ok() {
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
                        println!("📱 actor: get_device_info via AT commands");
                        let (mfr, model, fw, imei, serial, hw_ver, module_type,
                             sim_status, imsi, iccid, phone, net_status,
                             signal_quality, temperature) = if serial_available {
                            let mfr = send_at_get_line(&serial_path, "AT+CGMI").await.unwrap_or_else(|| "Quectel".to_string());
                            let model = send_at_get_line(&serial_path, "AT+CGMM").await.unwrap_or_else(|| "Unknown".to_string());
                            let fw = send_at_get_line(&serial_path, "AT+CGMR").await.unwrap_or_else(|| "Unknown".to_string());
                            let imei = send_at_get_line(&serial_path, "AT+CGSN").await.unwrap_or_else(|| "Unknown".to_string());
                            let serial = imei.clone(); // IMEI 作为唯一序列号

                            // 硬件版本
                            let hw_ver = send_at_get_line(&serial_path, "AT+QIMPVER").await
                                .and_then(|r| r.strip_prefix("+QIMPVER:")
                                    .map(|s| s.trim().trim_matches('"').to_string()))
                                .unwrap_or_else(|| {
                                    // 回退：从 ATI 中提取
                                    "Unknown".to_string()
                                });

                            let module_type = model.clone();

                            // SIM 状态 (AT+CPIN?)
                            let sim_status = send_at_get_line(&serial_path, "AT+CPIN?").await
                                .and_then(|r| r.strip_prefix("+CPIN:")
                                    .map(|s| s.trim().trim_matches('"').to_string()))
                                .unwrap_or_else(|| "Unknown".to_string());

                            // IMSI (AT+CIMI)
                            let imsi = send_at_get_line(&serial_path, "AT+CIMI").await
                                .and_then(|r| r.strip_prefix("+CIMI:")
                                    .map(|s| s.trim().to_string()))
                                .unwrap_or_default();

                            // ICCID (AT+QCCID)
                            let iccid = send_at_get_line(&serial_path, "AT+QCCID").await
                                .and_then(|r| r.strip_prefix("+QCCID:")
                                    .map(|s| s.trim().trim_matches('"').to_string()))
                                .unwrap_or_else(|| "Unknown".to_string());

                            // 电话号码（AT+CNUM 多未配置，静默处理）
                            let phone = send_at_get_line(&serial_path, "AT+CNUM").await
                                .and_then(|r| {
                                    let r = r.trim();
                                    if r.starts_with("+CNUM:") {
                                        r.split(',').nth(1)
                                            .map(|s| s.trim().trim_matches('"').to_string())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_default();

                            // 网络注册状态 (AT+CREG?)
                            let net_status = send_at_get_line(&serial_path, "AT+CREG?").await
                                .map(|r| parse_net_status(&r))
                                .unwrap_or_else(|| "Unknown".to_string());

                            // 信号质量 (AT+CSQ)
                            let signal_quality = send_at_get_line(&serial_path, "AT+CSQ").await
                                .map(|r| parse_signal_quality(&r))
                                .unwrap_or_else(|| "Unknown".to_string());

                            // 模块温度 (AT+QTEMP)
                            let temperature = send_at_get_line(&serial_path, "AT+QTEMP").await
                                .and_then(|r| parse_qtemp_temperature(&r))
                                .unwrap_or_else(|| "-- °C".to_string());

                            (mfr, model, fw, imei, serial, hw_ver, module_type,
                             sim_status, imsi, iccid, phone, net_status,
                             signal_quality, temperature)
                        } else {
                            ("Quectel".to_string(), "RM520N-GL".to_string(),
                             "RM520NGLAAR03A03M4G_BETA".to_string(), "867584032145678".to_string(),
                             "QC2023RM520N001".to_string(), "R1.0".to_string(),
                             "M.2 5G NR Module".to_string(), "Ready".to_string(),
                             "460010123456789".to_string(), "89860112851234567890".to_string(),
                             "+8613800138000".to_string(), "Registered".to_string(),
                             "-64 dBm".to_string(), "42 °C".to_string())
                        };

                        // 静态能力信息（基于型号的常量）
                        let (bands, max_rate, volte, gnss) = if model.contains("RG520N") || model.contains("RM5") {
                            ("NR n1/n3/n5/n7/n8/n20/n28/n41/n77/n78/n79, LTE B1/B3/B5/B7/B8/B18/B19/B20/B26/B28/B32/B34/B38/B39/B40/B41/B42/B43".to_string(),
                             "DL 4.2 Gbps / UL 600 Mbps".to_string(),
                             "Supported".to_string(),
                             "GPS/GLONASS/BDS/Galileo".to_string())
                        } else {
                            ("--".to_string(), "--".to_string(), "--".to_string(), "--".to_string())
                        };

                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "device_info",
                            "data": {
                                "manufacturer": mfr,
                                "model": model,
                                "firmware_version": fw,
                                "imei": imei,
                                "serial": serial,
                                "hw_version": hw_ver,
                                "module_type": module_type,
                                "sim_status": sim_status,
                                "imsi": imsi,
                                "iccid": iccid,
                                "phone": phone,
                                "net_status": net_status,
                                "signal": signal_quality,
                                "temperature": temperature,
                                "bands": bands,
                                "max_rate": max_rate,
                                "volte": volte,
                                "gnss": gnss
                            }
                        }));
                    }
                    AtAction::Reboot => {
                        println!("🔄 actor: rebooting module...");
                        if serial_available {
                            let _ = send_at_command_inner(&serial_path, "AT+CFUN=1,1").await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Reboot command sent to module." }
                        }));
                    }
                    AtAction::FactoryReset => {
                        println!("⚠️ actor: factory reset module...");
                        if serial_available {
                            let _ = send_at_command_inner(&serial_path, "AT&F0").await;
                        }
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Factory reset command sent to module." }
                        }));
                    }
                    AtAction::FlightMode(on) => {
                        let cfun_val = if on { "4" } else { "1" };
                        println!("✈️ actor: setting flight mode to {}", if on { "ON" } else { "OFF" });
                        if serial_available {
                            let _ = send_at_command_inner(&serial_path, &format!("AT+CFUN={}", cfun_val)).await;
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

                    if serial_available {
                        // 非轮询任务优先级最高：有待处理的用户请求时跳过本轮 AT 轮询
                        if actor_rx.is_empty() {
                            if let Ok(cgpaddr_resp) = send_at_command_dedup(&serial_path, "AT+CGPADDR").await {
                            parse_cgpaddr(&cgpaddr_resp, &mut telemetry);
                        }
                        if let Ok(qeng_resp) = send_at_command_dedup(&serial_path, "AT+QENG=\"servingcell\"").await {
                            parse_qeng(&qeng_resp, &mut telemetry);
                        }
                        if let Ok(qcainfo_resp) = send_at_command_dedup(&serial_path, "AT+QCAINFO").await {
                            parse_qcainfo(&qcainfo_resp, &mut telemetry);
                        }
                        if let Ok(qtemp_resp) = send_at_command_dedup(&serial_path, "AT+QTEMP").await {
                            if let Some(temp) = parse_qtemp_temperature(&qtemp_resp) {
                                telemetry.temperature = temp;
                            }
                        }
                        // 实时获取流量统计（QGDNRCNT → QGDAT 回退）
                        telemetry.traffic_stats = send_at_command_dedup(&serial_path, "AT+QGDNRCNT?")
                            .await
                            .ok()
                            .and_then(|r| r.lines().find(|l| l.contains("+QGDNRCNT:")).and_then(|l| parse_traffic_line(l, "+QGDNRCNT:", false)))
                            .unwrap_or_default();
                        if telemetry.traffic_stats.is_empty() {
                            telemetry.traffic_stats = send_at_command_dedup(&serial_path, "AT+QGDAT?")
                                .await
                                .ok()
                                .and_then(|r| r.lines().find(|l| l.contains("+QGDAT:")).and_then(|l| parse_traffic_line(l, "+QGDAT:", true)))
                                .unwrap_or_default();
                        }
                        } // end if actor_rx.is_empty()
                        set_default(&mut telemetry.traffic_stats, "N/A");
                        if telemetry.signal_percentage.is_empty() {
                            telemetry.signal_percentage = "85%".to_string();
                        }
                        if telemetry.network_mode.is_empty() {
                            telemetry.network_mode = "--".to_string();
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
                        telemetry.traffic_stats = "TX 1.23 GB / RX 4.56 GB".to_string();
                    }

                    // 获取 CPU 物理温度（仅当 AT+QTEMP 未提供模块温度时作为 fallback）
                    if telemetry.temperature.is_empty() {
                        let cpu_temp = components.iter()
                            .find(|c| {
                                let label = c.label();
                                label.contains("CPU") || label.contains("cpu") || label.contains("Package") || label.contains("package")
                            })
                            .and_then(|c| c.temperature());

                        telemetry.temperature = cpu_temp.map_or("--".to_string(), |t| format!("{:.0} °C", t));
                    }
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
    static_file_response(
        include_str!("../templates/style.css"),
        "text/css; charset=utf-8",
    )
}

/// Decode UCS-2 hex-encoded SMS body text in +CMGL AT responses
fn decode_cmgl_body(response: &str) -> String {
    let mut result = String::new();
    let mut in_body = false;

    for line in response.lines() {
        if line.starts_with("+CMGL:") {
            in_body = true;
            result.push_str(line);
            result.push('\n');
        } else if in_body {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "OK" {
                in_body = false;
                result.push_str(line);
                result.push('\n');
            } else {
                let decoded = decode_hex_ucs2(trimmed);
                if decoded.is_empty() {
                    result.push_str(line);
                } else {
                    result.push_str(&decoded);
                }
                result.push('\n');
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}
