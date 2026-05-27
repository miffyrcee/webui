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
use nom::{
    IResult,
    Parser,
    bytes::complete::{tag, take_while},
    character::complete::{char, digit1},
    sequence::delimited,
    branch::alt,
};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};
use tokio::time::sleep;

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

    // 先获取一次静态信息
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

/// 启动时一次性获取静态信息：固件版本、SIM槽位、运营商、APN
async fn fetch_static_info(state: &Arc<AppState>, serial_path: &str) {
    let mut info = StaticInfo::default();

    let mut serial_file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(serial_path)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("❌ 打开串口失败: {}，使用模拟静态数据", e);
            info.firmware_version = "RM520NGLAAR03A03M4G_BETA".to_string();
            info.active_sim = "SIM 1".to_string();
            info.network_provider = "CHN-UNICOM".to_string();
            info.apn = "3gnet".to_string();
            info.traffic_stats = "N/A".to_string();
            let mut guard = state.static_info.write().await;
            *guard = info;
            return;
        }
    };

    // 固件版本
    match send_at_command_async(&mut serial_file, "AT+CGMR").await {
        Ok(resp) => {
            for line in resp.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.contains("OK") && !trimmed.contains("ERROR") && !trimmed.starts_with("AT+") {
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
    match send_at_command_async(&mut serial_file, "AT+QUIMSLOT?").await {
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
    match send_at_command_async(&mut serial_file, "AT+QSPN").await {
        Ok(resp) => {
            for line in resp.lines() {
                if line.contains("+QSPN:") {
                    // +QSPN: <FNN>,<SNN>,<SPN>,<Alphabet>
                    // 先去掉 "+QSPN: " 前缀，再按逗号分割
                    let after_prefix = line.trim().strip_prefix("+QSPN:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if !parts.is_empty() {
                        let raw = parts[0].trim().trim_matches('"').trim();
                        if !raw.is_empty() && raw != "????" {
                            // 尝试 hex 解码（移远部分模块返回 UCS2 十六进制编码）
                            let decoded = decode_hex_ucs2(raw);
                            info.network_provider = if decoded.is_empty() { raw.to_string() } else { decoded };
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("⚠️ QSPN 获取失败: {}", e),
    }
    if info.network_provider.is_empty() {
        match send_at_command_async(&mut serial_file, "AT+COPS?").await {
            Ok(resp) => {
                for line in resp.lines() {
                    if line.contains("+COPS:") {
                        // +COPS: <mode>[,<format>,<oper>[,<Act>]]
                        let after_prefix = line.trim().strip_prefix("+COPS:").unwrap_or(line).trim();
                        let parts: Vec<&str> = after_prefix.split(',').collect();
                        if parts.len() >= 2 {
                            let raw = parts[1].trim().trim_matches('"').trim();
                            if !raw.is_empty() && raw != "????" {
                                let decoded = decode_hex_ucs2(raw);
                                info.network_provider = if decoded.is_empty() { raw.to_string() } else { decoded };
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
        match send_at_command_async(&mut serial_file, "AT+QENG=\"servingcell\"").await {
            Ok(resp) => {
                for line in resp.lines() {
                    if line.contains("+QENG: \"servingcell\"") {
                        let parts: Vec<&str> = line.split(',').map(|s| s.trim().trim_matches('"')).collect();
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
    match send_at_command_async(&mut serial_file, "AT+CGDCONT?").await {
        Ok(resp) => {
            for line in resp.lines() {
                if line.contains("+CGDCONT: 1,") {
                    let after_prefix = line.trim().strip_prefix("+CGDCONT: 1,").unwrap_or(line).trim();
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

    // 流量统计 — AT+QGDAT? (或 QGDCNT)
    match send_at_command_async(&mut serial_file, "AT+QGDAT?").await {
        Ok(resp) => {
            for line in resp.lines() {
                if line.contains("+QGDAT:") {
                    // +QGDAT: <tx_bytes>,<rx_bytes>,<tx_packets>,<rx_packets>
                    let after_prefix = line.trim().strip_prefix("+QGDAT:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if parts.len() >= 2 {
                        let tx: u64 = parts[0].trim().trim_matches('"').parse().unwrap_or(0);
                        let rx: u64 = parts[1].trim().trim_matches('"').parse().unwrap_or(0);
                        info.traffic_stats = format!("TX {} / RX {}", format_bytes(tx), format_bytes(rx));
                    }
                    break;
                }
            }
        }
        Err(e) => eprintln!("⚠️ QGDAT 获取失败: {}", e),
    }
    if info.traffic_stats.is_empty() {
        info.traffic_stats = "N/A".to_string();
    }

    println!(
        "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}",
        info.firmware_version, info.active_sim, info.network_provider, info.apn
    );

    let mut guard = state.static_info.write().await;
    *guard = info;
}

/// 将 UCS2 十六进制编码字符串解码为 UTF-8
/// 例如 "4E2D56FD79FB52A8" -> "中国联通"
fn decode_hex_ucs2(hex: &str) -> String {
    if hex.len() % 4 != 0 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    let mut bytes = Vec::new();
    for i in (0..hex.len()).step_by(4) {
        if let Ok(code) = u16::from_str_radix(&hex[i..i + 4], 16) {
            match code {
                0x0000..=0x007F => bytes.push(code as u8),
                0x0080..=0x07FF => {
                    bytes.push(0xC0 | (code >> 6) as u8);
                    bytes.push(0x80 | (code & 0x3F) as u8);
                }
                _ => {
                    bytes.push(0xE0 | (code >> 12) as u8);
                    bytes.push(0x80 | ((code >> 6) & 0x3F) as u8);
                    bytes.push(0x80 | (code & 0x3F) as u8);
                }
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// 格式化字节数为可读字符串 (KB / MB / GB)
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.2} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

async fn index_handler() -> impl IntoResponse {
    let html = include_str!("index.html");
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .unwrap()
}

/// 异步发送 AT 命令并读取响应
async fn send_at_command_async(file: &mut File, cmd: &str) -> Result<String, String> {
    let start = std::time::Instant::now();
    let full_cmd = format!("{}\r\n", cmd);

    file.write_all(full_cmd.as_bytes())
        .await
        .map_err(|e| format!("写失败: {}", e))?;
    file.flush()
        .await
        .map_err(|e| format!("flush失败: {}", e))?;

    let mut buf = [0u8; 1024];
    let mut response_bytes = Vec::with_capacity(4096);

    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                response_bytes.extend_from_slice(&buf[..n]);
                let len = response_bytes.len();
                if len >= 6 && response_bytes[len - 6..] == *b"\r\nOK\r\n" {
                    break;
                }
                if len >= 9 && response_bytes[len - 9..] == *b"\r\nERROR\r\n" {
                    break;
                }
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

/// 解析合并命令的响应，提取各部分
fn parse_combined_response(raw: &str) -> (String, String, String) {
    let mut cpin = String::new();
    let mut qeng = String::new();
    let mut cgpaddr = String::new();

    if let Some(start) = raw.find("+CPIN:") {
        let remaining = &raw[start..];
        let end = remaining
            .find("+QENG:")
            .or_else(|| remaining.find("+CGPADDR:"))
            .unwrap_or(remaining.len());
        let block = &remaining[..end];
        if let Some(ok_pos) = block.find("\r\nOK\r\n") {
            cpin = block[..ok_pos + 6].to_string();
        } else if let Some(err_pos) = block.find("\r\nERROR\r\n") {
            cpin = block[..err_pos + 8].to_string();
        } else {
            cpin = block.to_string();
        }
    }

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
    // 收集所有 servingcell 行（载波聚合时有多行）
    let serving_lines: Vec<Vec<&str>> = qeng_res
        .lines()
        .filter(|l| l.contains("+QENG: \"servingcell\""))
        .map(|line| {
            line.trim()
                .split(',')
                .map(|s| s.trim().trim_matches('"'))
                .collect()
        })
        .collect();

    if serving_lines.is_empty() {
        println!("⚠️ 未找到 +QENG 响应");
        return;
    }

    // 使用第一行（PCC/主载波）作为基础字段
    let pcc = &serving_lines[0];
    if pcc.len() < 15 {
        println!("⚠️ QENG 字段数不足: {} (需要 >=15)", pcc.len());
        return;
    }

    // 基础字段（来自 PCC）
    telemetry.network_mode = format!("{} {}", pcc[2], pcc[3]);
    telemetry.mccmnc = format!("{}{}", pcc[4], pcc[5]);
    telemetry.cell_id = pcc[6].to_string();
    if pcc[6].len() >= 6 {
        telemetry.enb_id = pcc[6][..pcc[6].len() - 3].to_string();
    } else {
        telemetry.enb_id = pcc[6].to_string();
    }
    telemetry.tac = pcc[8].to_string();

    // 信号指标（来自 PCC）
    let rsrp: i32 = pcc[12].parse().unwrap_or(-140);
    let rsrq: i32 = pcc[13].parse().unwrap_or(-20);
    let sinr: i32 = pcc[14].parse().unwrap_or(-20);

    let rsrp_pct = ((rsrp + 140) as f32 / 96.0 * 100.0).clamp(0.0, 100.0) as i32;
    let rsrq_pct = ((rsrq + 20) as f32 / 17.0 * 100.0).clamp(0.0, 100.0) as i32;
    let sinr_pct = ((sinr + 20) as f32 / 50.0 * 100.0).clamp(0.0, 100.0) as i32;

    telemetry.assessment = if rsrp > -80 && sinr > 20 {
        "Excellent"
    } else {
        "Good"
    }
    .to_string();

    telemetry.ss_rsrp = format!("{} / {}%", rsrp, rsrp_pct);
    telemetry.ss_rsrq = format!("{} / {}%", rsrq, rsrq_pct);
    telemetry.sinr = format!("{} / {}%", sinr, sinr_pct);
    telemetry.signal_percentage = format!("{}%", rsrp_pct);

    // 载波聚合字段：合并所有 CC
    if serving_lines.len() > 1 {
        // Bands: 合并去重
        let mut bands: Vec<String> = Vec::new();
        for line in &serving_lines {
            if line.len() >= 11 {
                let band = format!("NR5G BAND {}", line[10]);
                if !bands.contains(&band) {
                    bands.push(band);
                }
            }
        }
        telemetry.bands = bands.join(", ");

        // Bandwidth: 提取主载波带宽，标注类型
        if serving_lines[0].len() >= 12 {
            let bw: i32 = serving_lines[0][11].parse().unwrap_or(0);
            telemetry.bandwidth = format!("NR {} MHz", bw);
        }

        // EARFCN / PCI: 逗号连接
        let earfcns: Vec<&str> = serving_lines.iter()
            .filter_map(|l| if l.len() >= 10 { Some(l[9]) } else { None })
            .collect();
        telemetry.earfcn = earfcns.join(", ");

        let pcis: Vec<&str> = serving_lines.iter()
            .filter_map(|l| if l.len() >= 8 { Some(l[7]) } else { None })
            .collect();
        telemetry.pci = pcis.join(", ");
    } else {
        // 单载波（原逻辑）
        telemetry.bands = format!("NR5G BAND {}", pcc[10]);
        telemetry.bandwidth = format!("{} MHz", pcc[11]);
        telemetry.earfcn = pcc[9].to_string();
        telemetry.pci = pcc[7].to_string();
    }
}

fn parse_cgpaddr(gpad_res: &str, telemetry: &mut TelemetryData) {
    for line in gpad_res.lines() {
        if let Ok((_, (_, ipv4, ipv6))) = parse_cgpaddr_line(line.trim()) {
            if !ipv4.is_empty()
                && ipv4 != "0.0.0.0"
                && is_valid_ipv4(ipv4)
                && telemetry.ipv4.is_empty()
            {
                telemetry.ipv4 = ipv4.to_string();
            }
            if !ipv6.is_empty()
                && ipv6 != "0.0.0.0"
            {
                // 尝试转换为标准IPv6格式（处理点分十进制16字节格式）
                let normalized = convert_dotted_ipv6_to_standard(ipv6);
                if is_valid_ipv6(&normalized) && telemetry.ipv6.is_empty() {
                    telemetry.ipv6 = normalized;
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

/// 将16字节点分十进制格式的IPv6转换为标准冒号十六进制格式
/// 例如: "36.9.137.112.10.181.36.74.24.179.107.247.91.255.29.48"
///    => "2409:8970:ab5:244a:18b3:6bf7:5bff:1d30"
fn convert_dotted_ipv6_to_standard(raw: &str) -> String {
    let bytes: Vec<u8> = raw
        .split('.')
        .filter_map(|s| s.parse::<u8>().ok())
        .collect();

    if bytes.len() != 16 {
        // 不是16字节的点分十进制，直接返回原值（可能是标准格式）
        return raw.to_string();
    }

    let mut groups = Vec::with_capacity(8);
    for i in 0..8 {
        let hi = bytes[i * 2];
        let lo = bytes[i * 2 + 1];
        groups.push(format!("{:x}", (hi as u16) << 8 | lo as u16));
    }
    groups.join(":")
}

/// 使用 nom 解析单行 +CGPADDR 响应
fn parse_cgpaddr_line(input: &str) -> IResult<&str, (u32, &str, &str)> {
    let (input, _) = tag("+CGPADDR:")(input)?;
    let (input, _) = take_while(|c: char| c == ' ' || c == '\t')(input)?;
    let (input, cid_str) = digit1(input)?;
    let cid: u32 = cid_str.parse().unwrap_or(0);
    let (input, _) = take_while(|c: char| c == ' ' || c == '\t')(input)?;
    let (input, _) = char(',')(input)?;
    let (input, _) = take_while(|c: char| c == ' ' || c == '\t')(input)?;
    let (input, ipv4) = parse_ip_value(input)?;
    let (input, _) = take_while(|c: char| c == ' ' || c == '\t')(input)?;
    let (input, _) = char(',')(input)?;
    let (input, _) = take_while(|c: char| c == ' ' || c == '\t')(input)?;
    let (input, ipv6) = parse_ip_value(input)?;
    Ok((input, (cid, ipv4, ipv6)))
}

/// 解析一个 IP 值：可能是带双引号的 "1.2.3.4" 或不带引号的 1.2.3.4
fn parse_ip_value(input: &str) -> IResult<&str, &str> {
    alt((
        delimited(
            char('"'),
            take_while(|c: char| c != '"' && c != '\r' && c != '\n'),
            char('"'),
        ),
        take_while(|c: char| c != ',' && c != '\r' && c != '\n' && c != ' ' && c != '\t'),
    ))
    .parse(input)
}

/// 校验是否为合法的 IPv4 地址（点分十进制）
fn is_valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| {
        if p.is_empty() || p.len() > 3 {
            return false;
        }
        p.parse::<u8>().is_ok()
    })
}

/// 校验是否为合法的 IPv6 地址（冒号十六进制格式）
fn is_valid_ipv6(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
        return false;
    }
    let double_colon_count = s.as_bytes().windows(2).filter(|w| *w == b"::").count();
    if double_colon_count > 1 {
        return false;
    }
    if s.starts_with(':') && !s.starts_with("::") {
        return false;
    }
    if s.ends_with(':') && !s.ends_with("::") {
        return false;
    }
    let segments: Vec<&str> = s.split(':').filter(|seg| !seg.is_empty()).collect();
    if segments.is_empty() {
        return double_colon_count == 1;
    }
    if segments.len() > 8 {
        return false;
    }
    segments
        .iter()
        .all(|seg| seg.len() <= 4 && u16::from_str_radix(seg, 16).is_ok())
}

async fn hardware_polling_actor(
    state: Arc<AppState>,
    mut actor_rx: mpsc::Receiver<AtRequest>,
    serial_path: String,
) {
    let mut sys = System::new_all();
    let mut components = Components::new_with_refreshed_list();

    let mut serial_file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&serial_path)
        .await
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
                            match send_at_command_async(file, &cmd).await {
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

                    let (cpin_res, qeng_res, gpad_res) = if let Some(file) = &mut serial_file {
                        let combined = "AT+CPIN?;+QENG=\"servingcell\";+CGPADDR";
                        match send_at_command_async(file, combined).await {
                            Ok(resp) => parse_combined_response(&resp),
                            Err(e) => {
                                println!("⚠️ 合并命令失败: {}，降级到单独发送", e);
                                let cpin = send_at_command_async(file, "AT+CPIN?").await.unwrap_or_default();
                                let qeng = send_at_command_async(file, "AT+QENG=\"servingcell\"").await.unwrap_or_default();
                                let gpad = send_at_command_async(file, "AT+CGPADDR").await.unwrap_or_default();
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

                    // 从静态信息中读取（只获取一次的数据）
                    {
                        let guard = state.static_info.read().await;
                        telemetry.active_sim = guard.active_sim.clone();
                        telemetry.network_provider = guard.network_provider.clone();
                        telemetry.apn = guard.apn.clone();
                        telemetry.traffic_stats = guard.traffic_stats.clone();
                    }

                    parse_qeng(&qeng_res, &mut telemetry);
                    parse_cgpaddr(&gpad_res, &mut telemetry);

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
                        let _ = state.tx.send(json_str);
                    }

                    println!("✅ 轮询任务完成，耗时: {}ms", start_poll.elapsed().as_millis());
                } else {
                    println!("💤 硬件轮询处于空闲模式 (无活跃视图)");
                }
            }
        }
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let mut broadcast_rx = state.tx.subscribe();
    println!(
        "🔌 新的 WebSocket 客户端已连接 (当前在线: {})",
        state.tx.receiver_count()
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
                        "manual_at" => AtAction::ManualAt(cmd.payload.unwrap_or_default()),
                        "set_interval" => {
                            let secs = cmd.payload.and_then(|p| p.parse().ok()).unwrap_or(3);
                            AtAction::SetInterval(secs)
                        }
                        "set_view_state" => {
                            let is_active = cmd.payload.as_deref() == Some("active");
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
                            let _ = local_tx.send(info_json.to_string()).await;
                            continue;
                        }
                        _ => continue,
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