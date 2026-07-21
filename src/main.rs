mod at;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json,
};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use rand::Rng;

type HmacSha256 = Hmac<Sha256>;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::fs;
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::{env, sync::Arc};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};

use at::{
    parse_cgpaddr, parse_cops_scan, parse_net_status, parse_qcainfo,
    parse_qeng, parse_qtemp_temperature, parse_signal_quality,
    decode_cmgl_body, decode_hex_ucs2, format_bytes, normalize_at_command,
};
use fs2::FileExt;

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const IDLE_INTERVAL_SECS: u64 = 15;

/// 直接读取 Linux /proc/uptime，零堆内存开销
fn get_uptime_mins() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|content| {
            content.split_whitespace().next()?.parse::<f64>().ok()
        })
        .map(|secs| (secs / 60.0) as u64)
        .unwrap_or(0)
}

/// 直接读取高通 SoC thermal zone 温度，零堆内存开销
fn get_soc_temperature() -> Option<String> {
    let paths = [
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/class/thermal/thermal_zone1/temp",
    ];
    for path in &paths {
        if let Ok(content) = fs::read_to_string(path) {
            if let Ok(milli_c) = content.trim().parse::<f32>() {
                let val = if milli_c > 1000.0 { milli_c / 1000.0 } else { milli_c };
                return Some(format!("{:.0} °C", val));
            }
        }
    }
    None
}

// ============================================================================
// 统一错误类型 AppError
// ============================================================================

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "success": false,
            "error": self.0.to_string(),
        }));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

// ============================================================================
// JWT Authentication Definition
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    iat: u64,
}

#[derive(Deserialize)]
struct LoginPayload {
    #[allow(dead_code)]
    username: String,
    nonce: String,
    response: String,
}

#[derive(Serialize)]
struct NonceResponse {
    nonce: String,
}

/// 从 Cookie 请求头解析 JWT Token
fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|cookie| {
            let mut parts = cookie.trim().splitn(2, '=');
            let name = parts.next()?;
            let value = parts.next()?;
            if name == "auth_token" {
                Some(value.to_string())
            } else {
                None
            }
        })
}

/// HS256 JWT 编码（纯 Rust，无 C 依赖）
fn jwt_encode(claims: &Claims, secret: &str) -> Result<String, String> {
    let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
    let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header.to_string());
    let payload = serde_json::to_string(claims).map_err(|e| e.to_string())?;
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{}.{}", header_b64, payload_b64);

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    let sig_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    Ok(format!("{}.{}", signing_input, sig_b64))
}

/// HS256 JWT 解码与验证（纯 Rust，无 C 依赖）
fn jwt_decode(token: &str, secret: &str) -> Result<Claims, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("无效的 JWT 令牌格式".to_string());
    }

    let signing_input = format!("{}.{}", parts[0], parts[1]);

    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| "JWT 签名验证失败".to_string())?;

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let claims: Claims =
        serde_json::from_slice(&payload_bytes).map_err(|e| e.to_string())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if claims.exp < now {
        return Err("JWT 令牌已过期".to_string());
    }

    Ok(claims)
}

/// 验证 JWT Token 是否有效
fn is_authenticated(headers: &HeaderMap, jwt_secret: &str) -> bool {
    if let Some(token) = extract_token_from_headers(headers) {
        jwt_decode(&token, jwt_secret).is_ok()
    } else {
        false
    }
}

// ============================================================================
// 使用 Argon2 密码哈希 + SHA256 质询响应兼容
// ============================================================================

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};

/// 使用 Argon2 生成密码哈希（用于存储，抵抗 GPU/彩虹表暴力破解）
fn hash_password_argon2(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| format!("Argon2 哈希失败: {}", e))
}

/// 对密码进行 SHA256 哈希（用于质询-响应协议，非存储）
fn hash_password_sha256(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}

// ============================================================================
// 模组闪存生命保护：内存环形滚动日志系统 (RAM Ring Buffer)
// 使用 mpsc 通道异步写入，消除 Mutex 竞争
// ============================================================================

static LOG_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();
static MEMORY_LOG_STORE: OnceLock<RwLock<VecDeque<String>>> = OnceLock::new();

fn get_log_store() -> &'static RwLock<VecDeque<String>> {
    MEMORY_LOG_STORE.get_or_init(|| RwLock::new(VecDeque::with_capacity(100)))
}

/// 初始化日志后台 Worker，返回 receiver 用于异步消费
pub fn init_log_worker() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(1024);
    LOG_TX.set(tx).ok();
    rx
}

/// 统一日志打印接口：内存存储 + 标准输出（不伤 Flash 寿命）
pub fn push_log(level: &str, module: &str, msg: &str) {
    let now = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
    let formatted = format!("[{}] [{}] [{}]: {}", now, level, module, msg);

    if level == "ERROR" {
        eprintln!("{}", formatted);
    } else {
        println!("{}", formatted);
    }

    if let Some(tx) = LOG_TX.get() {
        let _ = tx.try_send(formatted); // 非阻塞发送，满则丢弃（背压保护）
    }
}

/// 独立协程管理环形日志（无锁写入）
async fn log_worker_task(mut rx: mpsc::Receiver<String>) {
    while let Some(log) = rx.recv().await {
        let store = get_log_store();
        let mut guard = store.write().await;
        if guard.len() >= 100 {
            guard.pop_front();
        }
        guard.push_back(log);
    }
}

/// 获取日志快照（用于 WS 命令 get_backend_log）
async fn get_logs_snapshot() -> Vec<String> {
    let guard = get_log_store().read().await;
    guard.iter().cloned().collect()
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
    /// (is_nr5g, bands) — true=NR5G, false=LTE; "all" 或 "" 恢复所有频段
    SetBandLock { is_nr5g: bool, bands: String },
    /// (tech, pci, earfcn, band, enable) — enable=false 解除锁定
    SetCellLock { tech: String, pci: u32, earfcn: u32, band: Option<u32>, enable: bool },
    /// 诊断子命令
    GetDiagnostics(DiagnosticType),
}

struct AtRequest {
    action: AtAction,
    resp_tx: oneshot::Sender<serde_json::Value>,
}

struct AppState {
    /// 广播预序列化的 JSON 文本 Arc，避免每个客户端重复序列化
    tx: broadcast::Sender<Arc<String>>,
    /// 扁平、高优先级的单一串行指令通道
    command_tx: mpsc::Sender<AtRequest>,
    active_views: AtomicUsize,
    /// 全局状态 — 所有遥测汇聚于此
    global: RwLock<GlobalTelemetry>,
    /// JWT 签名密钥
    jwt_secret: String,
    /// 管理员密码的 Argon2 哈希（用于存储安全，供未来扩展）
    #[allow(dead_code)]
    admin_pass_hash: String,
    /// 管理员密码的 SHA256 哈希（用于质询-响应协议）
    admin_pass_sha: String,
    /// 管理员用户名
    admin_username: String,
    /// Nonce 防重放随机数池（一次性使用，60秒过期）
    nonce_pool: tokio::sync::Mutex<HashMap<String, Instant>>,
}

#[derive(Clone, Serialize, Debug, Default)]
struct GlobalTelemetry {
    firmware_version: Option<String>,
    temperature: Option<String>,
    sim_status: Option<String>,
    signal_percentage: Option<String>,
    internet_connection: Option<String>,
    active_sim: Option<String>,
    network_provider: Option<String>,
    mccmnc: Option<String>,
    apn: Option<String>,
    network_mode: Option<String>,
    bands: Option<String>,
    bandwidth: Option<String>,
    earfcn: Option<String>,
    pci: Option<String>,
    ipv4: Option<String>,
    ipv6: Option<String>,
    uptime: Option<String>,
    assessment: Option<String>,
    traffic_stats: Option<String>,
    cell_id: Option<String>,
    enb_id: Option<String>,
    tac: Option<String>,
    ss_rsrq: Option<String>,
    ss_rsrp: Option<String>,
    sinr: Option<String>,
    updated: Option<String>,
}

impl GlobalTelemetry {
    /// 从 TelemetryData（解析器填充）和当前全局状态合并构造 GlobalTelemetry。
    fn from_telemetry_and_global(t: &TelemetryData, g: &GlobalTelemetry, uptime: Option<String>, updated: Option<String>) -> Self {
        fn keep_valid(new: &Option<String>, old: &Option<String>) -> Option<String> {
            match new {
                Some(val) if !val.is_empty() && val != "NA" && val != "N/A" && val != "--" => Some(val.clone()),
                _ => old.clone(),
            }
        }

        Self {
            temperature: keep_valid(&t.temperature, &g.temperature),
            sim_status: keep_valid(&t.sim_status, &g.sim_status),
            signal_percentage: keep_valid(&t.signal_percentage, &g.signal_percentage),
            internet_connection: keep_valid(&t.internet_connection, &g.internet_connection),
            mccmnc: keep_valid(&t.mccmnc, &g.mccmnc),
            network_mode: keep_valid(&t.network_mode, &g.network_mode),
            bands: keep_valid(&t.bands, &g.bands),
            bandwidth: keep_valid(&t.bandwidth, &g.bandwidth),
            earfcn: keep_valid(&t.earfcn, &g.earfcn),
            pci: keep_valid(&t.pci, &g.pci),
            ipv4: keep_valid(&t.ipv4, &g.ipv4),
            ipv6: keep_valid(&t.ipv6, &g.ipv6),
            assessment: keep_valid(&t.assessment, &g.assessment),
            traffic_stats: keep_valid(&t.traffic_stats, &g.traffic_stats),
            cell_id: keep_valid(&t.cell_id, &g.cell_id),
            enb_id: keep_valid(&t.enb_id, &g.enb_id),
            tac: keep_valid(&t.tac, &g.tac),
            ss_rsrq: keep_valid(&t.ss_rsrq, &g.ss_rsrq),
            ss_rsrp: keep_valid(&t.ss_rsrp, &g.ss_rsrp),
            sinr: keep_valid(&t.sinr, &g.sinr),
            firmware_version: g.firmware_version.clone(),
            active_sim: g.active_sim.clone(),
            network_provider: g.network_provider.clone(),
            apn: g.apn.clone(),
            uptime,
            updated,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct WsCommand {
    action: String,
    payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticType {
    Neighbour,
    Qlts,
    MbnList,
    MbnAutoclose,
}

impl std::fmt::Display for DiagnosticType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagnosticType::Neighbour => write!(f, "neighbour"),
            DiagnosticType::Qlts => write!(f, "qlts"),
            DiagnosticType::MbnList => write!(f, "mbn_list"),
            DiagnosticType::MbnAutoclose => write!(f, "mbn_autoclose"),
        }
    }
}

#[derive(Serialize, Default, Debug, Clone)]
struct TelemetryData {
    temperature: Option<String>,
    sim_status: Option<String>,
    signal_percentage: Option<String>,
    internet_connection: Option<String>,
    active_sim: Option<String>,
    network_provider: Option<String>,
    mccmnc: Option<String>,
    apn: Option<String>,
    network_mode: Option<String>,
    bands: Option<String>,
    bandwidth: Option<String>,
    earfcn: Option<String>,
    pci: Option<String>,
    ipv4: Option<String>,
    ipv6: Option<String>,
    uptime: Option<String>,
    assessment: Option<String>,
    traffic_stats: Option<String>,
    cell_id: Option<String>,
    enb_id: Option<String>,
    tac: Option<String>,
    ss_rsrq: Option<String>,
    ss_rsrp: Option<String>,
    sinr: Option<String>,
    updated: Option<String>,
}

// ============================================================================
// HardwareBackend trait + RealBackend（真机后端）
// ============================================================================

/// 设备信息（HardwareBackend::read_device_info 返回值）
#[derive(Default)]
struct DeviceInfoData {
    manufacturer: String,
    model: String,
    firmware: String,
    imei: String,
    serial: String,
    hw_version: String,
    module_type: String,
    sim_status: String,
    imsi: String,
    iccid: String,
    phone: String,
    net_status: String,
    signal_quality: String,
    temperature: String,
    bands: String,
}

#[async_trait::async_trait]
trait HardwareBackend: Send + Sync {
    async fn exec_raw_at(&self, cmd: &str) -> String;
    async fn read_sms_list(&self) -> String;
    async fn configure_apn(&self, apn: &str, user: &str, pass: &str, auth_type: u8);
    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String>;
    async fn set_data_session(&self, connect: bool);
    async fn scan_available_networks(&self) -> Vec<serde_json::Value>;
    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool;
    async fn read_device_info(&self) -> DeviceInfoData;
    async fn send_reboot(&self);
    async fn send_factory_reset(&self);
    async fn set_airplane_mode(&self, on: bool);
    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String>;
    async fn set_cell_lock(&self, tech: &str, pci: u32, earfcn: u32, band: Option<u32>, enable: bool) -> Result<String, String>;
    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String>;
    async fn poll_telemetry(&self) -> TelemetryData;
    async fn read_static_info(&self) -> (String, String, String, String);
}

// ---------------------------------------------------------------------------
// 真机后端
// ---------------------------------------------------------------------------

struct RealBackend {
    serial_path: String,
}

#[async_trait::async_trait]
impl HardwareBackend for RealBackend {
    async fn exec_raw_at(&self, cmd: &str) -> String {
        match send_at_command_dedup(&self.serial_path, cmd).await {
            Ok(resp) => resp,
            Err(e) => format!("ERROR: {}", e),
        }
    }

    async fn read_sms_list(&self) -> String {
        let _ = send_at_command_inner(&self.serial_path, "AT+CMGF=1").await;
        send_at_command_inner(&self.serial_path, "AT+CMGL=\"ALL\"")
            .await
            .unwrap_or_else(|_| "+CMGL: 0 messages\r\nOK\r\n".to_string())
    }

    async fn configure_apn(&self, apn: &str, user: &str, pass: &str, auth_type: u8) {
        let _ = send_at_command_inner(
            &self.serial_path,
            &format!("AT+CGDCONT=1,\"IPV4V6\",\"{}\"", apn),
        )
        .await;
        if !user.is_empty() {
            let _ = send_at_command_inner(
                &self.serial_path,
                &format!("AT+QGAUTH=1,{},\"{}\",\"{}\"", auth_type, user, pass),
            )
            .await;
        }
    }

    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String> {
        let at_mode = match mode {
            "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
            "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
            "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
            "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
            "auto" => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
            "disable_sa" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",1",
            "disable_nsa" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",2",
            "enable_all_5g" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",0",
            _ => return Err(format!("不支持的网络模式参数: {}", mode)),
        };
        send_at_command_inner(&self.serial_path, at_mode).await
    }

    async fn set_data_session(&self, connect: bool) {
        let action_val = if connect { "1" } else { "0" };
        let _ = send_at_command_inner(
            &self.serial_path,
            &format!("AT+CGACT={},1", action_val),
        )
        .await;
    }

    async fn scan_available_networks(&self) -> Vec<serde_json::Value> {
        push_log("INFO", "Scan", "开始网络扫描 (AT+COPS=?, 240s 超时)...");
        match send_at_command_inner_with_timeout(
            &self.serial_path,
            "AT+COPS=?",
            Duration::from_secs(240),
        )
        .await
        {
            Ok(resp) => {
                let networks = parse_cops_scan(&resp);
                push_log("INFO", "Scan", &format!("网络扫描完成，发现 {} 个网络", networks.len()));
                networks
            }
            Err(e) => {
                push_log("ERROR", "Scan", &format!("网络扫描失败: {}", e));
                vec![]
            }
        }
    }

    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool {
        let mut success = false;
        if send_at_command_inner(&self.serial_path, "AT+CMGF=1").await.is_ok() {
            if send_at_command_inner(
                &self.serial_path,
                &format!("AT+CMGS=\"{}\"", recipient),
            )
            .await
            .is_ok()
            {
                if send_at_command_inner(
                    &self.serial_path,
                    &format!("{}\u{001A}", message),
                )
                .await
                .is_ok()
                {
                    success = true;
                }
            }
        }
        success
    }

    async fn read_device_info(&self) -> DeviceInfoData {
        let mfr = send_at_get_line(&self.serial_path, "AT+CGMI")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let model = send_at_get_line(&self.serial_path, "AT+CGMM")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let fw = send_at_get_line(&self.serial_path, "AT+CGMR")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let imei = send_at_get_line(&self.serial_path, "AT+CGSN")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let serial = imei.clone();
        let hw_ver = "Unknown".to_string();
        let module_type = model.clone();
        let sim_status = send_at_get_line(&self.serial_path, "AT+CPIN?")
            .await
            .and_then(|r| {
                match at::parser::parse_single_line(&r) {
                    Some(at::parser::ParsedLine::Cpin(cpin)) => Some(cpin.status),
                    _ => None,
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let imsi = send_at_get_line(&self.serial_path, "AT+CIMI")
            .await
            .and_then(|r| {
                match at::parser::parse_single_line(&r) {
                    Some(at::parser::ParsedLine::Cimi(imsi)) => Some(imsi),
                    Some(at::parser::ParsedLine::Other(val)) => {
                        Some(val.trim().to_string())
                    }
                    _ => None,
                }
            })
            .unwrap_or_default();
        let iccid = send_at_get_line(&self.serial_path, "AT+QCCID")
            .await
            .and_then(|r| {
                match at::parser::parse_single_line(&r) {
                    Some(at::parser::ParsedLine::Qccid(iccid)) => Some(iccid),
                    _ => None,
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let phone = send_at_get_line(&self.serial_path, "AT+CNUM")
            .await
            .and_then(|r| {
                match at::parser::parse_single_line(&r) {
                    Some(at::parser::ParsedLine::Cnum(cnum)) => Some(cnum.number),
                    _ => None,
                }
            })
            .unwrap_or_default();
        let net_status = send_at_get_line(&self.serial_path, "AT+CREG?")
            .await
            .map(|r| parse_net_status(&r))
            .unwrap_or_else(|| "Unknown".to_string());
        let signal_quality = send_at_get_line(&self.serial_path, "AT+CSQ")
            .await
            .map(|r| parse_signal_quality(&r))
            .unwrap_or_else(|| "Unknown".to_string());
        let temperature = send_at_get_line(&self.serial_path, "AT+QTEMP")
            .await
            .and_then(|r| parse_qtemp_temperature(&r))
            .unwrap_or_else(|| "-- °C".to_string());

        let bands = query_device_bands(&self.serial_path).await;

        DeviceInfoData {
            manufacturer: mfr,
            model,
            firmware: fw,
            imei,
            serial,
            hw_version: hw_ver,
            module_type,
            sim_status,
            imsi,
            iccid,
            phone,
            net_status,
            signal_quality,
            temperature,
            bands,
        }
    }

    async fn send_reboot(&self) {
        let _ = send_at_command_inner(&self.serial_path, "AT+CFUN=1,1").await;
    }

    async fn send_factory_reset(&self) {
        let _ = send_at_command_inner(&self.serial_path, "AT&F0").await;
    }

    async fn set_airplane_mode(&self, on: bool) {
        let cfun_val = if on { "4" } else { "1" };
        let _ = send_at_command_inner(&self.serial_path, &format!("AT+CFUN={}", cfun_val)).await;
    }

    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String> {
        let cmd = if is_nr5g {
            let b = if bands.is_empty() || bands == "all" {
                "1:2:3:5:7:8:12:20:25:28:38:40:41:48:66:71:77:78:79"
            } else {
                bands
            };
            format!("AT+QNWPREFCFG=\"nr5g_band\",{}", b)
        } else {
            let b = if bands.is_empty() || bands == "all" {
                "1:3:5:8:34:38:39:40:41"
            } else {
                bands
            };
            format!("AT+QNWPREFCFG=\"lte_band\",{}", b)
        };
        send_at_command_inner(&self.serial_path, &cmd).await
    }

    async fn set_cell_lock(&self, tech: &str, pci: u32, earfcn: u32, band: Option<u32>, enable: bool) -> Result<String, String> {
        if !enable {
            if tech.eq_ignore_ascii_case("5g") {
                send_at_command_inner(&self.serial_path, "AT+QNWLOCK=\"common/5g\",0").await
            } else {
                send_at_command_inner(&self.serial_path, "AT+QNWLOCK=\"common/lte\",0").await
            }
        } else if tech.eq_ignore_ascii_case("5g") {
            let b = band.ok_or_else(|| "5G 锁小区必须提供 Band".to_string())?;
            send_at_command_inner(&self.serial_path, &format!("AT+QNWLOCK=\"common/5g\",{},{},30,{}", pci, earfcn, b)).await
        } else {
            send_at_command_inner(&self.serial_path, &format!("AT+QNWLOCK=\"common/lte\",1,{},{}", earfcn, pci)).await
        }
    }

    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String> {
        match diag {
            DiagnosticType::Neighbour => send_at_command_inner(&self.serial_path, "AT+QENG=\"neighbourcell\"").await,
            DiagnosticType::Qlts => {
                match send_at_command_inner(&self.serial_path, "AT+QLTS=2").await {
                    Ok(res) => Ok(res),
                    Err(_) => send_at_command_inner(&self.serial_path, "AT+QLTS").await,
                }
            }
            DiagnosticType::MbnList => send_at_command_inner(&self.serial_path, "AT+QMBNCFG=\"List\"").await,
            DiagnosticType::MbnAutoclose => send_at_command_inner(&self.serial_path, "AT+QMBNCFG=\"AutoSel\",0").await,
        }
    }

    async fn poll_telemetry(&self) -> TelemetryData {
        let mut telemetry = TelemetryData::default();

        if let Ok(cgpaddr_resp) =
            send_at_command_dedup(&self.serial_path, "AT+CGPADDR").await
        {
            parse_cgpaddr(&cgpaddr_resp, &mut telemetry);
        }
        if let Ok(qeng_resp) =
            send_at_command_dedup(&self.serial_path, "AT+QENG=\"servingcell\"").await
        {
            parse_qeng(&qeng_resp, &mut telemetry);
        }
        if let Ok(qcainfo_resp) =
            send_at_command_dedup(&self.serial_path, "AT+QCAINFO").await
        {
            parse_qcainfo(&qcainfo_resp, &mut telemetry);
        }
        if let Ok(cpin_resp) =
            send_at_command_dedup(&self.serial_path, "AT+CPIN?").await
        {
            if let Some(status) = cpin_resp.lines().find_map(|l| {
                match at::parser::parse_single_line(l) {
                    Some(at::parser::ParsedLine::Cpin(cpin))
                        if !cpin.status.is_empty() =>
                    {
                        Some(cpin.status)
                    }
                    _ => None,
                }
            }) {
                telemetry.sim_status = Some(status);
            }
        }
        if let Ok(qtemp_resp) = send_at_command_dedup(&self.serial_path, "AT+QTEMP").await {
            if let Some(temp) = parse_qtemp_temperature(&qtemp_resp) {
                telemetry.temperature = Some(temp);
            }
        }

        telemetry.traffic_stats = send_at_command_dedup(&self.serial_path, "AT+QGDNRCNT?")
            .await
            .ok()
            .and_then(|r| {
                r.lines().find_map(|l| {
                    match at::parser::parse_single_line(l) {
                        Some(at::parser::ParsedLine::TrafficStats(stats)) => {
                            Some(format!(
                                "TX {} / RX {}",
                                format_bytes(stats.tx_bytes),
                                format_bytes(stats.rx_bytes)
                            ))
                        }
                        _ => None,
                    }
                })
            });
        if telemetry.traffic_stats.is_none() {
            telemetry.traffic_stats = send_at_command_dedup(&self.serial_path, "AT+QGDAT?")
                .await
                .ok()
                .and_then(|r| {
                    r.lines().find_map(|l| {
                        match at::parser::parse_single_line(l) {
                            Some(at::parser::ParsedLine::TrafficStats(stats)) => {
                                Some(format!(
                                    "TX {} / RX {}",
                                    format_bytes(stats.tx_bytes),
                                    format_bytes(stats.rx_bytes)
                                ))
                            }
                            _ => None,
                        }
                    })
                });
        }

        telemetry
    }

    async fn read_static_info(&self) -> (String, String, String, String) {
        push_log("INFO", "System", "开始获取静态信息...");

        let firmware_version;
        let mut active_sim = String::new();

        push_log("INFO", "System", "[1/4] 获取固件版本 (AT+CGMR)...");
        firmware_version = send_at_get_line(&self.serial_path, "AT+CGMR")
            .await
            .unwrap_or_else(|| "NA".to_string());

        push_log("INFO", "System", "[2/5] 获取 SIM 槽位 (AT+QUIMSLOT?)...");
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+QUIMSLOT?").await {
            if let Some(slot_name) = resp.lines().find_map(|l| {
                match at::parser::parse_single_line(l) {
                    Some(at::parser::ParsedLine::Quimslot(slot_resp)) => {
                        Some(format!("SIM {}", slot_resp.slot))
                    }
                    _ => None,
                }
            }) {
                active_sim = slot_name;
            }
        }
        if active_sim.is_empty() {
            active_sim = "NA".to_string();
        }

        push_log("INFO", "System", "[3/5] 获取运营商...");
        let network_provider = fetch_network_provider(&self.serial_path).await;

        push_log("INFO", "System", "[4/5] 获取 APN (AT+CGDCONT?)...");
        let mut apn = String::new();
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+CGDCONT?").await {
            let cgdcont_entries: Vec<_> = resp
                .lines()
                .filter_map(|l| {
                    match at::parser::parse_single_line(l) {
                        Some(at::parser::ParsedLine::Cgdcont(entry))
                            if !entry.apn.is_empty() =>
                        {
                            Some(entry)
                        }
                        _ => None,
                    }
                })
                .collect();

            if let Some(entry) = cgdcont_entries.iter().find(|e| {
                at::parser::is_valid_data_apn(&e.apn)
            }) {
                apn = entry.apn.clone();
            }

            if apn.is_empty() {
                if let Some(entry) = cgdcont_entries.first() {
                    apn = entry.apn.clone();
                }
            }
        }
        if apn.is_empty() {
            apn = "N/A".to_string();
        }

        push_log("INFO", "System", &format!("静态信息已获取: FW={}, SIM={}, Provider={}, APN={}", firmware_version, active_sim, network_provider, apn));

        (firmware_version, active_sim, network_provider, apn)
    }
}

// ---------------------------------------------------------------------------
// Mock 硬件后端（无模组电脑环境本地开发）
// ---------------------------------------------------------------------------

struct MockBackend;

#[async_trait::async_trait]
impl HardwareBackend for MockBackend {
    async fn exec_raw_at(&self, cmd: &str) -> String {
        format!("+MOCK_AT: {} -> OK\r\n", cmd)
    }

    async fn read_sms_list(&self) -> String {
        "+CMGL: 0 messages\r\nOK\r\n".to_string()
    }

    async fn configure_apn(&self, _apn: &str, _user: &str, _pass: &str, _auth_type: u8) {
        push_log("MOCK", "APN", &format!("Mock: 配置 APN"));
    }

    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String> {
        push_log("MOCK", "Net", &format!("Mock: 设置网络模式 {}", mode));
        Ok("OK\r\n".to_string())
    }

    async fn set_data_session(&self, connect: bool) {
        push_log("MOCK", "Net", &format!("Mock: 数据会话 {}", if connect { "连接" } else { "断开" }));
    }

    async fn scan_available_networks(&self) -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({ "operator": "中国移动 (MOCK)", "mccmnc": "46000", "technology": "NR5G", "status": "Available", "band": "n78" }),
            serde_json::json!({ "operator": "中国联通 (MOCK)", "mccmnc": "46001", "technology": "LTE", "status": "Available", "band": "B1" }),
            serde_json::json!({ "operator": "中国电信 (MOCK)", "mccmnc": "46003", "technology": "NR5G", "status": "Available", "band": "n78" }),
        ]
    }

    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool {
        push_log("MOCK", "SMS", &format!("Mock: 发送短信至 {}: {}", recipient, message));
        true
    }

    async fn read_device_info(&self) -> DeviceInfoData {
        DeviceInfoData {
            manufacturer: "QUECTEL (MOCK)".to_string(),
            model: "RM520N-GL (MOCK)".to_string(),
            firmware: "RM520NGLAAR01A01M4G (MOCK)".to_string(),
            imei: "861234567890123".to_string(),
            serial: "MOCK-SERIAL-001".to_string(),
            hw_version: "1.0 (MOCK)".to_string(),
            module_type: "RM520N (MOCK)".to_string(),
            sim_status: "READY (MOCK)".to_string(),
            imsi: "460001234567890".to_string(),
            iccid: "89860123456789012345".to_string(),
            phone: "+8613800000000".to_string(),
            net_status: "Registered, home network (MOCK)".to_string(),
            signal_quality: "-85 dBm / 75% (MOCK)".to_string(),
            temperature: "42 °C (MOCK)".to_string(),
            bands: "NR n78/n41, LTE B1/B3/B5/B8 (MOCK)".to_string(),
        }
    }

    async fn send_reboot(&self) {
        push_log("MOCK", "System", "Mock: 重启模组");
    }

    async fn send_factory_reset(&self) {
        push_log("MOCK", "System", "Mock: 恢复出厂设置");
    }

    async fn set_airplane_mode(&self, on: bool) {
        push_log("MOCK", "System", &format!("Mock: 飞行模式 {}", if on { "开启" } else { "关闭" }));
    }

    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String> {
        push_log("MOCK", "Band", &format!("Mock: 频段锁定 is_nr5g={}, bands={}", is_nr5g, bands));
        Ok("OK\r\n".to_string())
    }

    async fn set_cell_lock(&self, tech: &str, pci: u32, earfcn: u32, band: Option<u32>, enable: bool) -> Result<String, String> {
        push_log("MOCK", "Cell", &format!("Mock: 小区锁定 tech={}, pci={}, earfcn={}, band={:?}, enable={}", tech, pci, earfcn, band, enable));
        Ok("OK\r\n".to_string())
    }

    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String> {
        push_log("MOCK", "Diag", &format!("Mock: 诊断信息 {}", diag));
        Ok(format!("+MOCK_DIAG: {} -> OK\r\n", diag))
    }

    async fn poll_telemetry(&self) -> TelemetryData {
        TelemetryData {
            temperature: Some("42 °C".to_string()),
            sim_status: Some("READY".to_string()),
            signal_percentage: Some("88%".to_string()),
            internet_connection: Some("Connected".to_string()),
            network_mode: Some("NR5G SA".to_string()),
            bands: Some("NR5G BAND 78".to_string()),
            bandwidth: Some("100 MHz".to_string()),
            earfcn: Some("630000".to_string()),
            pci: Some("123".to_string()),
            ipv4: Some("10.88.99.100".to_string()),
            ipv6: Some("240e:1234::1".to_string()),
            mccmnc: Some("46000".to_string()),
            cell_id: Some("0x12345678".to_string()),
            enb_id: Some("0x1234".to_string()),
            tac: Some("1234".to_string()),
            ss_rsrp: Some("-85 dBm / 75%".to_string()),
            ss_rsrq: Some("-12 dB".to_string()),
            sinr: Some("18 dB".to_string()),
            assessment: Some("Excellent".to_string()),
            traffic_stats: Some("TX 1.2 GB / RX 3.5 GB".to_string()),
            ..Default::default()
        }
    }

    async fn read_static_info(&self) -> (String, String, String, String) {
        push_log("MOCK", "System", "Mock: 获取静态信息");
        (
            "RM520NGLAAR01A01M4G (MOCK)".to_string(),
            "SIM 1 (MOCK)".to_string(),
            "中国电信 (MOCK)".to_string(),
            "ctnet (MOCK)".to_string(),
        )
    }
}


// ============================================================================
// 通用工具函数
// ============================================================================

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
async fn fetch_network_provider(serial_path: &str) -> String {
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QSPN").await {
        if let Some(provider) = resp.lines().find_map(|l| {
            match at::parser::parse_single_line(l) {
                Some(at::parser::ParsedLine::Qspn(qspn))
                    if !qspn.fnn.is_empty() && qspn.fnn != "????" =>
                {
                    let decoded = decode_hex_ucs2(&qspn.fnn);
                    Some(if decoded.is_empty() { qspn.fnn } else { decoded })
                }
                _ => None,
            }
        }) {
            return provider;
        }
    }

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+COPS?").await {
        if let Some(provider) = resp.lines().find_map(|l| {
            match at::parser::parse_single_line(l) {
                Some(at::parser::ParsedLine::Cops(cops)) => {
                    if let Some(oper) = cops.oper {
                        let trimmed = oper.trim();
                        if !trimmed.is_empty() && trimmed != "????" {
                            let decoded = decode_hex_ucs2(trimmed);
                            return Some(if decoded.is_empty() { trimmed.to_string() } else { decoded });
                        }
                    }
                    None
                }
                _ => None,
            }
        }) {
            return provider;
        }
    }

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QENG=\"servingcell\"").await {
        if let Some(mccmnc) = resp.lines().find_map(|l| {
            match at::parser::parse_single_line(l) {
                Some(at::parser::ParsedLine::QengServingCell(cell)) => {
                    Some(format!("{}{}", cell.mcc, cell.mnc))
                }
                _ => None,
            }
        }) {
            return match mccmnc.as_str() {
                "46000" | "46002" | "46007" => "中国移动".to_string(),
                "46001" => "中国联通".to_string(),
                "46003" | "46011" => "中国电信".to_string(),
                _ => "Unknown".to_string(),
            };
        }
    }

    "Unknown".to_string()
}

// ============================================================================
// AT 命令基础架构（互斥锁 + 去重队列 + 跨进程文件锁）
// ============================================================================

/// 跨进程文件锁
struct SerialFileLock {
    file: Option<std::fs::File>,
}

impl Drop for SerialFileLock {
    fn drop(&mut self) {
        if let Some(ref file) = self.file {
            let _ = file.unlock();
        }
    }
}

const LOCK_FILE_PATH: &str = "/tmp/atcmd_rs.lock";

async fn acquire_serial_lock() -> Result<SerialFileLock, String> {
    let result = tokio::task::spawn_blocking(|| -> Result<std::fs::File, String> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(LOCK_FILE_PATH)
            .map_err(|e| format!("无法打开锁文件 {}: {}", LOCK_FILE_PATH, e))?;

        file.lock_exclusive()
            .map_err(|e| format!("获取 {} 排他锁失败: {}", LOCK_FILE_PATH, e))?;

        Ok(file)
    })
    .await
    .map_err(|e| format!("文件锁任务被取消: {}", e))??;

    Ok(SerialFileLock { file: Some(result) })
}

static AT_CMD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn get_at_lock() -> &'static tokio::sync::Mutex<()> {
    AT_CMD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

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

    async fn execute(&self, serial_path: &str, cmd: &str) -> Result<String, String> {
        use std::collections::hash_map::Entry;

        let mut guard = self.inner.lock().await;

        match guard.entry(cmd.to_string()) {
            Entry::Occupied(mut entry) => {
                let (tx, rx) = oneshot::channel();
                entry.get_mut().waiters.push(tx);
                drop(guard);
                rx.await.unwrap_or_else(|_| Err("AT命令队列等待被取消".to_string()))
            }
            Entry::Vacant(entry) => {
                entry.insert(AtPendingCmd { waiters: vec![] });
                drop(guard);

                let result = send_at_command_inner(serial_path, cmd).await;

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

async fn send_at_command_dedup(serial_path: &str, cmd: &str) -> Result<String, String> {
    get_at_queue().execute(serial_path, cmd).await
}

async fn send_at_command_inner_with_timeout(
    serial_path: &str,
    cmd: &str,
    timeout: Duration,
) -> Result<String, String> {
    let start = std::time::Instant::now();

    let _lock = get_at_lock().lock().await;
    let _file_lock = acquire_serial_lock().await?;

    let child = tokio::process::Command::new("atcmd_rs")
        .arg("-p")
        .arg(serial_path)
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("atcmd_rs启动失败: {}", e))?;

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("atcmd_rs执行失败: {}", e)),
        Err(_) => return Err(format!("AT命令超时({}s): {}", timeout.as_secs(), cmd)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "atcmd_rs失败 (exit={}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();

    if raw.contains("ERROR") || raw.contains("+CME ERROR:") {
        let elapsed = start.elapsed();
        push_log("WARN", "AT", &format!("[AT] {} 返回 ERROR ({}ms)", cmd, elapsed.as_millis()));
        return Err(format!("AT命令返回错误: {}", raw.trim()));
    }

    let elapsed = start.elapsed();
    let preview = raw
        .lines()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("AT+")
        })
        .unwrap_or(&raw);
    push_log("INFO", "AT", &format!("{} 成功 ({}ms): {}", cmd, elapsed.as_millis(), preview.trim()));
    Ok(raw)
}

async fn send_at_command_inner(serial_path: &str, cmd: &str) -> Result<String, String> {
    send_at_command_inner_with_timeout(serial_path, cmd, Duration::from_secs(10)).await
}

async fn query_device_bands(serial_path: &str) -> String {
    let mut nr_bands: Vec<String> = Vec::new();
    let mut lte_bands: Vec<String> = Vec::new();

    if let Ok(resp) = send_at_command_dedup(serial_path, "AT+QENG=\"servingcell\"").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if let Some(at::parser::ParsedLine::QengServingCell(cell)) =
                at::parser::parse_single_line(trimmed)
            {
                if cell.band.is_empty() || cell.band == "-" {
                    continue;
                }
                if cell.rat.contains("NR5G") {
                    if !nr_bands.contains(&format!("n{}", cell.band)) {
                        nr_bands.push(format!("n{}", cell.band));
                    }
                } else if cell.rat == "LTE" {
                    if !lte_bands.contains(&format!("B{}", cell.band)) {
                        lte_bands.push(format!("B{}", cell.band));
                    }
                }
            }
        }
    }

    if let Ok(resp) = send_at_command_dedup(serial_path, "AT+QCAINFO").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if let Some(at::parser::ParsedLine::Qcainfo(entry)) =
                at::parser::parse_single_line(trimmed)
            {
                if entry.band.starts_with("NR5G BAND ") {
                    let num = entry.band.strip_prefix("NR5G BAND ").unwrap_or("").trim();
                    if !num.is_empty() && !nr_bands.contains(&format!("n{}", num)) {
                        nr_bands.push(format!("n{}", num));
                    }
                } else if let Ok(band_num) = entry.band.parse::<u32>() {
                    if band_num > 100 {
                        if !nr_bands.contains(&format!("n{}", band_num)) {
                            nr_bands.push(format!("n{}", band_num));
                        }
                    } else {
                        if !lte_bands.contains(&format!("B{}", band_num)) {
                            lte_bands.push(format!("B{}", band_num));
                        }
                    }
                }
            }
        }
    }

    nr_bands.sort_by_key(|b| {
        b.trim_start_matches('n').parse::<u32>().unwrap_or(999)
    });
    lte_bands.sort_by_key(|b| {
        b.trim_start_matches('B').parse::<u32>().unwrap_or(999)
    });

    let mut result = Vec::new();
    if !nr_bands.is_empty() {
        result.push(format!("NR {}", nr_bands.join("/")));
    }
    if !lte_bands.is_empty() {
        result.push(format!("LTE {}", lte_bands.join("/")));
    }

    if result.is_empty() {
        "--".to_string()
    } else {
        result.join(", ")
    }
}

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

fn static_file_response(
    body: &'static str,
    content_type: &'static str,
) -> Result<Response<axum::body::Body>, AppError> {
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(body))?)
}

// ============================================================================
// 质询-响应认证 Handlers
// ============================================================================

/// 将 GlobalTelemetry 序列化为 JSON 字符串（预序列化广播辅助函数）
fn serialize_global(global: &GlobalTelemetry, update_type: &str) -> String {
    serde_json::json!({
        "update_type": update_type,
        "data": global
    }).to_string()
}

/// 从请求头中提取客户端 IP
fn client_ip(headers: &HeaderMap) -> String {
    if let Some(val) = headers.get("x-forwarded-for") {
        if let Ok(val) = val.to_str() {
            if let Some(ip) = val.split(',').next() {
                return ip.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

/// 获取一次性动态随机数（Nonce）：GET /api/get_nonce
async fn get_nonce_handler(State(state): State<Arc<AppState>>) -> Json<NonceResponse> {
    let now = Instant::now();
    let mut pool = state.nonce_pool.lock().await;

    pool.retain(|_, timestamp| now.duration_since(*timestamp).as_secs() < 60);

    let random_bytes: [u8; 16] = rand::thread_rng().r#gen();
    let nonce = hex::encode(random_bytes);

    pool.insert(nonce.clone(), now);

    Json(NonceResponse { nonce })
}

async fn index_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Result<Response, AppError> {
    if !is_authenticated(&headers, &state.jwt_secret) {
        return Ok(Redirect::to("/login").into_response());
    }
    static_file_response(include_str!("index.html"), "text/html; charset=utf-8")
}

async fn login_get_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Result<Response, AppError> {
    if is_authenticated(&headers, &state.jwt_secret) {
        return Ok(Redirect::to("/").into_response());
    }
    static_file_response(include_str!("login.html"), "text/html; charset=utf-8")
}

async fn login_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<LoginPayload>,
) -> Response {
    let ip = client_ip(&headers);

    let is_nonce_valid = {
        let mut pool = state.nonce_pool.lock().await;
        pool.remove(&payload.nonce).is_some()
    };

    if !is_nonce_valid {
        push_log("WARN", "Auth", &format!(
            "登录失败(随机数无效): 用户 '{}' 从 {} 尝试",
            payload.username, ip
        ));
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                r#"{"success":false,"msg":"随机数无效或已过期，请刷新页面重试"}"#,
            ))
            .unwrap();
    }

    // SHA256(nonce + username + SHA256(admin_password))
    let expected_response =
        hash_password_sha256(&format!("{}{}{}", payload.nonce, state.admin_username, state.admin_pass_sha));

    if payload.response.eq_ignore_ascii_case(&expected_response) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let claims = Claims {
            sub: "admin".to_string(),
            exp: now + 86400 * 7,
            iat: now,
        };

        let token = match jwt_encode(&claims, &state.jwt_secret) {
            Ok(t) => t,
            Err(e) => {
                push_log("ERROR", "Auth", &format!("JWT 签名生成失败: {}", e));
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        r#"{"success":false,"msg":"服务器内部错误，登录失败"}"#,
                    ))
                    .unwrap();
            }
        };

        push_log("INFO", "Auth", &format!("管理员登录成功 (来源: {})", ip));

        let cookie = format!(
            "auth_token={}; HttpOnly; Path=/; SameSite=Lax; Max-Age=604800",
            token
        );

        Response::builder()
            .status(StatusCode::OK)
            .header(header::SET_COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(r#"{"success":true,"msg":"登录成功"}"#))
            .unwrap()
    } else {
        push_log("WARN", "Auth", &format!(
            "登录失败(密码错误): 用户 '{}' 从 {} 尝试",
            payload.username, ip
        ));
        Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(r#"{"success":false,"msg":"密码错误，请重试"}"#))
            .unwrap()
    }
}

async fn logout_post_handler() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::SET_COOKIE, "auth_token=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0")
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(r#"{"success":true}"#))
        .unwrap()
}

// ============================================================================
// 统一硬件任务：串行消费 /opt/atcmd_rs，定时轮询 + 用户指令均经此通道
// ============================================================================

/// 处理单个 channel 消息
async fn handle_channel_req(
    req: Option<AtRequest>,
    backend: &Arc<dyn HardwareBackend>,
    interval_secs: &mut u64,
    interval: &mut tokio::time::Interval,
    current_interval_secs: &mut u64,
) -> bool {
    match req {
        Some(r) => {
            handle_at_request(r, backend, interval_secs, interval, current_interval_secs).await;
            true
        }
        None => false,
    }
}

/// 处理单个 AT 请求
async fn handle_at_request(
    req: AtRequest,
    backend: &Arc<dyn HardwareBackend>,
    interval_secs: &mut u64,
    interval: &mut tokio::time::Interval,
    current_interval_secs: &mut u64,
) {
    match req.action {
        AtAction::ManualAt(cmd) => {
            let cmd = normalize_at_command(&cmd);
            push_log("INFO", "Actor", &format!("顺序执行手动 AT: {}", cmd));
            let response = backend.exec_raw_at(&cmd).await;
            let _ = req.resp_tx.send(serde_json::json!({ "type": "at_res", "data": response }));
        }
        AtAction::SetInterval(secs) => {
            let new_secs = secs.max(5);
            *interval_secs = new_secs;
            *current_interval_secs = new_secs;
            *interval = tokio::time::interval(Duration::from_secs(*interval_secs));
            interval.tick().await;
            push_log("INFO", "Settings", &format!("轮询间隔已调整为 {} 秒", new_secs));
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": format!("轮询间隔已调整为 {} 秒", new_secs) }
            }));
        }
        AtAction::GetSmsList => {
            push_log("INFO", "Actor", "读取短信列表...");
            let resp = backend.read_sms_list().await;
            let decoded = decode_cmgl_body(&resp);
            let _ = req.resp_tx.send(serde_json::json!({ "type": "sms_list", "data": decoded }));
        }
        AtAction::SetApn(apn, user, pass, auth_type) => {
            push_log("INFO", "Actor", &format!("配置 APN: {}", apn));
            backend.configure_apn(&apn, &user, &pass, auth_type).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": { "status": "APN settings applied", "apn": apn }
            }));
        }
        AtAction::SetNetworkMode(mode) => {
            push_log("INFO", "Actor", &format!("切换网络模式为: {}", mode));
            let res = backend.set_network_mode_pref(&mode).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::NetConnect(connect) => {
            push_log("INFO", "Actor", &format!("拨号控制: {}", if connect { "连接" } else { "断开" }));
            backend.set_data_session(connect).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": { "status": format!("{} command sent", if connect { "Connect" } else { "Disconnect" }) }
            }));
        }
        AtAction::NetworkScan => {
            push_log("INFO", "Actor", "开始扫描可用网络...");
            let networks = backend.scan_available_networks().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "scan_result",
                "data": { "status": "Scan complete.", "networks": networks }
            }));
        }
        AtAction::SendSms(recipient, message) => {
            push_log("INFO", "Actor", &format!("发送短信至: {}", recipient));
            let success = backend.send_sms_msg(&recipient, &message).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "sms_sent",
                "data": { "status": if success { "SMS sent successfully" } else { "SMS failed" }, "recipient": recipient }
            }));
        }
        AtAction::GetDeviceInfo => {
            push_log("INFO", "Actor", "获取设备详细信息...");
            let info = backend.read_device_info().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "device_info",
                "data": {
                    "manufacturer": info.manufacturer,
                    "model": info.model,
                    "firmware_version": info.firmware,
                    "imei": info.imei,
                    "serial": info.serial,
                    "hw_version": info.hw_version,
                    "module_type": info.module_type,
                    "sim_status": info.sim_status,
                    "imsi": info.imsi,
                    "iccid": info.iccid,
                    "phone": info.phone,
                    "net_status": info.net_status,
                    "signal": info.signal_quality,
                    "temperature": info.temperature,
                    "bands": info.bands,
                    "max_rate": "--",
                    "volte": "--",
                    "gnss": "--"
                }
            }));
        }
        AtAction::Reboot => {
            push_log("WARN", "System", "重启模组指令已下发");
            backend.send_reboot().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": "Reboot command sent to module." }
            }));
        }
        AtAction::FactoryReset => {
            push_log("WARN", "System", "模组恢复出厂设置指令已下发");
            backend.send_factory_reset().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": "Factory reset command sent to module." }
            }));
        }
        AtAction::FlightMode(on) => {
            push_log("INFO", "System", &format!("飞行模式状态改变: {}", if on { "开启" } else { "关闭" }));
            backend.set_airplane_mode(on).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": format!("Flight mode turned {}", if on { "ON" } else { "OFF" }) }
            }));
        }
        AtAction::SetBandLock { is_nr5g, bands } => {
            push_log("INFO", "Actor", &format!("设置频段锁定: is_nr5g={}, bands={}", is_nr5g, bands));
            let res = backend.set_band_lock(is_nr5g, &bands).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "band_lock_res",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::SetCellLock { tech, pci, earfcn, band, enable } => {
            push_log("INFO", "Actor", &format!("设置小区锁定: tech={}, pci={}, earfcn={}, band={:?}, enable={}", tech, pci, earfcn, band, enable));
            let res = backend.set_cell_lock(&tech, pci, earfcn, band, enable).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "cell_lock_res",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::GetDiagnostics(diag) => {
            push_log("INFO", "Actor", &format!("获取诊断信息: {}", diag));
            let res = backend.get_diagnostics(&diag).await;
            let data = res.as_ref().ok().and_then(|r| serde_json::from_str::<serde_json::Value>(r).ok()).unwrap_or_default();
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "diagnostics_res",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e), "data": data }
            }));
        }
    }
}

/// 统一硬件任务：定时轮询 + 用户 AT 指令。严格串行 Actor + 预序列化广播
async fn hardware_task(
    mut command_rx: mpsc::Receiver<AtRequest>,
    backend: Arc<dyn HardwareBackend>,
    state: Arc<AppState>,
) {
    let mut interval_secs = 5u64;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await;
    let mut consecutive_failures = 0u32;
    let mut current_interval_secs = interval_secs;

    loop {
        tokio::select! {
            biased;

            req = command_rx.recv() => {
                if !handle_channel_req(req, &backend, &mut interval_secs, &mut interval, &mut current_interval_secs).await {
                    break;
                }
            }
            _ = interval.tick() => {
                let active = state.active_views.load(Ordering::Acquire) > 0;

                if active && get_at_lock().try_lock().is_ok() {
                    let start_poll = std::time::Instant::now();
                    let mut telemetry = backend.poll_telemetry().await;

                    let telemetry_dead = telemetry.sim_status.is_none()
                        && telemetry.network_mode.is_none()
                        && telemetry.signal_percentage.is_none()
                        && telemetry.ipv4.is_none();
                    consecutive_failures = if telemetry_dead { consecutive_failures + 1 } else { 0 };

                    if telemetry.temperature.is_none() {
                        telemetry.temperature = get_soc_temperature();
                    }

                    telemetry.internet_connection = Some(if telemetry.ipv4.is_some() {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    });

                    let uptime_mins = get_uptime_mins();
                    let updated_str = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    {
                        let mut global = state.global.write().await;

                        if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                            push_log("ERROR", "Poll", &format!("模组连续 {} 次无响应，标记为离线", MAX_CONSECUTIVE_FAILURES));
                            let fw = global.firmware_version.clone();
                            let sim = global.active_sim.clone();
                            let prov = global.network_provider.clone();
                            let apn = global.apn.clone();
                            *global = GlobalTelemetry::default();
                            global.firmware_version = fw;
                            global.active_sim = sim;
                            global.network_provider = prov;
                            global.apn = apn;
                            global.internet_connection = Some("Disconnected".to_string());
                            global.uptime = Some(format!("{} minutes", uptime_mins));
                            global.updated = Some(updated_str);
                        } else {
                            *global = GlobalTelemetry::from_telemetry_and_global(
                                &telemetry, &global,
                                Some(format!("{} minutes", uptime_mins)),
                                Some(updated_str),
                            );
                        }

                        // 预序列化为 JSON 字符串并广播（零重复序列化损耗）
                        let json_str = serialize_global(&global, "delta");
                        let _ = state.tx.send(Arc::new(json_str));
                    }

                    push_log("INFO", "Poll", &format!("轮询完成 (耗时 {}ms)", start_poll.elapsed().as_millis()));

                } else if active {
                    push_log("WARN", "Poll", "串口忙或正在执行慢命令，跳过当次采集，复用缓存");
                    let uptime_mins = get_uptime_mins();
                    let updated_str = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    let mut global = state.global.write().await;
                    global.uptime = Some(format!("{} minutes", uptime_mins));
                    global.updated = Some(updated_str);

                    let json_str = serialize_global(&global, "delta");
                    let _ = state.tx.send(Arc::new(json_str));

                } else {
                    push_log("INFO", "Poll", "硬件轮询处于空闲模式 (无活跃视图)");
                }

                let next_secs = if active { interval_secs } else { IDLE_INTERVAL_SECS };
                if next_secs != current_interval_secs {
                    current_interval_secs = next_secs;
                    interval = tokio::time::interval(Duration::from_secs(next_secs));
                    interval.tick().await;
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    // 初始化异步日志 Worker
    let log_rx = init_log_worker();
    tokio::spawn(log_worker_task(log_rx));

    let serial_path =
        env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
    push_log("INFO", "System", &format!("正在初始化串口设备: {}", serial_path));

    let (tx, _) = broadcast::channel(100);
    let (command_tx, command_rx) = mpsc::channel(32);

    let jwt_secret =
        env::var("JWT_SECRET").unwrap_or_else(|_| "argon_rm520n_jwt_secret_key_default".to_string());
    let admin_password =
        env::var("WEBUI_PASSWORD").unwrap_or_else(|_| "admin123".to_string());
    let admin_username =
        env::var("WEBUI_USERNAME").unwrap_or_else(|_| "admin".to_string());

    // Argon2 密码哈希存储 + SHA256 兼容用于质询-响应
    let admin_pass_argon = hash_password_argon2(&admin_password)
        .unwrap_or_else(|e| {
            eprintln!("Argon2 哈希失败: {}，回退至 SHA256", e);
            hash_password_sha256(&admin_password)
        });
    let admin_pass_sha = hash_password_sha256(&admin_password);

    push_log("INFO", "System", &format!("密码哈希算法: Argon2 (存储) + SHA256 (质询-响应)"));

    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        command_tx,
        active_views: AtomicUsize::new(0),
        global: RwLock::new(GlobalTelemetry::default()),
        jwt_secret,
        admin_pass_hash: admin_pass_argon,
        admin_pass_sha,
        admin_username,
        nonce_pool: tokio::sync::Mutex::new(HashMap::new()),
    });

    // 根据串口设备是否存在选择后端
    let backend: Arc<dyn HardwareBackend> = if std::path::Path::new(&serial_path).exists() {
        push_log("INFO", "System", "检测到真实串口设备，使用 RealBackend");
        Arc::new(RealBackend { serial_path: serial_path.clone() })
    } else {
        push_log("WARN", "System", "未检测到串口设备，已自动开启模拟测试模式 (MockBackend)");
        Arc::new(MockBackend)
    };

    // 1. 启动硬件后台轮询与指令消费任务
    tokio::spawn({
        let backend = backend.clone();
        let state = app_state.clone();
        async move { hardware_task(command_rx, backend, state).await }
    });

    // 2. 将静态信息初始化放入后台异步任务
    tokio::spawn({
        let backend = backend.clone();
        let app_state = app_state.clone();
        async move {
            let (firmware_version, active_sim, network_provider, apn) = backend.read_static_info().await;
            {
                let mut guard = app_state.global.write().await;
                guard.firmware_version = Some(firmware_version);
                guard.active_sim = Some(active_sim);
                guard.network_provider = Some(network_provider);
                guard.apn = Some(apn);
                let json_str = serialize_global(&guard, "full");
                let _ = app_state.tx.send(Arc::new(json_str));
            }
        }
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/login", get(login_get_handler))
        .route("/api/get_nonce", get(get_nonce_handler))
        .route("/api/login", post(login_post_handler))
        .route("/api/logout", post(logout_post_handler))
        .route("/ws", get(ws_handler))
        .route("/style.css", get(style_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    push_log("INFO", "System", "RM520N WebUI 后端服务已在 http://0.0.0.0:3000 监听");
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state.jwt_secret) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized WebSocket Request").into_response();
    }
    ws.on_upgrade(|socket| handle_ws(socket, state)).into_response()
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    // 收到 Arc<String> 预序列化广播，零反序列化/序列化开销
    let mut broadcast_rx = state.tx.subscribe();
    let online_count = state.tx.receiver_count();
    push_log("INFO", "WS", &format!("新的 WebSocket 客户端已连接 (当前在线: {})", online_count));

    state.active_views.fetch_add(1, Ordering::SeqCst);
    let mut current_is_active = true;

    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // 发送全量快照作为第一条消息
    {
        let guard = state.global.read().await;
        let full_json = serialize_global(&guard, "full");
        if ws_sender.send(Message::Text(full_json.into())).await.is_err() {
            if current_is_active {
                state.active_views.fetch_sub(1, Ordering::SeqCst);
            }
            return;
        }
    }

    tokio::select! {
        _ = async {
            loop {
                tokio::select! {
                    result = broadcast_rx.recv() => {
                        match result {
                            Ok(pre_serialized) => {
                                if ws_sender.send(Message::Text((*pre_serialized).clone().into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => continue, // 通道满跳过旧消息或已关闭
                        }
                    }
                    Some(reply) = local_rx.recv() => {
                        if ws_sender.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            push_log("WARN", "WS", "WebSocket 发送通道中断，准备断开连接");
        } => {},

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
                            let guard = state_inner.global.read().await;
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
                        "get_backend_log" => {
                            let logs = get_logs_snapshot().await;
                            let log_json = serde_json::json!({
                                "type": "backend_log",
                                "data": logs
                            });
                            let _ = local_tx.send(log_json.to_string()).await;
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
                        "set_band_lock" => {
                            let payload = cmd.payload.as_ref();
                            let is_nr5g = payload.and_then(|p| p.get("nr5g")).or_else(|| payload.and_then(|p| p.get("is_nr5g"))).and_then(|v| v.as_bool()).unwrap_or(false);
                            let bands = payload.and_then(|p| p.get("bands")).or_else(|| payload.and_then(|p| p.get("band"))).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            AtAction::SetBandLock { is_nr5g, bands }
                        }
                        "set_cell_lock" => {
                            let payload = cmd.payload.as_ref();
                            let tech = payload.and_then(|p| p.get("tech")).and_then(|v| v.as_str()).unwrap_or("lte").to_string();
                            let pci = payload.and_then(|p| p.get("pci")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let earfcn = payload.and_then(|p| p.get("earfcn")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let band = payload.and_then(|p| p.get("band")).and_then(|v| v.as_u64()).map(|v| v as u32);
                            let enable = payload.and_then(|p| p.get("enable")).and_then(|v| v.as_bool()).unwrap_or(true);
                            AtAction::SetCellLock { tech, pci, earfcn, band, enable }
                        }
                        "get_diagnostics" => {
                            let sub = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("");
                            let diag = match sub {
                                "neighbour" | "neighbor" => DiagnosticType::Neighbour,
                                "qlt" | "qlts" => DiagnosticType::Qlts,
                                "mbn_list" | "qmbncfg" => DiagnosticType::MbnList,
                                "mbn_autoclose" | "autosel" => DiagnosticType::MbnAutoclose,
                                _ => {
                                    push_log("WARN", "WS", &format!("未知诊断子命令: {}", sub));
                                    continue;
                                }
                            };
                            AtAction::GetDiagnostics(diag)
                        }
                        "set_mode_pref" => {
                            let mode = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("auto").to_string();
                            AtAction::SetNetworkMode(mode)
                        }
                        unknown_action => {
                            push_log("WARN", "WS", &format!("未知 WebSocket 动作: {:?}", unknown_action));
                            continue;
                        }
                    };

                    if state_inner.command_tx.send(AtRequest { action, resp_tx }).await.is_ok() {
                        if let Ok(reply) = resp_rx.await {
                            if local_tx.send(reply.to_string()).await.is_err() {
                                break;
                            }
                        }
                    }
                } else {
                    push_log("WARN", "WS", &format!("WS 收到无法解析的消息: {:?}", text));
                }
            }
            push_log("INFO", "WS", "浏览器主动断开 WebSocket 连接（接收流正常结束）");
        } => {}
    };

    if current_is_active {
        state.active_views.fetch_sub(1, Ordering::SeqCst);
    }

    push_log("INFO", "WS", &format!("WebSocket 客户端已断开 (当前在线: {})", state.tx.receiver_count()));
}

async fn style_handler() -> impl IntoResponse {
    static_file_response(
        include_str!("../templates/style.css"),
        "text/css; charset=utf-8",
    )
    .unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "CSS not found").into_response()
    })
}
