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
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::{env, sync::Arc};
use sysinfo::{Components, System};
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

// ============================================================================
// 模组闪存生命保护：内存环形滚动日志系统 (RAM Ring Buffer)
// ============================================================================
static MEMORY_LOGS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn get_logs() -> &'static Mutex<VecDeque<String>> {
    MEMORY_LOGS.get_or_init(|| Mutex::new(VecDeque::with_capacity(100)))
}

/// 统一日志打印接口：内存存储 + 标准输出（不伤 Flash 寿命）
pub fn push_log(level: &str, module: &str, msg: &str) {
    let now = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
    let formatted = format!("[{}] [{}] [{}]: {}", now, level, module, msg);

    // 打印到标准输出/标准错误，供系统 journald 收集（内存缓存，不写入闪存）
    if level == "ERROR" {
        eprintln!("{}", formatted);
    } else {
        println!("{}", formatted);
    }

    // 压入内存环形队列（超出 100 行自动挤出最老数据）
    if let Ok(mut logs) = get_logs().lock() {
        if logs.len() >= 100 {
            logs.pop_front();
        }
        logs.push_back(formatted);
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
    /// (is_nr5g, bands) — true=NR5G, false=LTE; "all" 或 "" 恢复所有频段
    SetBandLock { is_nr5g: bool, bands: String },
    /// (tech, pci, earfcn, band, enable) — enable=false 解除锁定
    SetCellLock { tech: String, pci: u32, earfcn: u32, band: Option<u32>, enable: bool },
    /// USB 网络模式（有效值: 0,1,2,3,5）
    SetUsbNet(u8),
    /// SIM 卡槽编号
    SwitchSimSlot(u8),
    /// 诊断子命令
    GetDiagnostics(DiagnosticType),
}

struct AtRequest {
    action: AtAction,
    resp_tx: oneshot::Sender<serde_json::Value>,
}

struct AppState {
    /// 广播 Arc 包装的遥测实体，减少硬件线程中的序列化损耗
    tx: broadcast::Sender<Arc<GlobalTelemetry>>,
    /// 扁平、高优先级的单一串行指令通道
    command_tx: mpsc::Sender<AtRequest>,
    active_views: AtomicUsize,
    /// 全局状态 — fetch_static_info 和 hardware_polling_actor 获取的所有值均汇聚于此，ws.send 全部从此读取
    global: RwLock<GlobalTelemetry>,
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
    /// 动态字段仅当解析器返回有效值（非空、非 NA/N/A/--）时才覆盖，
    /// 否则保留上一次的全局值，避免一次 AT 失败就冲掉全部数据。
    /// 静态字段始终从 global 保留。
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
    /// 手动执行 AT 命令（ManualAt）
    async fn exec_raw_at(&self, cmd: &str) -> String;

    /// 获取 SMS 列表（原始 CMGL 响应）
    async fn read_sms_list(&self) -> String;

    /// 配置 APN
    async fn configure_apn(&self, apn: &str, user: &str, pass: &str, auth_type: u8);

    /// 设置网络模式偏好（支持 SA/NSA 切换）
    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String>;

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

    /// 设置频段锁定（is_nr5g=true → NR5G, false → LTE; bands="all"/"" 恢复全频段）
    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String>;

    /// 设置小区锁定（enable=false 解除锁定; 5G 锁需要 band）
    async fn set_cell_lock(&self, tech: &str, pci: u32, earfcn: u32, band: Option<u32>, enable: bool) -> Result<String, String>;

    /// 设置 USB 网络模式（0: ECM, 1: NCM, 2: MBIM, 3: RNDIS, 5: QMI）
    async fn set_usb_net(&self, mode: u8) -> Result<String, String>;

    /// 切换 SIM 卡槽（含 CFUN 重注册）
    async fn switch_sim_slot(&self, slot: u8) -> Result<String, String>;

    /// 获取诊断信息（子命令: Neighbour/Qlts/MbnList/MbnAutoclose）
    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String>;

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

    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String> {
        let at_mode = match mode {
            "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
            "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
            "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",NR5G:LTE",
            "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
            "auto" => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
            // SA/NSA 切换扩展
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
                        // 裸 IMSI（无 +CIMI: 前缀）
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
            // 解除锁定
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

    async fn set_usb_net(&self, mode: u8) -> Result<String, String> {
        if ![0, 1, 2, 3, 5].contains(&mode) {
            return Err("非法的 usbnet 模式代码，有效值: 0(ECM) 1(NCM) 2(MBIM) 3(RNDIS) 5(QMI)".to_string());
        }
        send_at_command_inner(&self.serial_path, &format!("AT+QCFG=\"usbnet\",{}", mode)).await
    }

    async fn switch_sim_slot(&self, slot: u8) -> Result<String, String> {
        if slot != 1 && slot != 2 {
            return Err("SIM 卡槽只能为 1 或 2".to_string());
        }
        send_at_command_inner(&self.serial_path, &format!("AT+QUIMSLOT={}", slot)).await?;
        // CFUN 循环使 SIM 切换生效
        push_log("INFO", "System", "执行 CFUN=0 软射频关闭...");
        let _ = send_at_command_inner(&self.serial_path, "AT+CFUN=0").await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        push_log("INFO", "System", "执行 CFUN=1 重新激活搜网...");
        send_at_command_inner(&self.serial_path, "AT+CFUN=1").await
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

        push_log("INFO", "System", "[4/5] 获取 APN (AT+CGCONTRDP)...");
        let mut apn = String::new();
        let mut found_apn = false;
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+CGCONTRDP").await {
            if let Some(parsed_apn) = resp.lines().find_map(|l| {
                match at::parser::parse_single_line(l) {
                    Some(at::parser::ParsedLine::Cgcontrdp(c_resp))
                        if !c_resp.apn.is_empty() =>
                    {
                        Some(c_resp.apn)
                    }
                    _ => None,
                }
            }) {
                apn = parsed_apn;
                found_apn = true;
            }
        }
        if !found_apn {
            push_log("INFO", "System", "[4/5] 回退 AT+CGDCONT? 获取 APN...");
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

                // 优先选取非 placeholder 的有效 APN
                if let Some(entry) = cgdcont_entries.iter().find(|e| {
                    !e.apn.contains("placeholder") && !e.apn.starts_with("apn")
                }) {
                    apn = entry.apn.clone();
                    found_apn = true;
                }

                // 次选：无差别选取第一个被成功解析出来的 APN
                if !found_apn {
                    if let Some(entry) = cgdcont_entries.first() {
                        apn = entry.apn.clone();
                    }
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


// ============================================================================
// 通用工具函数
// ============================================================================

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
async fn fetch_network_provider(serial_path: &str) -> String {
    // 1. AT+QSPN (Pest 化)
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

    // 2. AT+COPS? (Pest 化)
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

    // 3. AT+QENG="servingcell" 通过 MCC/MNC 映射 (Pest 化)
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

/// 跨进程文件锁 — 防止其他进程（如后台监控守护进程）同时操作串口
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

/// 锁文件路径（位于 tmpfs，重启自动清理）
const LOCK_FILE_PATH: &str = "/tmp/atcmd_rs.lock";

/// 获取跨进程排他文件锁。
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

/// 带队列去重的 AT 命令发送。
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

/// 底层 AT 命令发送（默认 10 秒超时）
async fn send_at_command_inner(serial_path: &str, cmd: &str) -> Result<String, String> {
    send_at_command_inner_with_timeout(serial_path, cmd, Duration::from_secs(10)).await
}

/// 从设备查询频段信息（NR + LTE），通过多条 AT 命令从模组获取
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

/// 对比新老 Telemetry 差异并生成差分 JSON（仅包含发生变化的字段）
fn generate_telemetry_diff(old: &GlobalTelemetry, new: &GlobalTelemetry) -> serde_json::Value {
    let mut diff_map = serde_json::Map::new();

    macro_rules! compare_field {
        ($field:ident) => {
            if old.$field != new.$field {
                diff_map.insert(stringify!($field).to_string(), serde_json::json!(&new.$field));
            }
        };
    }

    compare_field!(temperature);
    compare_field!(sim_status);
    compare_field!(signal_percentage);
    compare_field!(internet_connection);
    compare_field!(active_sim);
    compare_field!(network_provider);
    compare_field!(mccmnc);
    compare_field!(apn);
    compare_field!(network_mode);
    compare_field!(bands);
    compare_field!(bandwidth);
    compare_field!(earfcn);
    compare_field!(pci);
    compare_field!(ipv4);
    compare_field!(ipv6);
    compare_field!(uptime);
    compare_field!(assessment);
    compare_field!(traffic_stats);
    compare_field!(cell_id);
    compare_field!(enb_id);
    compare_field!(tac);
    compare_field!(ss_rsrq);
    compare_field!(ss_rsrp);
    compare_field!(sinr);
    compare_field!(updated);

    serde_json::Value::Object(diff_map)
}

/// 将 GlobalTelemetry 的 Arc 广播到所有 WS 客户端
fn broadcast_state(state: &AppState, global: &GlobalTelemetry) {
    let global_arc = Arc::new(global.clone());
    let _ = state.tx.send(global_arc);
}

/// 处理单个 channel 消息，返回 false 表示 channel 已关闭
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

/// 处理单个 AT 请求（Actor 严格顺序消费）
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
        AtAction::SetUsbNet(mode) => {
            push_log("INFO", "Actor", &format!("设置USB网络模式: {}", mode));
            let res = backend.set_usb_net(mode).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "usbnet_res",
                "data": {
                    "success": res.is_ok(),
                    "advise": "reboot_required",
                    "msg": res.unwrap_or_else(|e| e)
                }
            }));
        }
        AtAction::SwitchSimSlot(slot) => {
            push_log("INFO", "Actor", &format!("切换SIM卡槽: {}", slot));
            let res = backend.switch_sim_slot(slot).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "sim_slot_res",
                "data": {
                    "success": res.is_ok(),
                    "advise": "reconnecting",
                    "msg": res.unwrap_or_else(|e| e)
                }
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

/// 统一硬件任务：定时轮询 + 用户 AT 指令。严格串行 Actor + 离线检测 + 动态空闲降频
async fn hardware_task(
    mut command_rx: mpsc::Receiver<AtRequest>,
    backend: Arc<dyn HardwareBackend>,
    state: Arc<AppState>,
) {
    let mut components = Components::new_with_refreshed_list();
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
                let active = state.active_views.load(Ordering::Relaxed) > 0;

                if active && get_at_lock().try_lock().is_ok() {
                    let start_poll = std::time::Instant::now();
                    let mut telemetry = backend.poll_telemetry().await;

                    let telemetry_dead = telemetry.sim_status.is_none()
                        && telemetry.network_mode.is_none()
                        && telemetry.signal_percentage.is_none()
                        && telemetry.ipv4.is_none();
                    consecutive_failures = if telemetry_dead { consecutive_failures + 1 } else { 0 };

                    if telemetry.temperature.is_none() {
                        components.refresh(true);
                        if let Some(temp) = components.iter()
                            .find(|c| {
                                let lower = c.label().to_lowercase();
                                lower.contains("cpu") || lower.contains("package")
                            })
                            .and_then(|c| c.temperature())
                        {
                            telemetry.temperature = Some(format!("{:.0} °C", temp));
                        }
                    }

                    telemetry.internet_connection = Some(if telemetry.ipv4.is_some() {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    });

                    let uptime_sec = System::uptime();
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
                            global.uptime = Some(format!("{} minutes", uptime_sec / 60));
                            global.updated = Some(updated_str);
                        } else {
                            *global = GlobalTelemetry::from_telemetry_and_global(
                                &telemetry, &global,
                                Some(format!("{} minutes", uptime_sec / 60)),
                                Some(updated_str),
                            );
                        }

                        broadcast_state(&state, &global);
                    }

                    push_log("INFO", "Poll", &format!("轮询完成 (耗时 {}ms)", start_poll.elapsed().as_millis()));

                } else if active {
                    push_log("WARN", "Poll", "串口忙或正在执行慢命令，跳过当次采集，复用缓存");
                    let uptime_sec = System::uptime();
                    let updated_str = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    let mut global = state.global.write().await;
                    global.uptime = Some(format!("{} minutes", uptime_sec / 60));
                    global.updated = Some(updated_str);
                    broadcast_state(&state, &global);

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
    let serial_path =
        env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());
    push_log("INFO", "System", &format!("正在初始化串口设备: {}", serial_path));

    let (tx, _) = broadcast::channel(100);
    let (command_tx, command_rx) = mpsc::channel(32);

    let app_state = Arc::new(AppState {
        tx: tx.clone(),
        command_tx,
        active_views: AtomicUsize::new(0),
        global: RwLock::new(GlobalTelemetry::default()),
    });

    let backend: Arc<dyn HardwareBackend> = Arc::new(RealBackend { serial_path: serial_path.clone() });

    let (firmware_version, active_sim, network_provider, apn) = backend.read_static_info().await;
    {
        let mut guard = app_state.global.write().await;
        guard.firmware_version = Some(firmware_version);
        guard.active_sim = Some(active_sim);
        guard.network_provider = Some(network_provider);
        guard.apn = Some(apn);
    }

    tokio::spawn({
        let backend = backend.clone();
        let state = app_state.clone();
        async move { hardware_task(command_rx, backend, state).await }
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/style.css", get(style_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    push_log("INFO", "System", "RM520N WebUI 后端服务已在 http://0.0.0.0:3000 监听");
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let mut broadcast_rx = state.tx.subscribe();
    let online_count = state.tx.receiver_count();
    push_log("INFO", "WS", &format!("新的 WebSocket 客户端已连接 (当前在线: {})", online_count));

    state.active_views.fetch_add(1, Ordering::SeqCst);
    let mut current_is_active = true;

    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    tokio::select! {
        _ = async {
            let mut last_sent_telemetry: Option<GlobalTelemetry> = None;

            loop {
                tokio::select! {
                    Ok(new_telemetry_arc) = broadcast_rx.recv() => {
                        let msg_to_send = match last_sent_telemetry {
                            None => {
                                last_sent_telemetry = Some((*new_telemetry_arc).clone());
                                serde_json::json!({
                                    "update_type": "full",
                                    "data": *new_telemetry_arc
                                }).to_string()
                            }
                            Some(ref old) => {
                                let diff = generate_telemetry_diff(old, &new_telemetry_arc);
                                last_sent_telemetry = Some((*new_telemetry_arc).clone());

                                if diff.as_object().map_or(false, |m| !m.is_empty()) {
                                    serde_json::json!({
                                        "update_type": "delta",
                                        "data": diff
                                    }).to_string()
                                } else {
                                    continue;
                                }
                            }
                        };

                        if ws_sender.send(Message::Text(msg_to_send.into())).await.is_err() {
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
            push_log("WARN", "WS", "WebSocket 发送通道中断，准备断开连接");
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
                        "get_backend_log" => {
                            let logs = if let Ok(guard) = get_logs().lock() {
                                let v: Vec<String> = guard.iter().cloned().collect();
                                v
                            } else {
                                vec![]
                            };
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
                        "set_usbnet" => {
                            let mode = cmd.payload.as_ref().and_then(|p| p.as_u64()).unwrap_or(0) as u8;
                            AtAction::SetUsbNet(mode)
                        }
                        "switch_sim_slot" => {
                            let slot = cmd.payload.as_ref().and_then(|p| p.as_u64()).unwrap_or(1) as u8;
                            AtAction::SwitchSimSlot(slot)
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
                        // `set_mode_pref` 是 `set_network_mode` 的别名
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
        } => {
            push_log("INFO", "WS", "浏览器主动断开 WebSocket 连接（接收流正常结束）");
        }
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
}
