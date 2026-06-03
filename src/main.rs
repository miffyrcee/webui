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
use tokio::time::sleep;

use at::parser::{parse_cgpaddr, parse_combined_response, parse_qcainfo, parse_qeng};
use at::utils::{decode_hex_ucs2, format_bytes};

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";

/// 单次 I/O 超时（用于 tokio::time::timeout 包裹单个 read/write 操作）
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// 串口句柄：tokio::fs::File（阻塞 I/O 在 spawn_blocking 线程池执行）
enum SerialHandle {
    Raw(tokio::fs::File),
}

impl SerialHandle {
    /// 阻塞写入（tokio::fs::File::write_all 内部用 spawn_blocking，外层加 timeout）
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), String> {
        match self {
            SerialHandle::Raw(f) => {
                tokio::time::timeout(IO_TIMEOUT, f.write_all(buf))
                    .await
                    .map_err(|_| format!("写超时({}ms)", IO_TIMEOUT.as_millis()))?
                    .map_err(|e| format!("写失败: {}", e))
            }
        }
    }

    /// 阻塞 flush
    async fn flush(&mut self) -> Result<(), String> {
        match self {
            SerialHandle::Raw(f) => {
                tokio::time::timeout(IO_TIMEOUT, f.flush())
                    .await
                    .map_err(|_| format!("flush超时({}ms)", IO_TIMEOUT.as_millis()))?
                    .map_err(|e| format!("flush失败: {}", e))
            }
        }
    }

    /// 阻塞读取（带超时）
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            SerialHandle::Raw(f) => {
                tokio::time::timeout(IO_TIMEOUT, f.read(buf))
                    .await
                    .map_err(|_| format!("读超时({}ms)", IO_TIMEOUT.as_millis()))?
                    .map_err(|e| format!("读错误: {}", e))
            }
        }
    }
}

#[derive(Debug)]
enum AtAction {
    ManualAt(String),
    SetInterval(u64),
}

struct AtRequest {
    action: AtAction,
    resp_tx: oneshot::Sender<String>,
}

struct AppState {
    tx: broadcast::Sender<String>,
    actor_tx: mpsc::Sender<AtRequest>,
    active_views: AtomicUsize,
    /// 启动时一次性获取的静态信息（在线程安全下共享）
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
        active_views: AtomicUsize::new(0),
        static_info: RwLock::new(StaticInfo::default()),
    });

    // 先获取一次静态信息（带超时保护，避免启动卡死）
    let fetch_result = tokio::time::timeout(
        Duration::from_secs(30),
        fetch_static_info(&app_state, &serial_path),
    )
    .await;
    if fetch_result.is_err() {
        eprintln!("⚠️ fetch_static_info 超时(30s)，使用默认静态信息继续启动");
        let mut guard = app_state.static_info.write().await;
        *guard = StaticInfo {
            firmware_version: "Unknown".to_string(),
            active_sim: "SIM 1".to_string(),
            network_provider: "Unknown".to_string(),
            apn: "N/A".to_string(),
            traffic_stats: "N/A".to_string(),
        };
    }

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

/// 清空串口缓冲区：读取并丢弃任何残留数据（非阻塞，短超时）
async fn drain_serial_buffer(serial: &mut SerialHandle) {
    let mut buf = [0u8; 1024];
    let mut total_drained = 0u32;
    let mut first_chunk = String::new();
    loop {
        // 用短超时读取缓冲区残留（300ms）
        let result = tokio::time::timeout(Duration::from_millis(300), serial.read(&mut buf)).await;
        match result {
            Ok(Ok(n)) if n > 0 => {
                total_drained += n as u32;
                if first_chunk.is_empty() {
                    let readable: String = buf[..n]
                        .iter()
                        .map(|&b| if b.is_ascii_graphic() || b == b' ' || b == b'\n' || b == b'\r' { b as char } else { '.' })
                        .collect();
                    first_chunk = readable;
                }
            }
            _ => break,
        }
        if total_drained > 8192 {
            break;
        }
    }
    if total_drained > 0 {
        println!("🔄 清空了串口缓冲区: {} 字节残留数据 (首段: {:?})", total_drained, first_chunk);
    }
}

/// 尝试打开串口，返回 Some(SerialHandle) 或 None（模拟模式）
async fn open_serial(path: &str) -> Option<SerialHandle> {
    println!("🔌 正在尝试打开串口设备: {} ...", path);
    let start = std::time::Instant::now();
    // tokio::fs::File 内部使用 spawn_blocking，SMD 设备必须用阻塞 I/O
    match tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await
    {
        Ok(file) => {
            println!("✅ 串口已打开(阻塞Raw文件模式): {} (耗时: {}ms)", path, start.elapsed().as_millis());
            Some(SerialHandle::Raw(file))
        }
        Err(e) => {
            eprintln!("❌ 打开串口失败: {} (路径: {}, 耗时: {}ms)，将运行在模拟模式", e, path, start.elapsed().as_millis());
            None
        }
    }
}

/// 启动时一次性获取静态信息：固件版本、SIM槽位、运营商、APN
async fn fetch_static_info(state: &Arc<AppState>, serial_path: &str) {
    println!("📋 开始获取静态信息（此过程会短暂占用串口）...");
    let mut info = StaticInfo::default();

    let mut serial = match open_serial(serial_path).await {
        Some(s) => s,
        None => {
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
        }
    };

    // SMD 通道初始化：清空残留 + 关闭回显 + 验证通信
    drain_serial_buffer(&mut serial).await;
    let _ = send_at_command_async(&mut serial, "ATE0").await;
    let _ = send_at_command_async(&mut serial, "AT").await;

    // 固件版本
    println!("📋 [1/5] 获取固件版本 (AT+CGMR)...");
    match send_at_command_async(&mut serial, "AT+CGMR").await {
        Ok(resp) => {
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
        Err(e) => eprintln!("⚠️ 获取固件版本失败: {}", e),
    }
    if info.firmware_version.is_empty() {
        info.firmware_version = "Unknown".to_string();
    }

    // SIM 槽位
    println!("📋 [2/5] 获取 SIM 槽位 (AT+QUIMSLOT?)...");
    match send_at_command_async(&mut serial, "AT+QUIMSLOT?").await {
        Ok(resp) => {
            for line in resp.lines() {
                if line.contains("+QUIMSLOT:") {
                    let parts: Vec<&str> = line.split(':').collect();
                    if parts.len() >= 2 {
                        info.active_sim = format!("SIM {}", parts[1].trim());
                    }
                }
            }
        }
        Err(e) => eprintln!("⚠️ 获取SIM槽位失败: {}", e),
    }
    if info.active_sim.is_empty() {
        info.active_sim = "SIM 1".to_string();
    }

    // 运营商 — 先尝试 QSPN（中文名），再 fallback COPS
    println!("📋 [3/5] 获取运营商 (AT+QSPN / AT+COPS?)...");
    match send_at_command_async(&mut serial, "AT+QSPN").await {
        Ok(resp) => {
            for line in resp.lines() {
                if line.contains("+QSPN:") {
                    let after_prefix = line.trim().strip_prefix("+QSPN:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if !parts.is_empty() {
                        let raw = parts[0].trim().trim_matches('"').trim();
                        if !raw.is_empty() && raw != "????" {
                            let decoded = decode_hex_ucs2(raw);
                            info.network_provider = if decoded.is_empty() {
                                raw.to_string()
                            } else {
                                decoded
                            };
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("⚠️ QSPN 获取失败: {}", e),
    }
    if info.network_provider.is_empty() {
        match send_at_command_async(&mut serial, "AT+COPS?").await {
            Ok(resp) => {
                for line in resp.lines() {
                    if line.contains("+COPS:") {
                        let after_prefix =
                            line.trim().strip_prefix("+COPS:").unwrap_or(line).trim();
                        let parts: Vec<&str> = after_prefix.split(',').collect();
                        if parts.len() >= 2 {
                            let raw = parts[1].trim().trim_matches('"').trim();
                            if !raw.is_empty() && raw != "????" {
                                let decoded = decode_hex_ucs2(raw);
                                info.network_provider = if decoded.is_empty() {
                                    raw.to_string()
                                } else {
                                    decoded
                                };
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("⚠️ COPS 获取失败: {}", e),
        }
    }
    if info.network_provider.is_empty() {
        // 最后尝试从 MCCMNC 映射
        match send_at_command_async(&mut serial, "AT+QENG=\"servingcell\"").await {
            Ok(resp) => {
                for line in resp.lines() {
                    if line.contains("+QENG: \"servingcell\"") {
                        let parts: Vec<&str> = line
                            .split(',')
                            .map(|s| s.trim().trim_matches('"'))
                            .collect();
                        if parts.len() >= 6 {
                            let mccmnc = format!("{}{}", parts[4], parts[5]);
                            info.network_provider = match mccmnc.as_str() {
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
            Err(e) => eprintln!("⚠️ MCCMNC 回退获取失败: {}", e),
        }
    }
    if info.network_provider.is_empty() || info.network_provider == "????" {
        info.network_provider = "Unknown".to_string();
    }

    // APN (CGDCONT 第一个上下文)
    println!("📋 [4/5] 获取 APN (AT+CGDCONT?)...");
    match send_at_command_async(&mut serial, "AT+CGDCONT?").await {
        Ok(resp) => {
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
        Err(e) => eprintln!("⚠️ 获取APN失败: {}", e),
    }
    if info.apn.is_empty() {
        info.apn = "N/A".to_string();
    }

    // 流量统计 — 尝试 QGDNRCNT 或 QGDAT
    println!("📋 [5/5] 获取流量统计 (AT+QGDNRCNT?)...");
    info.traffic_stats = send_at_command_async(&mut serial, "AT+QGDNRCNT?")
        .await
        .ok()
        .and_then(|r| {
            r.lines()
                .find(|l| l.contains("+QGDNRCNT:"))
                .and_then(|line| {
                    let parts: Vec<&str> = line
                        .trim()
                        .strip_prefix("+QGDNRCNT:")?
                        .trim()
                        .split(',')
                        .collect();
                    if parts.len() >= 2 {
                        let tx: u64 = parts[0].trim().parse().unwrap_or(0);
                        let rx: u64 = parts[1].trim().parse().unwrap_or(0);
                        Some(format!("TX {} / RX {}", format_bytes(tx), format_bytes(rx)))
                    } else {
                        None
                    }
                })
        })
        .unwrap_or_default();
    if info.traffic_stats.is_empty() {
        match send_at_command_async(&mut serial, "AT+QGDAT?").await {
            Ok(resp) => {
                for line in resp.lines() {
                    if line.contains("+QGDAT:") {
                        let parts: Vec<&str> = line
                            .trim()
                            .strip_prefix("+QGDAT:")
                            .unwrap_or(line)
                            .trim()
                            .split(',')
                            .collect();
                        if parts.len() >= 2 {
                            let tx: u64 = parts[0].trim().trim_matches('"').parse().unwrap_or(0);
                            let rx: u64 = parts[1].trim().trim_matches('"').parse().unwrap_or(0);
                            info.traffic_stats =
                                format!("TX {} / RX {}", format_bytes(tx), format_bytes(rx));
                        }
                        break;
                    }
                }
            }
            Err(e) => eprintln!("⚠️ QGDAT 获取失败: {}", e),
        }
    }
    if info.traffic_stats.is_empty() {
        info.traffic_stats = "N/A".to_string();
    }

    println!(
        "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}, Traffic={}",
        info.firmware_version, info.active_sim, info.network_provider, info.apn, info.traffic_stats
    );

    // 完成后关闭串口（drop serial），hardware_polling_actor 会重新打开
    drop(serial);
    println!("📋 静态信息串口已关闭，hardware_polling_actor 将重新打开");

    let mut guard = state.static_info.write().await;
    *guard = info;
}

/// 异步发送 AT 命令并读取响应
async fn send_at_command_async(serial: &mut SerialHandle, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();
    let full_cmd = format!("{}\r\n", cmd);

    serial
        .write_all(full_cmd.as_bytes())
        .await
        .map_err(|e| format!("写失败: {}", e))?;
    serial
        .flush()
        .await
        .map_err(|e| format!("flush失败: {}", e))?;

    // SMD 通道需要短暂等待模块处理 AT 命令
    sleep(Duration::from_millis(200)).await;

    // 总超时 5 秒（防止单个 AT 命令永久卡死）
    let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    let mut buf = [0u8; 1024];
    let mut response_bytes = Vec::with_capacity(4096);

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
                // Raw模式：0字节读取不代表EOF，只是暂无数据
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
                let len = response_bytes.len();
                if len >= 6 && response_bytes[len - 6..] == *b"\r\nOK\r\n" {
                    break;
                }
                if len >= 9 && response_bytes[len - 9..] == *b"\r\nERROR\r\n" {
                    break;
                }
                // 检查更长的buffer尾部（如果一次性读入了多个OK/ERROR中间有数据）
                if len >= 15 {
                    let tail = &response_bytes[len - 15..];
                    if tail.windows(6).any(|w| w == b"\r\nOK\r\n")
                        || tail.windows(9).any(|w| w == b"\r\nERROR\r\n")
                    {
                        break;
                    }
                }
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

async fn index_handler() -> impl IntoResponse {
    let html = include_str!("index.html");
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .unwrap()
}

async fn hardware_polling_actor(
    state: Arc<AppState>,
    mut actor_rx: mpsc::Receiver<AtRequest>,
    serial_path: String,
) {
    let mut sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();

    // 打开串口（独立连接，与 fetch_static_info 互不影响）
    let mut serial = open_serial(&serial_path).await;

    // SMD 通道初始化：清空残留 + 关闭回显 + 验证通信
    if let Some(ref mut port) = serial {
        drain_serial_buffer(port).await;
        let _ = send_at_command_async(port, "ATE0").await;
        let _ = send_at_command_async(port, "AT").await;
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
                        let response = if let Some(ref mut port) = serial {
                            match send_at_command_async(port, &cmd).await {
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
                let active_count = state.active_views.load(Ordering::SeqCst);
                if active_count > 0 {
                    let start_poll = std::time::Instant::now();
                    println!("📡 开始周期性硬件轮询 (活跃视图: {})", active_count);

                    let (cpin_res, qeng_res, _qca_res, gpad_res) = if let Some(ref mut port) = serial {
                        let combined = "AT+CPIN?;+QENG=\"servingcell\";+QCAINFO;+CGPADDR";
                        eprintln!("📡 发送合并轮询命令 ({}ms 间隔)...", interval_secs);
                        let poll_cmd_start = std::time::Instant::now();
                        match send_at_command_async(port, combined).await {
                            Ok(resp) => {
                                let parsed = parse_combined_response(&resp);
                                eprintln!("📡 合并命令解析完成 ({}ms)，各段长度: CPIN={}, QENG={}, QCA={}, GPAD={}",
                                    poll_cmd_start.elapsed().as_millis(),
                                    parsed.0.len(), parsed.1.len(), parsed.2.len(), parsed.3.len());
                                parsed
                            }
                            Err(e) => {
                                println!("⚠️ 合并命令失败 ({}ms): {}，降级到单独发送", poll_cmd_start.elapsed().as_millis(), e);
                                let cpin = send_at_command_async(port, "AT+CPIN?").await.unwrap_or_else(|e| {
                                    eprintln!("❌ AT+CPIN? 降级也失败: {}", e);
                                    String::new()
                                });
                                let qeng = send_at_command_async(port, "AT+QENG=\"servingcell\"").await.unwrap_or_else(|e| {
                                    eprintln!("❌ AT+QENG 降级也失败: {}", e);
                                    String::new()
                                });
                                let qca = send_at_command_async(port, "AT+QCAINFO").await.unwrap_or_else(|e| {
                                    eprintln!("❌ AT+QCAINFO 降级也失败: {}", e);
                                    String::new()
                                });
                                let gpad = send_at_command_async(port, "AT+CGPADDR").await.unwrap_or_else(|e| {
                                    eprintln!("❌ AT+CGPADDR 降级也失败: {}", e);
                                    String::new()
                                });
                                (cpin, qeng, qca, gpad)
                            }
                        }
                    } else {
                        sleep(Duration::from_millis(50)).await;
                        (
                            "+CPIN: READY\r\nOK\r\n".to_string(),
                            "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-64,-11,22,1,-\r\nOK\r\n".to_string(),
                            "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\r\nOK\r\n".to_string(),
                            "+CGPADDR: 1,\"10.202.165.254\",\"2409::1\"\r\nOK\r\n".to_string(),
                        )
                    };

                    let sim_status = if cpin_res.contains("READY") { "Active" } else { "No Card / Locked" };
                    eprintln!("📡 SIM 状态: {}", sim_status);

                    let mut telemetry = TelemetryData::default();
                    telemetry.sim_status = sim_status.to_string();

                    // 从静态信息中读取（只获取一次的数据）
                    {
                        let guard = state.static_info.read().await;
                        telemetry.active_sim = guard.active_sim.clone();
                        telemetry.network_provider = guard.network_provider.clone();
                        telemetry.apn = guard.apn.clone();
                        telemetry.traffic_stats = guard.traffic_stats.clone();
                        eprintln!("📡 静态信息: SIM={}, Provider={}, APN={}, Traffic={}",
                            guard.active_sim, guard.network_provider, guard.apn, guard.traffic_stats);
                    }

                    let parse_start = std::time::Instant::now();
                    parse_qeng(&qeng_res, &mut telemetry);
                    parse_qcainfo(&_qca_res, &mut telemetry);
                    parse_cgpaddr(&gpad_res, &mut telemetry);
                    eprintln!("📡 解析完成 ({}ms)", parse_start.elapsed().as_millis());

                    sys.refresh_cpu_all();
                    components.refresh(false);
                    let cpu_temp = components.iter()
                        .find(|c| {
                            let label = c.label();
                            label.contains("CPU") || label.contains("cpu") || label.contains("Package") || label.contains("package")
                        })
                        .map_or(46.0, |c| c.temperature().unwrap_or(46.0));

                    telemetry.temperature = format!("{:.0} °C", cpu_temp);
                    telemetry.internet_connection = if telemetry.ipv4 != "--" { "Connected".to_string() } else { "Disconnected".to_string() };

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
    println!("🔌 新的 WebSocket 客户端已连接 (当前在线: {})", online_count);

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
                            let payload = cmd.payload.unwrap_or_default();
                            println!("👨‍💻 WS 收到手动 AT 命令: {}", payload);
                            AtAction::ManualAt(payload)
                        }
                        "set_interval" => {
                            let secs = cmd.payload.and_then(|p| p.parse().ok()).unwrap_or(3);
                            println!("🔄 WS 收到间隔调整: {} 秒", secs);
                            AtAction::SetInterval(secs)
                        }
                        "set_view_state" => {
                            let is_active = cmd.payload.as_deref() == Some("active");
                            eprintln!("📡 WS 视图状态变更: active={} (当前={})", is_active, current_is_active);
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
                            // 返回静态信息给前端
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
                            eprintln!("📤 WS 回复静态信息: FW={}, SIM={}",
                                guard.firmware_version, guard.active_sim);
                            let _ = local_tx.send(info_json.to_string()).await;
                            continue;
                        }
                        unknown_action => {
                            eprintln!("⚠️ 未知 WebSocket 动作: {:?} (payload: {:?})", unknown_action, cmd.payload);
                            continue;
                        }
                    };
                    let _ = state_inner
                        .actor_tx
                        .send(AtRequest { action, resp_tx })
                        .await;

                    if let Ok(reply) = resp_rx.await {
                        let result_json = serde_json::json!({ "type": "at_res", "data": reply });
                        if local_tx.send(result_json.to_string()).await.is_err() {
                            break;
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
        " WebSocket 客户端已断开 (当前在线: {})",
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