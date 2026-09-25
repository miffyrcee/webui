//! Actor 层的动作定义：Web 层产出的 `AtAction` 经通道投递给硬件任务串行执行。

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticType {
    Neighbour,
    Qlts,
    MbnList,
    AutoSelQuery,
}

impl std::fmt::Display for DiagnosticType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagnosticType::Neighbour => write!(f, "neighbour"),
            DiagnosticType::Qlts => write!(f, "qlts"),
            DiagnosticType::MbnList => write!(f, "mbn_list"),
            DiagnosticType::AutoSelQuery => write!(f, "autosel_query"),
        }
    }
}

#[derive(Debug)]
pub enum AtAction {
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
    SetCellLock {
        tech: String,
        pci: u32,
        earfcn: u32,
        band: Option<u32>,
        enable: bool,
    },
    /// 诊断子命令
    GetDiagnostics(DiagnosticType),
    /// (mode_num) — 0:RMNET, 1:ECM, 2:MBIM, 3:RNDIS, 4:NCM(SDX55), 5:NCM(SDX62)
    SetUsbNetMode(u8),
    /// 获取当前 USB 模式
    GetUsbConfig,
    /// (slot) — 切换 SIM 卡槽，仅支持 1/2
    SetSimSlot(u32),
    /// 查询 MBN 列表（AT+QMBNCFG="List"）
    GetMbnList,
    /// (name) — 选择 MBN 配置（AT+QMBNCFG="Select",...）
    SetMbn(String),
    /// true=启用 AutoSel, false=禁用（AT+QMBNCFG="AutoSel",x）
    SetMbnAutoSel(bool),
    /// 停用当前 MBN（AT+QMBNCFG="Deactivate"）
    DeactivateMbn,
    /// 读取 IMEI（AT+EGMR=0,7）
    ReadImei,
    /// (imei) — 写入 IMEI（AT+EGMR=1,7,"<imei>"）
    WriteImei(String),
    /// (driver, pcie_rc) — M.2 网口配置：加载网卡驱动 + 配置 PCIe/数据通道，原子执行
    SetEthConfig { driver: String, pcie_rc: bool },
    /// (mode) — IP Passthrough 配置：dmz 准直通 / nat 标准路由，原子执行
    SetIpptConfig { mode: String },
}

pub struct AtRequest {
    pub action: AtAction,
    pub resp_tx: oneshot::Sender<serde_json::Value>,
}
