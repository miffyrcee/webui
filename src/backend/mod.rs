//! 硬件驱动层：统一 `HardwareBackend` 抽象与真机 / Mock 两套实现。

pub mod mock;
pub mod process;
pub mod real;

use crate::actor::action::DiagnosticType;
use crate::actor::telemetry::TelemetryData;

pub use self::mock::MockBackend;
pub use self::process::*;
pub use self::real::RealBackend;

/// 设备信息（HardwareBackend::read_device_info 返回值）
#[derive(Default)]
pub struct DeviceInfoData {
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
    pub imei: String,
    pub serial: String,
    pub hw_version: String,
    pub module_type: String,
    pub sim_status: String,
    pub imsi: String,
    pub iccid: String,
    pub phone: String,
    pub net_status: String,
    pub signal_quality: String,
    pub temperature: String,
    pub bands: String,
}

#[async_trait::async_trait]
pub trait HardwareBackend: Send + Sync {
    async fn exec_raw_at(&self, cmd: &str) -> String;
    async fn read_sms_list(&self) -> String;
    async fn configure_apn(
        &self,
        apn: &str,
        user: &str,
        pass: &str,
        auth_type: u8,
    ) -> Result<String, String>;
    async fn set_network_mode_pref(&self, mode: &str) -> Result<String, String>;
    async fn set_data_session(&self, connect: bool) -> Result<String, String>;
    async fn scan_available_networks(&self) -> Vec<serde_json::Value>;
    async fn send_sms_msg(&self, recipient: &str, message: &str) -> bool;
    async fn read_device_info(&self) -> DeviceInfoData;
    async fn send_reboot(&self);
    async fn send_factory_reset(&self);
    async fn set_airplane_mode(&self, on: bool);
    async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String>;
    async fn set_cell_lock(
        &self,
        tech: &str,
        pci: u32,
        earfcn: u32,
        band: Option<u32>,
        enable: bool,
    ) -> Result<String, String>;
    async fn get_diagnostics(&self, diag: &DiagnosticType) -> Result<String, String>;
    async fn set_usb_net_mode(&self, mode: u8) -> Result<String, String>;
    async fn get_usb_config(&self) -> Result<serde_json::Value, String>;
    async fn set_sim_slot(&self, slot: u32) -> Result<String, String>;
    async fn get_mbn_list(&self) -> Result<Vec<serde_json::Value>, String>;
    async fn set_mbn(&self, name: &str) -> Result<String, String>;
    async fn poll_telemetry(&self) -> TelemetryData;
    async fn read_static_info(&self) -> (String, String, String, String);
    fn device_name(&self) -> &str;
}
