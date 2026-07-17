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

use at::{
    parse_cgpaddr, parse_cops_scan, parse_net_status, parse_qcainfo, parse_qcfg_bands,
    parse_qeng, parse_qtemp_temperature, parse_signal_quality, parse_traffic_line,
    decode_cmgl_body, decode_hex_ucs2, normalize_at_command, set_default,
};
use fs2::FileExt;

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
    /// 全局状态 — fetch_static_info 和 hardware_polling_actor 获取的所有值均汇聚于此，ws.send 全部从此读取
    global: RwLock<GlobalTelemetry>,
}

#[derive(Clone, Serialize, Debug)]
struct GlobalTelemetry {
    firmware_version: String,
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

impl Default for GlobalTelemetry {
    fn default() -> Self {
        Self {
            firmware_version: "NA".to_string(),
            temperature: "NA".to_string(),
            sim_status: "NA".to_string(),
            signal_percentage: "NA".to_string(),
            internet_connection: "NA".to_string(),
            active_sim: "NA".to_string(),
            network_provider: "NA".to_string(),
            mccmnc: "NA".to_string(),
            apn: "NA".to_string(),
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

impl GlobalTelemetry {
    /// 从 TelemetryData（解析器填充）和当前全局状态合并构造 GlobalTelemetry。
    /// 动态字段仅当解析器返回有效值（非空、非 NA/N/A/--）时才覆盖，
    /// 否则保留上一次的全局值，避免一次 AT 失败就冲掉全部数据。
    /// 静态字段始终从 global 保留。
    fn from_telemetry_and_global(t: &TelemetryData, g: &GlobalTelemetry, uptime: String, updated: String) -> Self {
        fn keep_valid(new: &str, old: &str) -> String {
            if !new.is_empty() && new != "NA" && new != "N/A" && new != "--" {
                new.to_string()
            } else {
                old.to_string()
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
            // 静态字段从已有全局状态保留
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

// ============================================================================
// HardwareBackend trait + RealBackend（真机后端，失败返回 NA）
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
    /// 手动执行 AT 命令（ManualAt）
    async fn exec_raw_at(&self, cmd: &str) -> String;

    /// 获取 SMS 列表（原始 CMGL 响应）
    async fn read_sms_list(&self) -> String;

    /// 配置 APN
    async fn configure_apn(&self, apn: &str, user: &str, pass: &str, auth_type: u8);

    /// 设置网络模式偏好
    async fn set_network_mode_pref(&self, mode: &str);

    /// 设置数据会话连接/断开
    async fn set_data_session(&self, connect: bool);

    /// 执行网络扫描，返回可用网络列表
    async fn scan_available_networks(&self) -> Vec<serde_json::Value>;

    /// 发送 SMS，返回成功与否
    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool;

    /// 读取设备详细信息
    async fn read_device_info(&self) -> DeviceInfoData;

    /// 发送重启命令
    async fn send_reboot(&self);

    /// 发送恢复出厂设置命令
    async fn send_factory_reset(&self);

    /// 设置飞行模式
    async fn set_airplane_mode(&self, on: bool);

    /// 轮询采集遥测数据
    async fn poll_telemetry(&self) -> TelemetryData;

    /// 启动时读取静态信息 (fw, sim, provider, apn)
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

    async fn set_network_mode_pref(&self, mode: &str) {
        let at_mode = match mode {
            "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
            "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
            "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
            "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
            _ => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
        };
        let _ = send_at_command_inner(&self.serial_path, at_mode).await;
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
        println!("🔍 开始网络扫描 (AT+COPS=?, 180s 超时)...");
        match send_at_command_inner_with_timeout(
            &self.serial_path,
            "AT+COPS=?",
            Duration::from_secs(180),
        )
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
        let hw_ver = send_at_get_line(&self.serial_path, "AT+QIMPVER")
            .await
            .and_then(|r| {
                r.strip_prefix("+QIMPVER:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let module_type = model.clone();
        let sim_status = send_at_get_line(&self.serial_path, "AT+CPIN?")
            .await
            .and_then(|r| {
                r.strip_prefix("+CPIN:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let imsi = send_at_get_line(&self.serial_path, "AT+CIMI")
            .await
            .and_then(|r| r.strip_prefix("+CIMI:").map(|s| s.trim().to_string()))
            .unwrap_or_default();
        let iccid = send_at_get_line(&self.serial_path, "AT+QCCID")
            .await
            .and_then(|r| {
                r.strip_prefix("+QCCID:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let phone = send_at_get_line(&self.serial_path, "AT+CNUM")
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
            for line in cpin_resp.lines() {
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
        if let Ok(qtemp_resp) = send_at_command_dedup(&self.serial_path, "AT+QTEMP").await {
            if let Some(temp) = parse_qtemp_temperature(&qtemp_resp) {
                telemetry.temperature = temp;
            }
        }

        telemetry.traffic_stats = send_at_command_dedup(&self.serial_path, "AT+QGDNRCNT?")
            .await
            .ok()
            .and_then(|r| {
                r.lines()
                    .find(|l| l.contains("+QGDNRCNT:"))
                    .and_then(|l| parse_traffic_line(l, "+QGDNRCNT:", false))
            })
            .unwrap_or_default();
        if telemetry.traffic_stats.is_empty() {
            telemetry.traffic_stats = send_at_command_dedup(&self.serial_path, "AT+QGDAT?")
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

    async fn read_static_info(&self) -> (String, String, String, String) {
        println!("📋 开始获取静态信息（持久串口连接）...");

        let firmware_version;
        let mut active_sim = String::new();
        let mut apn = String::new();

        println!("📋 [1/4] 获取固件版本 (AT+CGMR)...");
        firmware_version = send_at_get_line(&self.serial_path, "AT+CGMR")
            .await
            .unwrap_or_else(|| "NA".to_string());

        println!("📋 [2/5] 获取 SIM 槽位 (AT+QUIMSLOT?)...");
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+QUIMSLOT?").await {
            if let Some(line) = resp.lines().find(|l| l.contains("+QUIMSLOT:")) {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 2 {
                    active_sim = format!("SIM {}", parts[1].trim());
                }
            }
        }
        set_default(&mut active_sim, "NA");

        println!("📋 [3/5] 获取运营商...");
        let network_provider = fetch_network_provider(&self.serial_path).await;

        println!("📋 [4/5] 获取 APN (AT+CGCONTRDP)...");
        let mut found_apn = false;
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+CGCONTRDP").await {
            for line in resp.lines() {
                if line.contains("+CGCONTRDP:") {
                    let after_prefix = line.trim().strip_prefix("+CGCONTRDP:").unwrap_or(line).trim();
                    let parts: Vec<&str> = after_prefix.split(',').collect();
                    if parts.len() >= 3 {
                        let a = parts[2].trim().trim_matches('"').to_string();
                        if !a.is_empty() {
                            apn = a;
                            found_apn = true;
                            break;
                        }
                    }
                }
            }
        }
        if !found_apn {
            println!("📋 [4/5] 回退 AT+CGDCONT? 获取 APN...");
            if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+CGDCONT?").await {
                for line in resp.lines() {
                    if line.contains("+CGDCONT:") {
                        let after_prefix = line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
                        let parts: Vec<&str> = after_prefix.split(',').collect();
                        if parts.len() >= 3 {
                            let a = parts[2].trim().trim_matches('"').to_string();
                            if !a.is_empty()
                                && !a.contains("placeholder")
                                && !a.starts_with("apn")
                            {
                                apn = a;
                                found_apn = true;
                                break;
                            }
                        }
                    }
                }
                if !found_apn {
                    for line in resp.lines() {
                        if line.contains("+CGDCONT:") {
                            let after_prefix =
                                line.trim().strip_prefix("+CGDCONT:").unwrap_or(line).trim();
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
        }
        set_default(&mut apn, "N/A");

        println!(
            "📋 静态信息已获取: FW={}, SIM={}, Provider={}, APN={}",
            firmware_version, active_sim, network_provider, apn
        );

        (firmware_version, active_sim, network_provider, apn)
    }
}


// ============================================================================
// 通用工具函数
// ============================================================================

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

// ============================================================================
// AT 命令基础架构（互斥锁 + 去重队列 + 跨进程文件锁）
// ============================================================================

/// 跨进程文件锁 — 防止其他进程（如后台监控守护进程）同时操作串口
struct SerialFileLock {
    file: Option<std::fs::File>,
}

impl Drop for SerialFileLock {
    fn drop(&mut self) {
        // 文件关闭时内核自动释放 flock；显式 unlock 更可靠
        if let Some(ref file) = self.file {
            let _ = file.unlock();
        }
    }
}

/// 锁文件路径（位于 tmpfs，重启自动清理）
const LOCK_FILE_PATH: &str = "/tmp/atcmd_rs.lock";

/// 获取跨进程排他文件锁。
/// 同一时间仅允许一个进程持有锁，其他进程的 flock(LOCK_EX) 在内核态阻塞等待。
async fn acquire_serial_lock() -> Result<SerialFileLock, String> {
    let result = tokio::task::spawn_blocking(|| -> Result<std::fs::File, String> {
        // 以 CREATE+WRITE 模式打开锁文件，保证文件始终存在
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(LOCK_FILE_PATH)
            .map_err(|e| format!("无法打开锁文件 {}: {}", LOCK_FILE_PATH, e))?;

        // LOCK_EX：排他锁，阻塞等待直到获取到锁
        file.lock_exclusive()
            .map_err(|e| format!("获取 {} 排他锁失败: {}", LOCK_FILE_PATH, e))?;

        Ok(file)
    })
    .await
    .map_err(|e| format!("文件锁任务被取消: {}", e))??;

    Ok(SerialFileLock { file: Some(result) })
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
                rx.await.unwrap_or_else(|_| Err("AT命令队列等待被取消".to_string()))
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

/// 底层 AT 命令发送（支持自定义超时）
async fn send_at_command_inner_with_timeout(
    serial_path: &str,
    cmd: &str,
    timeout: Duration,
) -> Result<String, String> {
    let start = std::time::Instant::now();

    let _lock = get_at_lock().lock().await;

    // 跨进程排他锁：防止其他后台进程（如监控守护）同时操作 /dev/smd11
    // 导致数据被劫持或响应混乱
    let _file_lock = acquire_serial_lock().await?;

    let child = tokio::process::Command::new("/opt/atcmd_rs")
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

    // 检测错误（在原始响应中搜索 ERROR/+CME ERROR:）
    if raw.contains("ERROR") || raw.contains("+CME ERROR:") {
        let elapsed = start.elapsed();
        println!("⚠️ [AT] {} 返回 ERROR ({}ms)", cmd, elapsed.as_millis());
        return Err(format!("AT命令返回错误: {}", raw.trim()));
    }

    let elapsed = start.elapsed();
    // 取首行非空非 AT 回显的内容作为日志预览
    let preview = raw
        .lines()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("AT+")
        })
        .unwrap_or(&raw);
    println!(
        "✅ [AT] {} 成功 ({}ms): {}",
        cmd,
        elapsed.as_millis(),
        preview.trim()
    );
    Ok(raw)
}

/// 底层 AT 命令发送（默认 10 秒超时）
async fn send_at_command_inner(serial_path: &str, cmd: &str) -> Result<String, String> {
    send_at_command_inner_with_timeout(serial_path, cmd, Duration::from_secs(10)).await
}

/// 从设备查询频段信息（NR + LTE），通过多条 AT 命令从模组获取
async fn query_device_bands(serial_path: &str) -> String {
    // ── 方法 1: AT+QENG="servingcell" 获取当前服务小区频段 ──
    let mut nr_bands: Vec<String> = Vec::new();
    let mut lte_bands: Vec<String> = Vec::new();

    if let Ok(resp) = send_at_command_dedup(serial_path, "AT+QENG=\"servingcell\"").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("+QENG: \"servingcell\"") {
                continue;
            }
            // 格式: +QENG: "servingcell","NOCONN","NR5G-SA","TDD",460,00,...,<band>,...
            // band 字段位置: LTE=9, NR5G=9 (基于现有 parser 的索引)
            let rest = trimmed
                .strip_prefix("+QENG: \"servingcell\"")
                .unwrap_or("")
                .trim()
                .strip_prefix(',')
                .unwrap_or("");
            let parts: Vec<&str> = rest.split(',').map(|s| s.trim().trim_matches('"')).collect();
            // parts[0]=connection_status, parts[1]=rat
            // LTE: band at index 8, NR5G: band at index 9 (extra tac field)
            let band_idx = if parts.get(1).map_or(false, |r| r.contains("NR5G")) { 9 } else { 8 };
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

    // ── 方法 2: AT+QCAINFO 补充载波聚合频段 ──
    if let Ok(resp) = send_at_command_dedup(serial_path, "AT+QCAINFO").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("+QCAINFO:") {
                continue;
            }
            let rest = trimmed.strip_prefix("+QCAINFO:").unwrap_or("").trim();
            let parts: Vec<&str> = rest.split(',').map(|s| s.trim().trim_matches('"')).collect();
            // parts[3]=band, e.g. "NR5G BAND 41" or just "41"
            if let Some(raw) = parts.get(3) {
                let band_str = raw.trim();
                if band_str.starts_with("NR5G BAND ") {
                    let num = band_str.strip_prefix("NR5G BAND ").unwrap_or("").trim();
                    if !num.is_empty() && !nr_bands.contains(&format!("n{}", num)) {
                        nr_bands.push(format!("n{}", num));
                    }
                } else if let Ok(_) = band_str.parse::<i32>() {
                    // Could be NR or LTE — try to classify by value
                    if band_str.parse::<u32>().unwrap_or(0) > 100 {
                        // >100 are NR band numbers
                        if !nr_bands.contains(&format!("n{}", band_str)) {
                            nr_bands.push(format!("n{}", band_str));
                        }
                    } else {
                        if !lte_bands.contains(&format!("B{}", band_str)) {
                            lte_bands.push(format!("B{}", band_str));
                        }
                    }
                }
            }
        }
    }

    // ── 方法 3: AT+QCFG="band" 获取配置的完整频段列表 ──
    if nr_bands.len() <= 1 || lte_bands.len() <= 1 {
        if let Ok(resp) = send_at_command_dedup(serial_path, "AT+QCFG=\"band\"").await {
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

    // ── 格式化输出 ──
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

// ============================================================================
// 统一硬件任务：串行消费 /opt/atcmd_rs，定时轮询 + 用户指令均经此通道
// ============================================================================

/// 统一硬件任务：定时轮询 + 用户 AT 指令，全部串行处理
async fn hardware_task(
    mut actor_rx: mpsc::Receiver<AtRequest>,
    backend: Arc<dyn HardwareBackend>,
    state: Arc<AppState>,
) {
    let mut components = Components::new_with_refreshed_list();
    let mut interval_secs = 3u64;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await; // 首次 tick 立即完成

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if state.active_views.load(Ordering::SeqCst) > 0 {
                    let start_poll = std::time::Instant::now();
                    let mut telemetry = backend.poll_telemetry().await;

                    // CPU 温度 fallback（仅当 AT+QTEMP 未提供时）
                    if telemetry.temperature.is_empty() || telemetry.temperature == "-- °C" {
                        components.refresh(true);
                        if let Some(temp) = components.iter()
                            .find(|c| {
                                let label = c.label();
                                let lower = label.to_lowercase();
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
                    let updated_str = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    // 合并到全局状态并广播给所有 ws 客户端
                    {
                        let mut global = state.global.write().await;
                        *global = GlobalTelemetry::from_telemetry_and_global(
                            &telemetry, &global,
                            format!("{} minutes", uptime_sec / 60),
                            updated_str,
                        );

                        if let Ok(json_str) = serde_json::to_string(&*global) {
                            let send_rc = state.tx.receiver_count();
                            eprintln!("📤 WS 广播遥测数据 ({} 接收者, {} 字节)",
                                send_rc, json_str.len());
                            let _ = state.tx.send(json_str);
                        }
                    }

                    println!("✅ 轮询任务完成，总耗时: {}ms", start_poll.elapsed().as_millis());
                } else {
                    println!("💤 硬件轮询处于空闲模式 (无活跃视图)");
                }
            }
            req = actor_rx.recv() => {
                let req = match req {
                    Some(r) => r,
                    None => break, // 通道关闭，退出任务
                };
                match req.action {
                    AtAction::ManualAt(cmd) => {
                        let cmd = normalize_at_command(&cmd);
                        println!("👨‍💻 用户手动执行 AT: {}", cmd);
                        let response = backend.exec_raw_at(&cmd).await;
                        let _ = req.resp_tx.send(serde_json::json!({ "type": "at_res", "data": response }));
                    }
                    AtAction::SetInterval(secs) => {
                        let new_secs = secs.max(3);
                        interval_secs = new_secs;
                        interval = tokio::time::interval(Duration::from_secs(interval_secs));
                        interval.tick().await;
                        println!("🔄 轮询间隔已动态调整为 {} 秒", new_secs);
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": format!("轮询间隔已动态调整为 {} 秒", new_secs) }
                        }));
                    }
                    AtAction::GetSmsList => {
                        println!("📱 actor: get_sms_list via AT+CMGL");
                        let resp = backend.read_sms_list().await;
                        let decoded = decode_cmgl_body(&resp);
                        let _ = req.resp_tx.send(serde_json::json!({ "type": "sms_list", "data": decoded }));
                    }
                    AtAction::SetApn(apn, user, pass, auth_type) => {
                        println!("🌐 actor: set_apn apn={} user={} auth={}", apn, user, auth_type);
                        backend.configure_apn(&apn, &user, &pass, auth_type).await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": "APN settings applied", "apn": apn }
                        }));
                    }
                    AtAction::SetNetworkMode(mode) => {
                        println!("🌐 actor: set_network_mode {}", mode);
                        backend.set_network_mode_pref(&mode).await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("Network mode set to {}", mode) }
                        }));
                    }
                    AtAction::NetConnect(connect) => {
                        println!("🌐 actor: net_{}", if connect { "connect" } else { "disconnect" });
                        backend.set_data_session(connect).await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "network_status",
                            "data": { "status": format!("{} command sent", if connect { "Connect" } else { "Disconnect" }) }
                        }));
                    }
                    AtAction::NetworkScan => {
                        println!("🔍 actor: network_scan (后台异步执行，不阻塞轮询)");
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
                        let success = backend.send_sms_msg(&recipient, &message).await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "sms_sent",
                            "data": { "status": if success { "SMS sent successfully" } else { "SMS failed" }, "recipient": recipient }
                        }));
                    }
                    AtAction::GetDeviceInfo => {
                        println!("📱 actor: get_device_info via AT commands");
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
                        println!("🔄 actor: rebooting module...");
                        backend.send_reboot().await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Reboot command sent to module." }
                        }));
                    }
                    AtAction::FactoryReset => {
                        println!("⚠️ actor: factory reset module...");
                        backend.send_factory_reset().await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": "Factory reset command sent to module." }
                        }));
                    }
                    AtAction::FlightMode(on) => {
                        println!("✈️ actor: setting flight mode to {}", if on { "ON" } else { "OFF" });
                        backend.set_airplane_mode(on).await;
                        let _ = req.resp_tx.send(serde_json::json!({
                            "type": "settings_log",
                            "data": { "msg": format!("Flight mode turned {}", if on { "ON" } else { "OFF" }) }
                        }));
                    }
                }
            }
        }
    }
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
        global: RwLock::new(GlobalTelemetry::default()),
    });

    // 创建后端（设备不存在时命令失败返回 NA）
    let backend: Arc<dyn HardwareBackend> = Arc::new(RealBackend { serial_path: serial_path.clone() });

    // 获取一次静态信息
    let (firmware_version, active_sim, network_provider, apn) = backend.read_static_info().await;
    {
        let mut guard = app_state.global.write().await;
        guard.firmware_version = firmware_version;
        guard.active_sim = active_sim;
        guard.network_provider = network_provider;
        guard.apn = apn;
    }

    // 启动统一硬件任务（单消费者，串行处理所有 /opt/atcmd_rs）
    tokio::spawn({
        let backend = backend.clone();
        let state = app_state.clone();
        async move { hardware_task(actor_rx, backend, state).await }
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

