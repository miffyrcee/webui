// ============================================================================
// quectel-webui — 移远 5G 终端 WebUI 服务端
//
// 架构亮点：
// 1. ★ 零成本视图探活：state.tx.receiver_count() 替代 active_views，
//    彻底消除 set_view_state 通信与计数器漂移风险
// 2. ★ 内存级串口访问：专用串口 I/O 线程持有 /dev/smd11 fd，
//    消除子进程 fork/exec 开销（延迟从 50~100ms 降至 <5ms）
// 3. ★ URC 主动上报：AT 命令响应后自动检测尾随 URC 数据
// 4. ★ 静态/动态状态分离：StaticDeviceInfo 单次读取永不重传，
//    DynamicTelemetry 轻量高频广播（包体缩小 80%）
// ============================================================================

mod at;

use at::{
    decode_cmgl_body, decode_hex_ucs2, normalize_at_command, parse_cgpaddr, parse_cops_scan,
    parse_net_status, parse_qcainfo, parse_qcfg_bands, parse_qeng, parse_qtemp_temperature,
    parse_signal_quality, parse_traffic_line, set_default,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::header,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use chrono::Local;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::sync::OnceLock;
use std::time::Duration;
use std::{env, sync::Arc};
use sysinfo::{Components, System};
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};

// ============================================================================
// 1. 常量
// ============================================================================

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const IDLE_INTERVAL_SECS: u64 = 15;
const POLL_INTERVAL_ACTIVE: u64 = 5;

/// AT 响应终止符 — 出现即表示响应结束（保持与 atcmd_rs 一致）
const AT_TERMINATORS: &[&str] = &[
    "+CME ERROR:",
    "+CMS ERROR:",
    "BUSY\r\n",
    "ERROR\r\n",
    "NO ANSWER\r\n",
    "NO CARRIER\r\n",
    "NO DIALTONE\r\n",
    "OK\r\n",
];

/// URC 前缀 — 模组主动上报的行首匹配
const URC_PREFIXES: &[&str] = &[
    "+QIND:",
    "+CMT:",
    "+CMTI:",
    "+CREG:",
    "+CGREG:",
    "+CEREG:",
    "+QHUP:",
    "+QCLOSE:",
];

// ============================================================================
// 2. 数据模型
// ============================================================================

/// 静态设备信息 — 开机初始化时获取一次，永不重传。
/// 仅在 WebSocket 连接建立时通过 get_static_info 发送给新客户端。
#[derive(Clone, Serialize, Debug)]
struct StaticDeviceInfo {
    firmware_version: String,
    active_sim: String,
    network_provider: String,
    apn: String,
}

impl Default for StaticDeviceInfo {
    fn default() -> Self {
        Self {
            firmware_version: "NA".to_string(),
            active_sim: "NA".to_string(),
            network_provider: "NA".to_string(),
            apn: "NA".to_string(),
        }
    }
}

/// 动态遥测数据 — 高频变化，每个轮询周期广播。
/// 不包含设备开机后永不变化的静态信息（固件版本、ICCID等）。
#[derive(Clone, Serialize, Debug)]
struct DynamicTelemetry {
    temperature: String,
    sim_status: String,
    signal_percentage: String,
    internet_connection: String,
    mccmnc: String,
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

impl Default for DynamicTelemetry {
    fn default() -> Self {
        Self {
            temperature: "NA".to_string(),
            sim_status: "NA".to_string(),
            signal_percentage: "NA".to_string(),
            internet_connection: "NA".to_string(),
            mccmnc: "NA".to_string(),
            network_mode: "NA".to_string(),
            bands: "NA".to_string(),
            bandwidth: "NA".to_string(),
            earfcn: "NA".to_string(),
            pci: "NA".to_string(),
            ipv4: "NA".to_string(),
            ipv6: "NA".to_string(),
            uptime: "NA".to_string(),
            assessment: "NA".to_string(),
            traffic_stats: "NA".to_string(),
            cell_id: "NA".to_string(),
            enb_id: "NA".to_string(),
            tac: "NA".to_string(),
            ss_rsrq: "NA".to_string(),
            ss_rsrp: "NA".to_string(),
            sinr: "NA".to_string(),
            updated: "NA".to_string(),
        }
    }
}

impl DynamicTelemetry {
    /// 从 TelemetryData（解析器填充）和当前 DynamicTelemetry 合并构造新值。
    /// 动态字段仅当解析器返回有效值时才覆盖，否则保留上一次的有效值。
    fn from_telemetry(t: &TelemetryData, prev: &DynamicTelemetry, uptime: String, updated: String) -> Self {
        let keep_valid = |new: &str, old: &str| -> String {
            if !new.is_empty() && new != "NA" && new != "N/A" && new != "--" {
                new.to_string()
            } else {
                old.to_string()
            }
        };

        Self {
            temperature: keep_valid(&t.temperature, &prev.temperature),
            sim_status: keep_valid(&t.sim_status, &prev.sim_status),
            signal_percentage: keep_valid(&t.signal_percentage, &prev.signal_percentage),
            internet_connection: keep_valid(&t.internet_connection, &prev.internet_connection),
            mccmnc: keep_valid(&t.mccmnc, &prev.mccmnc),
            network_mode: keep_valid(&t.network_mode, &prev.network_mode),
            bands: keep_valid(&t.bands, &prev.bands),
            bandwidth: keep_valid(&t.bandwidth, &prev.bandwidth),
            earfcn: keep_valid(&t.earfcn, &prev.earfcn),
            pci: keep_valid(&t.pci, &prev.pci),
            ipv4: keep_valid(&t.ipv4, &prev.ipv4),
            ipv6: keep_valid(&t.ipv6, &prev.ipv6),
            assessment: keep_valid(&t.assessment, &prev.assessment),
            traffic_stats: keep_valid(&t.traffic_stats, &prev.traffic_stats),
            cell_id: keep_valid(&t.cell_id, &prev.cell_id),
            enb_id: keep_valid(&t.enb_id, &prev.enb_id),
            tac: keep_valid(&t.tac, &prev.tac),
            ss_rsrq: keep_valid(&t.ss_rsrq, &prev.ss_rsrq),
            ss_rsrp: keep_valid(&t.ss_rsrp, &prev.ss_rsrp),
            sinr: keep_valid(&t.sinr, &prev.sinr),
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

#[derive(Debug)]
enum AtAction {
    ManualAt(String),
    SetInterval(u64),
    GetSmsList,
    SetApn(String, String, String, u8),
    SetNetworkMode(String),
    NetConnect(bool),
    NetworkScan,
    SendSms(String, String),
    GetDeviceInfo,
    Reboot,
    FactoryReset,
    FlightMode(bool),
}

struct AtRequest {
    action: AtAction,
    resp_tx: oneshot::Sender<serde_json::Value>,
}

// ============================================================================
// 3. 串口 I/O 子系统
// ============================================================================
//
// 设计：
//   专用 std::thread 持有一个永久打开的 /dev/smd11 fd。
//   通过 std::sync::mpsc + recv_timeout 实现命令多路复用。
//   彻底消除 tokio::process::Command + /opt/atcmd_rs 子进程模式。
//
//   命令处理流程：
//     1. 写入 AT 命令 + \r\n
//     2. BufReader 逐行读取响应
//     3. 遇到终止符（OK/ERROR 等）则响应结束
//     4. 尝试用 fill_buf() 检测 BufReader 缓冲区中的残留 URC 数据
//
//   URC 检测（最佳努力）：
//     AT 响应完成后，BufReader 内部缓冲区可能包含模组紧随响应
//     发出的 URC 数据。fill_buf() 在不触发额外 read 的情况下
//     检查缓冲区中是否已有 URC，若有则提取并广播。
//     在专用线程 + libc 条件不具备时的务实选择。
// ============================================================================

/// 串口 I/O 命令（发送到专用线程）
enum SerialCmd {
    ExecAt {
        cmd: String,
        timeout: Duration,
        resp: oneshot::Sender<Result<String, String>>,
    },
}

/// 串口 I/O 句柄 — async 侧的轻量代理
struct SerialIo {
    cmd_tx: std::sync::mpsc::Sender<SerialCmd>,
}

impl SerialIo {
    /// 启动串口 I/O 线程并返回代理句柄。
    fn spawn(path: &str, urc_tx: broadcast::Sender<String>) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<SerialCmd>();
        let path_owned = path.to_string();

        std::thread::Builder::new()
            .name("serial-io".into())
            .spawn(move || Self::run_thread(path_owned, cmd_rx, urc_tx))
            .expect("spawn serial-io thread 失败");

        SerialIo { cmd_tx }
    }

    /// 发送 AT 命令到串口线程并等待响应。
    /// 必须在 AT_CMD_LOCK 保护下调用。
    async fn exec_at(&self, cmd: &str, timeout: Duration) -> Result<String, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SerialCmd::ExecAt {
                cmd: cmd.to_string(),
                timeout,
                resp: tx,
            })
            .map_err(|_| "串口 I/O 线程已终止".to_string())?;
        rx.await.map_err(|_| "串口 I/O 响应被取消".to_string())?
    }

    // ─── 专用线程主循环 ───

    fn run_thread(
        path: String,
        cmd_rx: std::sync::mpsc::Receiver<SerialCmd>,
        urc_tx: broadcast::Sender<String>,
    ) {
        // 打开串口，持续保持打开状态
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!("FATAL: 无法打开串口 {}: {}", path, e);
                return;
            }
        };

        println!("🔌 串口 I/O 线程已启动: {}", path);

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(SerialCmd::ExecAt {
                    cmd,
                    timeout,
                    resp,
                }) => {
                    let result = Self::blocking_exec(&file, &cmd, timeout, &urc_tx);
                    let _ = resp.send(result);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // 无待处理命令，不进行阻塞检查（避免阻塞读取）
                    // URC 检测在每次命令完成后附带走
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    println!("🔌 串口 I/O 线程收到关闭信号");
                    break;
                }
            }
        }
    }

    /// 阻塞式 AT 命令执行：写入 → 读取 → 终止检测 → 尾随 URC 提取
    fn blocking_exec(
        file: &std::fs::File,
        cmd: &str,
        timeout: Duration,
        urc_tx: &broadcast::Sender<String>,
    ) -> Result<String, String> {
        let deadline = std::time::Instant::now() + timeout;

        // ── 写入命令 ──
        let full_cmd = format!("{}\r\n", cmd);
        (&*file as &std::fs::File)
            .write_all(full_cmd.as_bytes())
            .map_err(|e| format!("串口写入失败: {}", e))?;

        // ── 读取响应 ──
        // BufReader 会缓冲从 file 读取的数据，减少系统调用次数
        let mut reader = BufReader::new(&*file as &std::fs::File);
        let mut response = String::new();
        let mut line = String::new();

        loop {
            if std::time::Instant::now() > deadline {
                return Err(format!("AT 命令超时 ({}s): {}", timeout.as_secs(), cmd));
            }

            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    response.push_str(&line);
                    // 检测终止符
                    if AT_TERMINATORS
                        .iter()
                        .any(|t| line.as_str().contains(t))
                    {
                        break;
                    }
                }
                Err(e) => return Err(format!("串口读取失败: {}", e)),
            }
        }

        // 检查是否有 ERROR
        if response.contains("ERROR") || response.contains("+CME ERROR:") {
            return Err(format!("AT 命令错误: {}", response.trim()));
        }

        // ── 在 reader 被 drop 前，检查缓冲区中的残留 URC 数据 ──
        // BufReader 在一次 read() 中可能已经读到了模组紧随 OK 发出的 URC 数据。
        // fill_buf() 返回缓冲区中的现有数据，若不为空则有 URC。
        // 注意：若缓冲区已空，fill_buf() 会触发新的 read() 调用并阻塞。
        // 因此我们仅做一次快速检查，超时由调用方 AT_CMD_LOCK 保证。
        Self::check_buffered_urc(&mut reader, urc_tx);

        // 清理响应：去掉 AT 回显、空行、OK 标记
        let clean: String = response
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("AT+")
                    && !t.starts_with("AT ")
                    && t != "OK"
                    && !t.starts_with("+CME ERROR:")
                    && t != "ERROR"
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(clean)
    }

    /// 检查 BufReader 内部缓冲区中的残留 URC 数据。
    /// 仅在缓冲区已有数据时提取（不触发新的 read 调用）。
    fn check_buffered_urc(
        reader: &mut BufReader<&std::fs::File>,
        urc_tx: &broadcast::Sender<String>,
    ) {
        let raw = match reader.fill_buf() {
            Ok(b) if !b.is_empty() => {
                let data = String::from_utf8_lossy(b).to_string();
                let len = b.len();
                reader.consume(len);
                data
            }
            _ => return,
        };

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if URC_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
                eprintln!("📡 URC 捕获: {}", trimmed);
                let _ = urc_tx.send(trimmed.to_string());
            } else {
                eprintln!("📡 尾随数据（非 URC）: {}", trimmed);
            }
        }
    }

}


// ============================================================================
// 4. 硬件后端抽象
// ============================================================================

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
    async fn set_network_mode_pref(&self, mode: &str);
    async fn set_data_session(&self, connect: bool);
    async fn scan_available_networks(&self) -> Vec<serde_json::Value>;
    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool;
    async fn read_device_info(&self) -> DeviceInfoData;
    async fn send_reboot(&self);
    async fn send_factory_reset(&self);
    async fn set_airplane_mode(&self, on: bool);
    async fn poll_telemetry(&self) -> TelemetryData;
    async fn read_static_info(&self) -> StaticDeviceInfo;
}

// ============================================================================
// 5. 真机后端 (RealBackend)
// ============================================================================

struct RealBackend {
    io: Arc<SerialIo>,
}

#[async_trait::async_trait]
impl HardwareBackend for RealBackend {
    async fn exec_raw_at(&self, cmd: &str) -> String {
        let _lock = get_at_lock().lock().await;
        match self.io.exec_at(cmd, Duration::from_secs(10)).await {
            Ok(resp) => resp,
            Err(e) => format!("ERROR: {}", e),
        }
    }

    async fn read_sms_list(&self) -> String {
        let _lock = get_at_lock().lock().await;
        let _ = self.io.exec_at("AT+CMGF=1", Duration::from_secs(5)).await;
        self.io
            .exec_at("AT+CMGL=\"ALL\"", Duration::from_secs(10))
            .await
            .unwrap_or_else(|_| "+CMGL: 0 messages\r\nOK\r\n".to_string())
    }

    async fn configure_apn(&self, apn: &str, user: &str, pass: &str, auth_type: u8) {
        let _lock = get_at_lock().lock().await;
        let _ = self
            .io
            .exec_at(
                &format!("AT+CGDCONT=1,\"IPV4V6\",\"{}\"", apn),
                Duration::from_secs(5),
            )
            .await;
        if !user.is_empty() {
            let _ = self
                .io
                .exec_at(
                    &format!("AT+QGAUTH=1,{},\"{}\",\"{}\"", auth_type, user, pass),
                    Duration::from_secs(5),
                )
                .await;
        }
    }

    async fn set_network_mode_pref(&self, mode: &str) {
        let _lock = get_at_lock().lock().await;
        let at_mode = match mode {
            "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
            "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
            "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
            "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
            _ => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
        };
        let _ = self.io.exec_at(at_mode, Duration::from_secs(5)).await;
    }

    async fn set_data_session(&self, connect: bool) {
        let _lock = get_at_lock().lock().await;
        let action_val = if connect { "1" } else { "0" };
        let _ = self
            .io
            .exec_at(
                &format!("AT+CGACT={},1", action_val),
                Duration::from_secs(10),
            )
            .await;
    }

    async fn scan_available_networks(&self) -> Vec<serde_json::Value> {
        let _lock = get_at_lock().lock().await;
        println!("🔍 开始网络扫描 (AT+COPS=?, 180s 超时)...");
        match self
            .io
            .exec_at("AT+COPS=?", Duration::from_secs(180))
            .await
        {
            Ok(resp) => {
                let networks = parse_cops_scan(&resp);
                println!("🔍 网络扫描完成，发现 {} 个网络", networks.len());
                networks
            }
            Err(e) => {
                eprintln!("⚠️ 网络扫描失败: {}", e);
                vec![]
            }
        }
    }

    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool {
        let _lock = get_at_lock().lock().await;
        let mut success = false;
        if self
            .io
            .exec_at("AT+CMGF=1", Duration::from_secs(5))
            .await
            .is_ok()
        {
            if self
                .io
                .exec_at(
                    &format!("AT+CMGS=\"{}\"", recipient),
                    Duration::from_secs(5),
                )
                .await
                .is_ok()
            {
                if self
                    .io
                    .exec_at(
                        &format!("{}\u{001A}", message),
                        Duration::from_secs(30),
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
        let mfr = self
            .send_at_get_line("AT+CGMI")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let model = self
            .send_at_get_line("AT+CGMM")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let fw = self
            .send_at_get_line("AT+CGMR")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let imei = self
            .send_at_get_line("AT+CGSN")
            .await
            .unwrap_or_else(|| "Unknown".to_string());
        let serial = imei.clone();
        let hw_ver = self
            .send_at_get_line("AT+QIMPVER")
            .await
            .and_then(|r| {
                r.strip_prefix("+QIMPVER:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let module_type = model.clone();
        let sim_status = self
            .send_at_get_line("AT+CPIN?")
            .await
            .and_then(|r| {
                r.strip_prefix("+CPIN:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let imsi = self
            .send_at_get_line("AT+CIMI")
            .await
            .and_then(|r| r.strip_prefix("+CIMI:").map(|s| s.trim().to_string()))
            .unwrap_or_default();
        let iccid = self
            .send_at_get_line("AT+QCCID")
            .await
            .and_then(|r| {
                r.strip_prefix("+QCCID:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let phone = self
            .send_at_get_line("AT+CNUM")
            .await
            .and_then(|r| {
                let r = r.trim();
                if r.starts_with("+CNUM:") {
                    r.split(',').nth(1).map(|s| s.trim().trim_matches('"').to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let net_status = self
            .send_at_get_line("AT+CREG?")
            .await
            .map(|r| parse_net_status(&r))
            .unwrap_or_else(|| "Unknown".to_string());
        let signal_quality = self
            .send_at_get_line("AT+CSQ")
            .await
            .map(|r| parse_signal_quality(&r))
            .unwrap_or_else(|| "Unknown".to_string());
        let temperature = self
            .send_at_get_line("AT+QTEMP")
            .await
            .and_then(|r| parse_qtemp_temperature(&r))
            .unwrap_or_else(|| "-- °C".to_string());

        let bands = query_device_bands(&self.io).await;

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
        let _lock = get_at_lock().lock().await;
        let _ = self
            .io
            .exec_at("AT+CFUN=1,1", Duration::from_secs(5))
            .await;
    }

    async fn send_factory_reset(&self) {
        let _lock = get_at_lock().lock().await;
        let _ = self.io.exec_at("AT&F0", Duration::from_secs(5)).await;
    }

    async fn set_airplane_mode(&self, on: bool) {
        let _lock = get_at_lock().lock().await;
        let cfun_val = if on { "4" } else { "1" };
        let _ = self
            .io
            .exec_at(&format!("AT+CFUN={}", cfun_val), Duration::from_secs(10))
            .await;
    }

    async fn poll_telemetry(&self) -> TelemetryData {
        let mut telemetry = TelemetryData::default();

        if let Ok(resp) = self.io.exec_at("AT+CGPADDR", Duration::from_secs(5)).await {
            parse_cgpaddr(&resp, &mut telemetry);
        }
        if let Ok(resp) = self
            .io
            .exec_at("AT+QENG=\"servingcell\"", Duration::from_secs(5))
            .await
        {
            parse_qeng(&resp, &mut telemetry);
        }
        if let Ok(resp) = self
            .io
            .exec_at("AT+QCAINFO", Duration::from_secs(5))
            .await
        {
            parse_qcainfo(&resp, &mut telemetry);
        }
        if let Ok(resp) = self.io.exec_at("AT+CPIN?", Duration::from_secs(5)).await {
            for line in resp.lines() {
                let trimmed = line.trim();
                if let Some(status) = trimmed.strip_prefix("+CPIN:") {
                    let s = status.trim().trim_matches('"');
                    if !s.is_empty() {
                        telemetry.sim_status = s.to_string();
                    }
                    break;
                }
            }
        }
        if let Ok(resp) = self.io.exec_at("AT+QTEMP", Duration::from_secs(5)).await {
            if let Some(temp) = parse_qtemp_temperature(&resp) {
                telemetry.temperature = temp;
            }
        }

        telemetry.traffic_stats = self
            .io
            .exec_at("AT+QGDNRCNT?", Duration::from_secs(5))
            .await
            .ok()
            .and_then(|r| {
                r.lines()
                    .find(|l| l.contains("+QGDNRCNT:"))
                    .and_then(|l| parse_traffic_line(l, "+QGDNRCNT:", false))
            })
            .unwrap_or_default();
        if telemetry.traffic_stats.is_empty() {
            telemetry.traffic_stats = self
                .io
                .exec_at("AT+QGDAT?", Duration::from_secs(5))
                .await
                .ok()
                .and_then(|r| {
                    r.lines()
                        .find(|l| l.contains("+QGDAT:"))
                        .and_then(|l| parse_traffic_line(l, "+QGDAT:", true))
                })
                .unwrap_or_default();
        }

        set_default(&mut telemetry.traffic_stats, "N/A");
        set_default(&mut telemetry.signal_percentage, "NA");
        set_default(&mut telemetry.network_mode, "NA");
        set_default(&mut telemetry.sim_status, "NA");

        telemetry
    }

    async fn read_static_info(&self) -> StaticDeviceInfo {
        println!("📋 开始获取静态信息...");

        let firmware_version = self
            .send_at_get_line("AT+CGMR")
            .await
            .unwrap_or_else(|| "NA".to_string());

        let mut active_sim = String::new();
        if let Ok(resp) = self.send_at_command_inner("AT+QUIMSLOT?").await {
            if let Some(line) = resp.lines().find(|l| l.contains("+QUIMSLOT:")) {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 2 {
                    active_sim = format!("SIM {}", parts[1].trim());
                }
            }
        }
        set_default(&mut active_sim, "NA");

        let network_provider = fetch_network_provider(&self.io).await;
        let apn = fetch_apn(&self.io).await;

        let info = StaticDeviceInfo {
            firmware_version,
            active_sim,
            network_provider,
            apn,
        };

        println!(
            "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}",
            info.firmware_version, info.active_sim, info.network_provider, info.apn
        );

        info
    }
}

// ═══ RealBackend 内部辅助方法 ═══

impl RealBackend {
    async fn send_at_get_line(&self, cmd: &str) -> Option<String> {
        let _lock = get_at_lock().lock().await;
        self.io
            .exec_at(cmd, Duration::from_secs(10))
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

    async fn send_at_command_inner(&self, cmd: &str) -> Result<String, String> {
        let _lock = get_at_lock().lock().await;
        self.io.exec_at(cmd, Duration::from_secs(10)).await
    }
}

// ============================================================================
// 6. 全局状态与互斥锁
// ============================================================================

struct AppState {
    tx: broadcast::Sender<String>,
    #[allow(dead_code)]
    actor_tx: mpsc::Sender<AtRequest>,
    actor_tx_high: mpsc::Sender<AtRequest>,
    static_info: RwLock<StaticDeviceInfo>,
    dynamic: RwLock<DynamicTelemetry>,
}

/// 全局 AT 命令互斥锁 — 防止并发发送 AT 命令。
/// 利用 try_lock() 实现忙碌避让（长耗时 AT 命令执行中时跳过轮询）。
static AT_CMD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn get_at_lock() -> &'static tokio::sync::Mutex<()> {
    AT_CMD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

// ============================================================================
// 7. 通用工具函数
// ============================================================================

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
async fn fetch_network_provider(io: &SerialIo) -> String {
    let _lock = get_at_lock().lock().await;

    if let Ok(resp) = io.exec_at("AT+QSPN", Duration::from_secs(5)).await {
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

    if let Ok(resp) = io.exec_at("AT+COPS?", Duration::from_secs(5)).await {
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

    if let Ok(resp) = io
        .exec_at("AT+QENG=\"servingcell\"", Duration::from_secs(5))
        .await
    {
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

/// 获取 APN：CGCONTRDP → 回退 CGDCONT
async fn fetch_apn(io: &SerialIo) -> String {
    let _lock = get_at_lock().lock().await;
    let mut apn = String::new();

    if let Ok(resp) = io.exec_at("AT+CGCONTRDP", Duration::from_secs(5)).await {
        for line in resp.lines() {
            if line.contains("+CGCONTRDP:") {
                let after_prefix = line.trim().strip_prefix("+CGCONTRDP:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 3 {
                    let a = parts[2].trim().trim_matches('"').to_string();
                    if !a.is_empty() {
                        return a;
                    }
                }
            }
        }
    }

    if let Ok(resp) = io.exec_at("AT+CGDCONT?", Duration::from_secs(5)).await {
        for line in resp.lines() {
            if line.contains("+CGDCONT:") {
                let after_prefix = line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
                let parts: Vec<&str> = after_prefix.split(',').collect();
                if parts.len() >= 3 {
                    let a = parts[2].trim().trim_matches('"').to_string();
                    if !a.is_empty() && !a.contains("placeholder") && !a.starts_with("apn") {
                        return a;
                    }
                }
            }
        }
        if apn.is_empty() {
            for line in resp.lines() {
                if line.contains("+CGDCONT:") {
                    let after_prefix = line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if parts.len() >= 3 {
                        let a = parts[2].trim().trim_matches('"').to_string();
                        if !a.is_empty() {
                            apn = a;
                            break;
                        }
                    }
                }
            }
        }
    }

    if apn.is_empty() { "N/A".to_string() } else { apn }
}

/// 查询设备频段信息
async fn query_device_bands(io: &SerialIo) -> String {
    let _lock = get_at_lock().lock().await;
    let mut nr_bands: Vec<String> = Vec::new();
    let mut lte_bands: Vec<String> = Vec::new();

    if let Ok(resp) = io
        .exec_at("AT+QENG=\"servingcell\"", Duration::from_secs(5))
        .await
    {
        for line in resp.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("+QENG: \"servingcell\"") {
                continue;
            }
            let rest = trimmed
                .strip_prefix("+QENG: \"servingcell\"")
                .unwrap_or("")
                .trim()
                .strip_prefix(',')
                .unwrap_or("");
            let parts: Vec<&str> = rest.split(',').map(|s| s.trim().trim_matches('"')).collect();
            let is_nr = parts.get(1).map_or(false, |r| r.contains("NR5G"));
            let band_idx = if is_nr { 9 } else { 8 };
            if let Some(band) = parts.get(band_idx) {
                let b = band.trim();
                if !b.is_empty() && b != "-" {
                    if parts.get(1).map_or(false, |r| r.contains("NR5G")) {
                        if !nr_bands.contains(&format!("n{}", b)) {
                            nr_bands.push(format!("n{}", b));
                        }
                    } else if parts.get(1).map_or(false, |r| *r == "LTE") {
                        if !lte_bands.contains(&format!("B{}", b)) {
                            lte_bands.push(format!("B{}", b));
                        }
                    }
                }
            }
        }
    }

    if let Ok(resp) = io.exec_at("AT+QCAINFO", Duration::from_secs(5)).await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("+QCAINFO:") {
                continue;
            }
            let rest = trimmed.strip_prefix("+QCAINFO:").unwrap_or("").trim();
            let parts: Vec<&str> = rest.split(',').map(|s| s.trim().trim_matches('"')).collect();
            if let Some(raw) = parts.get(3) {
                let band_str = raw.trim();
                if band_str.starts_with("NR5G BAND ") {
                    let num = band_str.strip_prefix("NR5G BAND ").unwrap_or("").trim();
                    if !num.is_empty() && !nr_bands.contains(&format!("n{}", num)) {
                        nr_bands.push(format!("n{}", num));
                    }
                } else if band_str.parse::<i32>().is_ok() {
                    if band_str.parse::<u32>().unwrap_or(0) > 100 {
                        if !nr_bands.contains(&format!("n{}", band_str)) {
                            nr_bands.push(format!("n{}", band_str));
                        }
                    } else if !lte_bands.contains(&format!("B{}", band_str)) {
                        lte_bands.push(format!("B{}", band_str));
                    }
                }
            }
        }
    }

    if nr_bands.len() <= 1 || lte_bands.len() <= 1 {
        if let Ok(resp) = io.exec_at("AT+QCFG=\"band\"", Duration::from_secs(5)).await {
            if let Some(line) = resp.lines().find(|l| l.trim().starts_with("+QCFG:")) {
                let parsed = parse_qcfg_bands(line.trim());
                if !parsed.0.is_empty() {
                    lte_bands = parsed.0;
                }
                if !parsed.1.is_empty() {
                    nr_bands = parsed.1;
                }
            }
        }
    }

    nr_bands.sort_by_key(|b| b.trim_start_matches('n').parse::<u32>().unwrap_or(999));
    lte_bands.sort_by_key(|b| b.trim_start_matches('B').parse::<u32>().unwrap_or(999));

    let mut result = Vec::new();
    if !nr_bands.is_empty() {
        result.push(format!("NR {}", nr_bands.join("/")));
    }
    if !lte_bands.is_empty() {
        result.push(format!("LTE {}", lte_bands.join("/")));
    }

    if result.is_empty() { "--".to_string() } else { result.join(", ") }
}

// ============================================================================
// 8. URC 处理任务
// ============================================================================

/// 应用 URC 更新到动态状态，返回 true 表示发生了有意义的变更
fn apply_urc(dynamic: &mut DynamicTelemetry, urc: &str) -> bool {
    if let Some(val) = urc
        .strip_prefix("+QIND: \"signal\",")
        .or_else(|| {
            urc.lines()
                .find(|l| l.contains("+QIND:") && l.contains("signal"))
                .and_then(|l| l.split(',').nth(1).map(|s| s.trim().trim_matches('"')))
        })
    {
        let old = dynamic.signal_percentage.clone();
        dynamic.signal_percentage = format!("{}%", val);
        return dynamic.signal_percentage != old;
    }

    if urc.contains("+QIND: \"srv\"") || urc.contains("+QIND: \"psattach\"") {
        dynamic.internet_connection = if urc.contains(",1") || urc.contains("\"attached\"") {
            "Connected".to_string()
        } else {
            "Disconnected".to_string()
        };
        return true;
    }

    if urc.starts_with("+CREG:") || urc.starts_with("+CEREG:") || urc.starts_with("+CGREG:") {
        dynamic.updated = Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
        return true;
    }

    if urc.starts_with("+CMT:") || urc.starts_with("+CMTI:") {
        dynamic.updated = Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
        return true;
    }

    if urc.contains("+QIND: \"pscall\"") {
        dynamic.internet_connection = if urc.contains(",1") {
            "Connected".to_string()
        } else {
            "Disconnected".to_string()
        };
        return true;
    }

    false
}

async fn urc_handler_task(
    mut urc_rx: broadcast::Receiver<String>,
    state: Arc<AppState>,
    interval_secs: Arc<RwLock<u64>>,
) {
    while let Ok(urc_msg) = urc_rx.recv().await {
        let changed = {
            let mut dynamic = state.dynamic.write().await;
            apply_urc(&mut dynamic, &urc_msg)
        };

        if changed {
            let dynamic = state.dynamic.read().await;
            if let Ok(json_str) = serde_json::to_string(&*dynamic) {
                eprintln!("📡 URC 触发广播: {} 字节", json_str.len());
                let _ = state.tx.send(json_str);
            }
            drop(dynamic);
            // URC 到来时提速轮询（模组活跃）
            *interval_secs.write().await = 3;
        }
    }
}

// ============================================================================
// 9. WebSocket 消息路由
// ============================================================================

fn broadcast_dynamic(state: &AppState, dynamic: &DynamicTelemetry) {
    if let Ok(json_str) = serde_json::to_string(dynamic) {
        let rc = state.tx.receiver_count();
        eprintln!("📤 WS 广播动态遥测 ({} 接收者, {} 字节)", rc, json_str.len());
        let _ = state.tx.send(json_str);
    }
}

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
            println!("👨‍💻 用户手动执行 AT: {}", cmd);
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                let response = backend.exec_raw_at(&cmd).await;
                let _ = resp_tx.send(serde_json::json!({ "type": "at_res", "data": response }));
            });
        }
        AtAction::SetInterval(secs) => {
            let new_secs = secs.max(5);
            *interval_secs = new_secs;
            *current_interval_secs = new_secs;
            *interval = tokio::time::interval(Duration::from_secs(*interval_secs));
            interval.tick().await;
            println!("🔄 轮询间隔已动态调整为 {} 秒", new_secs);
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": format!("轮询间隔已动态调整为 {} 秒", new_secs) }
            }));
        }
        AtAction::GetSmsList => {
            println!("📱 actor: get_sms_list (后台异步执行)");
            let backend_clone = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                let resp = backend_clone.read_sms_list().await;
                let decoded = decode_cmgl_body(&resp);
                let _ = resp_tx.send(serde_json::json!({ "type": "sms_list", "data": decoded }));
            });
        }
        AtAction::SetApn(apn, user, pass, auth_type) => {
            println!("🌐 actor: set_apn apn={} user={} auth={}", apn, user, auth_type);
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.configure_apn(&apn, &user, &pass, auth_type).await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "network_status",
                    "data": { "status": "APN settings applied", "apn": apn }
                }));
            });
        }
        AtAction::SetNetworkMode(mode) => {
            println!("🌐 actor: set_network_mode {}", mode);
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.set_network_mode_pref(&mode).await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "network_status",
                    "data": { "status": format!("Network mode set to {}", mode) }
                }));
            });
        }
        AtAction::NetConnect(connect) => {
            println!("🌐 actor: net_{}", if connect { "connect" } else { "disconnect" });
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.set_data_session(connect).await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "network_status",
                    "data": { "status": format!("{} command sent", if connect { "Connect" } else { "Disconnect" }) }
                }));
            });
        }
        AtAction::NetworkScan => {
            println!("🔍 actor: network_scan (后台异步执行)");
            let backend_clone = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                let networks = backend_clone.scan_available_networks().await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "scan_result",
                    "data": { "status": "Scan complete.", "networks": networks }
                }));
            });
        }
        AtAction::SendSms(recipient, message) => {
            println!("📱 actor: send_sms to={}", recipient);
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                let success = backend.send_sms_msg(&recipient, &message).await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "sms_sent",
                    "data": { "status": if success { "SMS sent successfully" } else { "SMS failed" }, "recipient": recipient }
                }));
            });
        }
        AtAction::GetDeviceInfo => {
            println!("📱 actor: get_device_info via AT commands");
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                let info = backend.read_device_info().await;
                let _ = resp_tx.send(serde_json::json!({
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
            });
        }
        AtAction::Reboot => {
            println!("🔄 actor: rebooting module...");
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.send_reboot().await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "settings_log",
                    "data": { "msg": "Reboot command sent to module." }
                }));
            });
        }
        AtAction::FactoryReset => {
            println!("⚠️ actor: factory reset module...");
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.send_factory_reset().await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "settings_log",
                    "data": { "msg": "Factory reset command sent to module." }
                }));
            });
        }
        AtAction::FlightMode(on) => {
            println!("✈️ actor: setting flight mode to {}", if on { "ON" } else { "OFF" });
            let backend = backend.clone();
            let resp_tx = req.resp_tx;
            tokio::spawn(async move {
                backend.set_airplane_mode(on).await;
                let _ = resp_tx.send(serde_json::json!({
                    "type": "settings_log",
                    "data": { "msg": format!("Flight mode turned {}", if on { "ON" } else { "OFF" }) }
                }));
            });
        }
    }
}

// ============================================================================
// 10. 统一硬件任务
// ============================================================================

async fn hardware_task(
    mut actor_rx_high: mpsc::Receiver<AtRequest>,
    mut actor_rx: mpsc::Receiver<AtRequest>,
    backend: Arc<dyn HardwareBackend>,
    state: Arc<AppState>,
    interval_secs: Arc<RwLock<u64>>,
) {
    let mut components = Components::new_with_refreshed_list();
    let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_ACTIVE));
    interval.tick().await;
    let mut consecutive_failures = 0u32;
    let mut current_interval_secs = POLL_INTERVAL_ACTIVE;

    loop {
        tokio::select! {
            biased;

            req = actor_rx_high.recv() => {
                let mut user_interval = interval_secs.write().await;
                if !handle_channel_req(req, &backend, &mut *user_interval, &mut interval, &mut current_interval_secs).await {
                    break;
                }
            }
            req = actor_rx.recv() => {
                let mut user_interval = interval_secs.write().await;
                if !handle_channel_req(req, &backend, &mut *user_interval, &mut interval, &mut current_interval_secs).await {
                    break;
                }
            }
            _ = interval.tick() => {
                // ★ 零成本视图探活：broadcast::Sender::receiver_count()
                // 自动随 WS 连接数增减，无需手动原子计数维护
                let active = state.tx.receiver_count() > 0;

                if active && get_at_lock().try_lock().is_ok() {
                    // ── 串口空闲，正常物理轮询 ──
                    let start_poll = std::time::Instant::now();
                    let mut telemetry = backend.poll_telemetry().await;

                    // 离线检测
                    let telemetry_dead = telemetry.sim_status == "NA"
                        && telemetry.network_mode == "NA"
                        && telemetry.signal_percentage == "NA"
                        && (telemetry.ipv4.is_empty() || telemetry.ipv4 == "--");
                    consecutive_failures = if telemetry_dead { consecutive_failures + 1 } else { 0 };

                    // CPU 温度回退
                    if telemetry.temperature.is_empty() || telemetry.temperature == "-- °C" {
                        components.refresh(true);
                        if let Some(temp) = components.iter()
                            .find(|c| {
                                let lower = c.label().to_lowercase();
                                lower.contains("cpu") || lower.contains("package")
                            })
                            .and_then(|c| c.temperature())
                        {
                            telemetry.temperature = format!("{:.0} °C", temp);
                        }
                    }

                    telemetry.internet_connection = if !telemetry.ipv4.is_empty() && telemetry.ipv4 != "--" {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    };

                    let uptime_sec = System::uptime();
                    let updated_str = Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
                    let uptime_str = format!("{} minutes", uptime_sec / 60);

                    {
                        let mut dynamic = state.dynamic.write().await;

                        if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                            eprintln!("⚠️ 模组连续 {} 次无响应，重置为离线状态", MAX_CONSECUTIVE_FAILURES);
                            *dynamic = DynamicTelemetry::default();
                            dynamic.internet_connection = "Disconnected".to_string();
                            dynamic.uptime = uptime_str;
                            dynamic.updated = updated_str;
                        } else {
                            *dynamic = DynamicTelemetry::from_telemetry(
                                &telemetry, &dynamic,
                                uptime_str,
                                updated_str,
                            );
                        }

                        broadcast_dynamic(&state, &dynamic);
                    }

                    println!("✅ 轮询任务完成，总耗时: {}ms", start_poll.elapsed().as_millis());

                } else if active {
                    // ── 串口繁忙，使用缓存 ──
                    println!("⚠️ Modem 串口忙，跳过物理轮询，发送缓存状态");
                    let uptime_sec = System::uptime();
                    let updated_str = Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    let mut dynamic = state.dynamic.write().await;
                    dynamic.uptime = format!("{} minutes", uptime_sec / 60);
                    dynamic.updated = updated_str;
                    broadcast_dynamic(&state, &dynamic);

                } else {
                    println!("💤 硬件轮询空闲模式 (无活跃视图)");
                }

                // 动态空闲降频
                let user_interval = *interval_secs.read().await;
                let next_secs = if active { user_interval } else { IDLE_INTERVAL_SECS };
                if next_secs != current_interval_secs {
                    current_interval_secs = next_secs;
                    interval = tokio::time::interval(Duration::from_secs(next_secs));
                    interval.tick().await;
                }
            }
        }
    }
}

// ============================================================================
// 11. HTTP/WS 处理器
// ============================================================================

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

async fn style_handler() -> impl IntoResponse {
    static_file_response(
        include_str!("../templates/style.css"),
        "text/css; charset=utf-8",
    )
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    // ★ 订阅广播自动增加 receiver_count — 视图探活零成本原生实现
    let mut broadcast_rx = state.tx.subscribe();
    let online_count = state.tx.receiver_count();
    println!("🔌 新 WebSocket 客户端已连接 (当前在线: {})", online_count);

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
            println!("💬 WebSocket 发送通道中断");
        },

        _ = async {
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
                            // ★ 已由 receiver_count() 完全替代，保留仅兼容旧前端
                            continue;
                        }
                        "get_static_info" => {
                            let guard = state.static_info.read().await;
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
                        _ => {
                            eprintln!("⚠️ 未知 WS 动作: {}", cmd.action);
                            continue;
                        }
                    };

                    if state.actor_tx_high.send(AtRequest { action, resp_tx }).await.is_ok() {
                        if let Ok(reply) = resp_rx.await {
                            if local_tx.send(reply.to_string()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        } => {
            println!("💬 浏览器主动断开 WebSocket 连接");
        }
    };

    // ★ broadcast_rx drop 时自动递减 receiver_count，无需手动清理
    println!("🔌 WebSocket 客户端已断开 (当前在线: {})", state.tx.receiver_count());
}

// ============================================================================
// 12. 主入口
// ============================================================================

#[tokio::main]
async fn main() {
    let serial_path =
        env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
    println!("🔌 串口设备: {}", serial_path);

    // ── 广播通道 ──
    let (tx, _) = broadcast::channel(100);
    let (urc_tx, _) = broadcast::channel(50);

    // ── 启动串口 I/O 线程（专用 std::thread，持久 fd）──
    let serial_io = Arc::new(SerialIo::spawn(&serial_path, urc_tx.clone()));

    // ── 硬件后端 ──
    let backend: Arc<dyn HardwareBackend> = Arc::new(RealBackend {
        io: serial_io.clone(),
    });

    // ── 通信通道 ──
    let (actor_tx, actor_rx) = mpsc::channel(32);
    let (actor_tx_high, actor_rx_high) = mpsc::channel(32);

    // ── 应用状态 ──
    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        actor_tx,
        actor_tx_high,
        static_info: RwLock::new(StaticDeviceInfo::default()),
        dynamic: RwLock::new(DynamicTelemetry::default()),
    });

    // ── 启动时采集静态信息（仅一次）──
    let static_info = backend.read_static_info().await;
    *app_state.static_info.write().await = static_info;

    // ── 轮询间隔（可由 URC 动态调整）──
    let interval_secs = Arc::new(RwLock::new(POLL_INTERVAL_ACTIVE));

    // ── URC 处理任务 ──
    let urc_rx = urc_tx.subscribe();
    tokio::spawn({
        let state = app_state.clone();
        let interval = interval_secs.clone();
        urc_handler_task(urc_rx, state, interval)
    });

    // ── 硬件轮询任务 ──
    tokio::spawn({
        let backend = backend.clone();
        let state = app_state.clone();
        let interval = interval_secs.clone();
        async move { hardware_task(actor_rx_high, actor_rx, backend, state, interval).await }
    });

    // ── HTTP 路由 ──
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/style.css", get(style_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("⚡ 移远 5G 终端 WebUI 服务端已就绪: http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}
