//! 全局遥测状态与增量合并逻辑。

use serde::Serialize;

/// 广播给前端的全局遥测快照（零锁：整体替换 Arc）。
#[derive(Clone, Serialize, Debug, Default)]
pub struct GlobalTelemetry {
    pub firmware_version: Option<String>,
    pub temperature: Option<String>,
    pub cpu_usage: Option<String>,
    pub memory_usage: Option<String>,
    pub sim_status: Option<String>,
    pub signal_percentage: Option<String>,
    pub internet_connection: Option<String>,
    pub active_sim: Option<String>,
    pub network_provider: Option<String>,
    pub mccmnc: Option<String>,
    pub apn: Option<String>,
    pub network_mode: Option<String>,
    pub bands: Option<String>,
    pub bandwidth: Option<String>,
    pub earfcn: Option<String>,
    pub pci: Option<String>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub uptime: Option<String>,
    pub assessment: Option<String>,
    pub traffic_stats: Option<String>,
    pub cell_id: Option<String>,
    pub enb_id: Option<String>,
    pub tac: Option<String>,
    pub ss_rsrq: Option<String>,
    pub ss_rsrp: Option<String>,
    pub sinr: Option<String>,
    pub updated: Option<String>,
}

/// 单次硬件轮询产出的原始遥测（由解析器填充）。
#[derive(Serialize, Default, Debug, Clone)]
pub struct TelemetryData {
    pub temperature: Option<String>,
    pub sim_status: Option<String>,
    pub signal_percentage: Option<String>,
    pub internet_connection: Option<String>,
    pub active_sim: Option<String>,
    pub network_provider: Option<String>,
    pub mccmnc: Option<String>,
    pub apn: Option<String>,
    pub network_mode: Option<String>,
    pub bands: Option<String>,
    pub bandwidth: Option<String>,
    pub earfcn: Option<String>,
    pub pci: Option<String>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub uptime: Option<String>,
    pub assessment: Option<String>,
    pub traffic_stats: Option<String>,
    pub cell_id: Option<String>,
    pub enb_id: Option<String>,
    pub tac: Option<String>,
    pub ss_rsrq: Option<String>,
    pub ss_rsrp: Option<String>,
    pub sinr: Option<String>,
    pub updated: Option<String>,
}

impl GlobalTelemetry {
    /// 从 TelemetryData（解析器填充）和当前全局状态合并构造 GlobalTelemetry。
    pub fn from_telemetry_and_global(
        t: &TelemetryData,
        g: &GlobalTelemetry,
        uptime: Option<String>,
        updated: Option<String>,
    ) -> Self {
        fn keep_valid(new: &Option<String>, old: &Option<String>) -> Option<String> {
            match new {
                Some(val) if !val.is_empty() => {
                    if val == "NA" || val == "N/A" || val == "--" {
                        Some("--".to_string())
                    } else {
                        Some(val.clone())
                    }
                }
                _ => old.clone(),
            }
        }

        Self {
            temperature: keep_valid(&t.temperature, &g.temperature),
            cpu_usage: g.cpu_usage.clone(),
            memory_usage: g.memory_usage.clone(),
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

/// 将 GlobalTelemetry 序列化为 JSON 字符串（预序列化广播辅助函数）
pub fn serialize_global(global: &GlobalTelemetry, update_type: &str) -> String {
    serde_json::json!({
        "update_type": update_type,
        "data": global
    })
    .to_string()
}
