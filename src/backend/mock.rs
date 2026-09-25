//! Mock 硬件后端（无模组电脑环境本地开发）。

use crate::actor::action::DiagnosticType;
use crate::actor::telemetry::TelemetryData;
use crate::backend::{DeviceInfoData, HardwareBackend};
use crate::logger::push_log;

pub struct MockBackend;

#[async_trait::async_trait]
impl HardwareBackend for MockBackend {
    async fn exec_raw_at(&self, cmd: &str) -> String {
        format!("+MOCK_AT: {} -> OK\r\n", cmd)
    }

    async fn read_sms_list(&self) -> String {
        "+CMGL: 0 messages\r\nOK\r\n".to_string()
    }

    async fn configure_apn(
        &self,
        apn: &str,
        _user: &str,
        _pass: &str,
        _auth_type: u8,
    ) -> Result<String, String> {
        push_log("MOCK", "APN", "Mock: 配置 APN");
        Ok(format!("APN {} 设置已应用 (Mock)", apn))
    }

    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String> {
        push_log("MOCK", "Net", &format!("Mock: 设置网络模式 {}", mode));
        Ok("OK\r\n".to_string())
    }

    async fn set_data_session(&self, connect: bool) -> Result<String, String> {
        push_log(
            "MOCK",
            "Net",
            &format!("Mock: 数据会话 {}", if connect { "连接" } else { "断开" }),
        );
        Ok(if connect {
            "拨号连接指令已下发 (Mock)"
        } else {
            "拨号断开指令已下发 (Mock)"
        }
        .to_string())
    }

    async fn scan_available_networks(&self) -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({ "operator": "中国移动 (MOCK)", "mccmnc": "46000", "technology": "NR5G", "status": "Available", "band": "n78" }),
            serde_json::json!({ "operator": "中国联通 (MOCK)", "mccmnc": "46001", "technology": "LTE", "status": "Available", "band": "B1" }),
            serde_json::json!({ "operator": "中国电信 (MOCK)", "mccmnc": "46003", "technology": "NR5G", "status": "Available", "band": "n78" }),
        ]
    }

    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool {
        push_log(
            "MOCK",
            "SMS",
            &format!("Mock: 发送短信至 {}: {}", recipient, message),
        );
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
        push_log(
            "MOCK",
            "System",
            &format!("Mock: 飞行模式 {}", if on { "开启" } else { "关闭" }),
        );
    }

    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String> {
        push_log(
            "MOCK",
            "Band",
            &format!("Mock: 频段锁定 is_nr5g={}, bands={}", is_nr5g, bands),
        );
        Ok("OK\r\n".to_string())
    }

    async fn set_cell_lock(
        &self,
        tech: &str,
        pci: u32,
        earfcn: u32,
        band: Option<u32>,
        enable: bool,
    ) -> Result<String, String> {
        push_log(
            "MOCK",
            "Cell",
            &format!(
                "Mock: 小区锁定 tech={}, pci={}, earfcn={}, band={:?}, enable={}",
                tech, pci, earfcn, band, enable
            ),
        );
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

    async fn set_usb_net_mode(&self, mode: u8) -> Result<String, String> {
        push_log(
            "MOCK",
            "USB",
            &format!("Mock: 设置 USB 网络模式为 {}", mode),
        );
        Ok("OK\r\n".to_string())
    }

    async fn get_usb_config(&self) -> Result<serde_json::Value, String> {
        push_log("MOCK", "USB", "Mock: 获取 USB 配置");
        Ok(serde_json::json!({
            "usbnet_mode": 1,
            "usbnet_name": "ECM (Linux/Mac免驱) [MOCK]",
            "usbnet_supported": true
        }))
    }

    async fn set_sim_slot(&self, slot: u32) -> Result<String, String> {
        push_log("MOCK", "SIM", &format!("Mock: 切换 SIM 卡槽为 {}", slot));
        Ok("OK\r\n".to_string())
    }

    async fn get_mbn_list(&self) -> Result<Vec<serde_json::Value>, String> {
        push_log("MOCK", "MBN", "Mock: 查询 MBN 列表");
        Ok(vec![
            serde_json::json!({ "index": 0, "state": 1, "name": "RM520NGLAAR01A02M4G_01.004 (MOCK)" }),
            serde_json::json!({ "index": 1, "state": 0, "name": "RM520NGLAAR01A02M4G_01.006 (MOCK)" }),
        ])
    }

    async fn set_mbn(&self, name: &str) -> Result<String, String> {
        push_log("MOCK", "MBN", &format!("Mock: 选择 MBN {}", name));
        Ok("OK\r\n".to_string())
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

    fn device_name(&self) -> &str {
        "Mock Backend (Testing)"
    }
}
