//! 移远模组真机驱动（经 `atcmd_rs` 子进程独占串口）。

use std::time::Duration;

use crate::actor::action::DiagnosticType;
use crate::actor::telemetry::TelemetryData;
use crate::at::{
    builder::{encode_ucs2_hex, needs_ucs2, sanitize_at_param},
    format_bytes,
    parser::{
        ParsedLine, is_valid_data_apn, parse_cgpaddr, parse_cops_scan, parse_net_status,
        parse_qcainfo, parse_qeng, parse_qtemp_temperature, parse_signal_quality, parse_single_line,
    },
};
use crate::backend::process::{
    fetch_network_provider, query_device_bands, send_at_command_inner,
    send_at_command_inner_with_timeout, send_at_get_line, send_sms_command_inner,
};
use crate::backend::{DeviceInfoData, HardwareBackend};
use crate::device::{self, DeviceProfile};
use crate::logger::push_log;

pub struct RealBackend {
    pub serial_path: String,
    pub profile: &'static DeviceProfile,
}

impl RealBackend {
    pub async fn new(serial_path: String) -> Self {
        let mut backend = Self {
            serial_path,
            profile: &device::PROFILE_GENERIC,
        };
        // 必须使用 std::path 再次检查，因为 main() 中构造前可能路径已不存在
        if std::path::Path::new(&backend.serial_path).exists() {
            if let Some(cgmm) = send_at_get_line(&backend.serial_path, "AT+CGMM").await {
                let detected = device::lookup_profile(&cgmm);
                push_log(
                    "INFO",
                    "Device",
                    &format!("检测到模组: {} → Profile: {}", cgmm.trim(), detected.name,),
                );
                backend.profile = detected;
            } else {
                push_log("WARN", "Device", "AT+CGMM 无响应，使用通用 Quectel Profile");
            }
        } else {
            push_log("WARN", "Device", "串口设备不存在，使用通用 Quectel Profile");
        }
        backend
    }
}

#[async_trait::async_trait]
impl HardwareBackend for RealBackend {
    async fn exec_raw_at(&self, cmd: &str) -> String {
        match send_at_command_inner(&self.serial_path, cmd).await {
            Ok(resp) => resp,
            Err(e) => format!("ERROR: {}", e),
        }
    }

    async fn read_sms_list(&self) -> String {
        let _ = send_at_command_inner(&self.serial_path, "AT+CMGF=1").await;
        let _ = send_at_command_inner(&self.serial_path, "AT+CSCS=\"GSM\"").await;
        send_at_command_inner(&self.serial_path, "AT+CMGL=\"ALL\"")
            .await
            .unwrap_or_else(|_| "+CMGL: 0 messages\r\nOK\r\n".to_string())
    }

    async fn configure_apn(
        &self,
        apn: &str,
        user: &str,
        pass: &str,
        auth_type: u8,
    ) -> Result<String, String> {
        let apn = sanitize_at_param(apn);
        let user = sanitize_at_param(user);
        let pass = sanitize_at_param(pass);
        send_at_command_inner(
            &self.serial_path,
            &format!("AT+CGDCONT=1,\"IPV4V6\",\"{}\"", apn),
        )
        .await?;
        if !user.is_empty() {
            send_at_command_inner(
                &self.serial_path,
                &format!("AT+CGAUTH=1,{},\"{}\",\"{}\"", auth_type, user, pass),
            )
            .await?;
        }
        Ok(format!("APN {} 设置已应用", apn))
    }

    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String> {
        let at_mode = match mode {
            "nr5g" => "AT+QNWPREFCFG=\"mode_pref\",NR5G",
            "lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE",
            "nr5g_lte" => "AT+QNWPREFCFG=\"mode_pref\",LTE:NR5G",
            "wcdma" => "AT+QNWPREFCFG=\"mode_pref\",WCDMA",
            "auto" => "AT+QNWPREFCFG=\"mode_pref\",AUTO",
            "disable_sa" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",1",
            "disable_nsa" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",2",
            "enable_all_5g" => "AT+QNWPREFCFG=\"nr5g_disable_mode\",0",
            _ => return Err(format!("不支持的网络模式参数: {}", mode)),
        };
        send_at_command_inner(&self.serial_path, at_mode).await
    }

    async fn set_data_session(&self, connect: bool) -> Result<String, String> {
        let action_val = if connect { "1" } else { "0" };
        send_at_command_inner(&self.serial_path, &format!("AT+CGACT={},1", action_val)).await?;
        Ok(if connect {
            "拨号连接指令已下发"
        } else {
            "拨号断开指令已下发"
        }
        .to_string())
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
                push_log(
                    "INFO",
                    "Scan",
                    &format!("网络扫描完成，发现 {} 个网络", networks.len()),
                );
                networks
            }
            Err(e) => {
                push_log("ERROR", "Scan", &format!("网络扫描失败: {}", e));
                vec![]
            }
        }
    }

    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool {
        let recipient = sanitize_at_param(recipient);
        let message = sanitize_at_param(message);

        if !needs_ucs2(&message) {
            // GSM 7-bit 默认编码（原有逻辑）
            if send_at_command_inner(&self.serial_path, "AT+CMGF=1")
                .await
                .is_ok()
            {
                send_sms_command_inner(
                    &self.serial_path,
                    &format!("AT+CMGS=\"{}\"", recipient),
                    &message,
                    None,
                )
                .await
                .is_ok()
            } else {
                false
            }
        } else {
            // UCS2 短信使用文本模式 + DCS=8 发送
            // Quectel 模块在 CSCS="UCS2" 文本模式下不支持 AT+CMGS，
            // 故保持 CSCS="GSM"（号码保持 ASCII），通过 CSMP DCS=8
            // 指定消息体为 UCS2 编码，将 UCS2 hex 文本作为正文发送。
            if send_at_command_inner(&self.serial_path, "AT+CMGF=1")
                .await
                .is_err()
            {
                return false;
            }
            if send_at_command_inner(&self.serial_path, "AT+CSCS=\"GSM\"")
                .await
                .is_err()
            {
                return false;
            }
            if send_at_command_inner(&self.serial_path, "AT+CSMP=17,167,0,8")
                .await
                .is_err()
            {
                return false;
            }

            let ucs2_hex_msg = encode_ucs2_hex(&message);
            let result = send_sms_command_inner(
                &self.serial_path,
                &format!("AT+CMGS=\"{}\"", recipient),
                &ucs2_hex_msg,
                None,
            )
            .await
            .is_ok();

            // 复位 CSMP（DCS=0 默认编码）
            let _ = send_at_command_inner(&self.serial_path, "AT+CSMP=17,167,0,0").await;

            result
        }
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
            .and_then(|r| match parse_single_line(&r) {
                Some(ParsedLine::Cpin(cpin)) => Some(cpin.status),
                _ => None,
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let imsi = send_at_get_line(&self.serial_path, "AT+CIMI")
            .await
            .and_then(|r| match parse_single_line(&r) {
                Some(ParsedLine::Cimi(imsi)) => Some(imsi),
                Some(ParsedLine::Other(val)) => Some(val.trim().to_string()),
                _ => None,
            })
            .unwrap_or_default();
        let iccid = send_at_get_line(&self.serial_path, "AT+QCCID")
            .await
            .and_then(|r| match parse_single_line(&r) {
                Some(ParsedLine::Qccid(iccid)) => Some(iccid),
                _ => None,
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let phone = send_at_get_line(&self.serial_path, "AT+CNUM")
            .await
            .and_then(|r| match parse_single_line(&r) {
                Some(ParsedLine::Cnum(cnum)) => Some(cnum.number),
                _ => None,
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
                self.profile.default_nr_bands
            } else {
                bands
            };
            format!("AT+QNWPREFCFG=\"nr5g_band\",{}", b)
        } else {
            let b = if bands.is_empty() || bands == "all" {
                self.profile.default_lte_bands
            } else {
                bands
            };
            format!("AT+QNWPREFCFG=\"lte_band\",{}", b)
        };
        send_at_command_inner(&self.serial_path, &cmd).await
    }

    async fn set_cell_lock(
        &self,
        tech: &str,
        pci: u32,
        earfcn: u32,
        band: Option<u32>,
        enable: bool,
    ) -> Result<String, String> {
        if !enable {
            if tech.eq_ignore_ascii_case("5g") {
                send_at_command_inner(&self.serial_path, "AT+QNWLOCK=\"common/5g\",0").await
            } else {
                send_at_command_inner(&self.serial_path, "AT+QNWLOCK=\"common/lte\",0").await
            }
        } else if tech.eq_ignore_ascii_case("5g") {
            let b = band.ok_or_else(|| "5G 锁小区必须提供 Band".to_string())?;
            // 低频段 FDD (<=28) 常用 15kHz，高频段 TDD 常用 30kHz
            let scs = if b <= self.profile.nr_cell_lock_scs_threshold {
                15
            } else {
                30
            };
            send_at_command_inner(
                &self.serial_path,
                &format!("AT+QNWLOCK=\"common/5g\",{},{},{},{}", pci, earfcn, scs, b),
            )
            .await
        } else {
            send_at_command_inner(
                &self.serial_path,
                &format!("AT+QNWLOCK=\"common/lte\",1,{},{}", earfcn, pci),
            )
            .await
        }
    }

    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String> {
        match diag {
            DiagnosticType::Neighbour => {
                send_at_command_inner(&self.serial_path, "AT+QENG=\"neighbourcell\"").await
            }
            DiagnosticType::Qlts => match send_at_command_inner(&self.serial_path, "AT+QLTS=2").await
            {
                Ok(res) => Ok(res),
                Err(_) => send_at_command_inner(&self.serial_path, "AT+QLTS").await,
            },
            DiagnosticType::MbnList => {
                send_at_command_inner(&self.serial_path, "AT+QMBNCFG=\"List\"").await
            }
            DiagnosticType::AutoSelQuery => {
                send_at_command_inner(&self.serial_path, "AT+QMBNCFG=\"AutoSel\"").await
            }
        }
    }

    async fn poll_telemetry(&self) -> TelemetryData {
        let mut telemetry = TelemetryData::default();

        if let Ok(cgpaddr_resp) = send_at_command_inner(&self.serial_path, "AT+CGPADDR").await {
            parse_cgpaddr(&cgpaddr_resp, &mut telemetry);
        }
        if let Ok(qeng_resp) =
            send_at_command_inner(&self.serial_path, "AT+QENG=\"servingcell\"").await
        {
            parse_qeng(&qeng_resp, &mut telemetry);
        }
        if let Ok(qcainfo_resp) = send_at_command_inner(&self.serial_path, "AT+QCAINFO").await {
            parse_qcainfo(&qcainfo_resp, &mut telemetry);
        }
        if let Ok(cpin_resp) = send_at_command_inner(&self.serial_path, "AT+CPIN?").await {
            if let Some(status) = cpin_resp.lines().find_map(|l| match parse_single_line(l) {
                Some(ParsedLine::Cpin(cpin)) if !cpin.status.is_empty() => Some(cpin.status),
                _ => None,
            }) {
                telemetry.sim_status = Some(status);
            }
        }
        if let Ok(qtemp_resp) = send_at_command_inner(&self.serial_path, "AT+QTEMP").await {
            if let Some(temp) = parse_qtemp_temperature(&qtemp_resp) {
                telemetry.temperature = Some(temp);
            }
        }

        telemetry.traffic_stats = send_at_command_inner(&self.serial_path, "AT+QGDNRCNT?")
            .await
            .ok()
            .and_then(|r| {
                r.lines().find_map(|l| match parse_single_line(l) {
                    Some(ParsedLine::TrafficStats(stats)) => Some(format!(
                        "TX {} / RX {}",
                        format_bytes(stats.tx_bytes),
                        format_bytes(stats.rx_bytes)
                    )),
                    _ => None,
                })
            });
        if telemetry.traffic_stats.is_none() {
            telemetry.traffic_stats = send_at_command_inner(&self.serial_path, "AT+QGDAT?")
                .await
                .ok()
                .and_then(|r| {
                    r.lines().find_map(|l| match parse_single_line(l) {
                        Some(ParsedLine::TrafficStats(stats)) => Some(format!(
                            "TX {} / RX {}",
                            format_bytes(stats.tx_bytes),
                            format_bytes(stats.rx_bytes)
                        )),
                        _ => None,
                    })
                });
        }

        telemetry
    }

    async fn read_static_info(&self) -> (String, String, String, String) {
        push_log("INFO", "System", "开始获取静态信息...");

        push_log("INFO", "System", "[1/4] 获取固件版本 (AT+CGMR)...");
        let firmware_version = send_at_get_line(&self.serial_path, "AT+CGMR")
            .await
            .unwrap_or_else(|| "NA".to_string());

        push_log("INFO", "System", "[2/5] 获取 SIM 槽位 (AT+QUIMSLOT?)...");
        let mut active_sim = String::new();
        if let Ok(resp) = send_at_command_inner(&self.serial_path, "AT+QUIMSLOT?").await {
            if let Some(slot_name) = resp.lines().find_map(|l| match parse_single_line(l) {
                Some(ParsedLine::Quimslot(slot_resp)) => Some(format!("SIM {}", slot_resp.slot)),
                _ => None,
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
                .filter_map(|l| match parse_single_line(l) {
                    Some(ParsedLine::Cgdcont(entry)) if !entry.apn.is_empty() => Some(entry),
                    _ => None,
                })
                .collect();

            if let Some(entry) = cgdcont_entries.iter().find(|e| is_valid_data_apn(&e.apn)) {
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

        push_log(
            "INFO",
            "System",
            &format!(
                "静态信息已获取: FW={}, SIM={}, Provider={}, APN={}",
                firmware_version, active_sim, network_provider, apn
            ),
        );

        (firmware_version, active_sim, network_provider, apn)
    }

    fn device_name(&self) -> &str {
        self.profile.name
    }

    async fn set_usb_net_mode(&self, mode: u8) -> Result<String, String> {
        if !matches!(mode, 0 | 1 | 2 | 3 | 4 | 5) {
            return Err("不支持的 USB 网络模式，有效值为 0(RMNET), 1(ECM), 2(MBIM), 3(RNDIS), 4(NCM/SDX55), 5(NCM/SDX62)".to_string());
        }
        let cmd = format!("AT+QCFG=\"usbnet\",{}", mode);
        send_at_command_inner(&self.serial_path, &cmd).await
    }

    async fn get_usb_config(&self) -> Result<serde_json::Value, String> {
        let usbnet_resp = send_at_command_inner(&self.serial_path, "AT+QCFG=\"usbnet\"").await;

        // 解析 usbnet（指令失败时 usbnet_supported = false）
        let (usbnet_mode, usbnet_supported) = match &usbnet_resp {
            Ok(resp) => {
                let mode = resp
                    .lines()
                    .find_map(|l| {
                        if l.contains("+QCFG: \"usbnet\",") {
                            l.split(',').nth(1)?.trim().parse::<u8>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                (mode, true)
            }
            Err(_) => (0, false),
        };

        let mode_name = if usbnet_supported {
            match usbnet_mode {
                0 => "RMNET (QMI)",
                1 => "ECM (Linux/Mac免驱)",
                2 => "MBIM (Win10/11原生)",
                3 => "RNDIS (Windows免驱)",
                4 => "NCM (SDX55)",
                5 => "NCM (SDX62 高速网卡)",
                _ => "Unknown",
            }
        } else {
            "N/A (指令不支持)"
        };

        Ok(serde_json::json!({
            "usbnet_mode": usbnet_mode,
            "usbnet_name": mode_name,
            "usbnet_supported": usbnet_supported
        }))
    }

    async fn set_sim_slot(&self, slot: u32) -> Result<String, String> {
        if !matches!(slot, 1 | 2) {
            return Err(format!("无效的 SIM 卡槽，仅支持 1/2: {}", slot));
        }
        send_at_command_inner(&self.serial_path, &format!("AT+QUIMSLOT={}", slot)).await
    }

    async fn get_mbn_list(&self) -> Result<Vec<serde_json::Value>, String> {
        let resp = send_at_command_inner(&self.serial_path, "AT+QMBNCFG=\"List\"").await?;
        let entries: Vec<serde_json::Value> = resp
            .lines()
            .filter_map(|line| match parse_single_line(line.trim()) {
                Some(ParsedLine::Qmbncfg(e)) => {
                    Some(serde_json::json!({ "index": e.index, "state": e.state, "name": e.name }))
                }
                _ => None,
            })
            .collect();
        if entries.is_empty() {
            Err(format!("未解析到 MBN 列表: {}", resp.trim()))
        } else {
            Ok(entries)
        }
    }

    async fn set_mbn(&self, name: &str) -> Result<String, String> {
        let name = sanitize_at_param(name);
        if name.is_empty() {
            return Err("MBN 名称不能为空".to_string());
        }
        send_at_command_inner(
            &self.serial_path,
            &format!("AT+QMBNCFG=\"Select\",\"{}\"", name),
        )
        .await
    }
}
