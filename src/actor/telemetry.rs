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
        // 设备/链路固有状态：本轮没读到（None）时沿用上一次的已知值，
        // 避免单条 AT 指令偶发超时导致卡槽状态、连接状态在界面上闪烁。
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

        // 注意：空口动态指标（IP、信号、小区、频段…）必须直接取本轮解析结果，
        // 采不到就置空由前端显示 "--"。若在这里回退旧值，模组欠费/脱网/拔天线后
        // 界面会永远停留在掉线前的值上，出现「已断网却仍显示 IP 与小区」的假数据。
        Self {
            temperature: t.temperature.clone(),
            signal_percentage: t.signal_percentage.clone(),
            mccmnc: t.mccmnc.clone(),
            network_mode: t.network_mode.clone(),
            bands: t.bands.clone(),
            bandwidth: t.bandwidth.clone(),
            earfcn: t.earfcn.clone(),
            pci: t.pci.clone(),
            ipv4: t.ipv4.clone(),
            ipv6: t.ipv6.clone(),
            assessment: t.assessment.clone(),
            traffic_stats: t.traffic_stats.clone(),
            cell_id: t.cell_id.clone(),
            enb_id: t.enb_id.clone(),
            tac: t.tac.clone(),
            ss_rsrq: t.ss_rsrq.clone(),
            ss_rsrp: t.ss_rsrp.clone(),
            sinr: t.sinr.clone(),
            sim_status: keep_valid(&t.sim_status, &g.sim_status),
            internet_connection: keep_valid(&t.internet_connection, &g.internet_connection),
            // 以下字段不由轮询产出，直接沿用全局值
            firmware_version: g.firmware_version.clone(),
            active_sim: g.active_sim.clone(),
            network_provider: g.network_provider.clone(),
            apn: g.apn.clone(),
            cpu_usage: g.cpu_usage.clone(),
            memory_usage: g.memory_usage.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_fields_are_cleared_when_not_collected() {
        // 模组脱网：本轮一条空口数据都没采到（全 None），
        // 上一轮的值绝不能被继承，否则界面会残留断网前的 IP/小区/信号。
        let g = GlobalTelemetry {
            ipv4: Some("10.0.0.1".to_string()),
            ipv6: Some("2409::1".to_string()),
            pci: Some("751".to_string()),
            bands: Some("NR5G BAND 41".to_string()),
            signal_percentage: Some("78%".to_string()),
            cell_id: Some("39074C001".to_string()),
            enb_id: Some("39074C".to_string()),
            ss_rsrp: Some("-65 / 78%".to_string()),
            sim_status: Some("READY".to_string()),
            ..Default::default()
        };

        let merged =
            GlobalTelemetry::from_telemetry_and_global(&TelemetryData::default(), &g, None, None);

        assert_eq!(merged.ipv4, None);
        assert_eq!(merged.ipv6, None);
        assert_eq!(merged.pci, None);
        assert_eq!(merged.bands, None);
        assert_eq!(merged.signal_percentage, None);
        assert_eq!(merged.cell_id, None);
        assert_eq!(merged.enb_id, None);
        assert_eq!(merged.ss_rsrp, None);
    }

    #[test]
    fn device_state_keeps_last_known_value() {
        // SIM 卡槽等设备固有状态仍应保留上次已知值，避免偶发超时造成闪烁
        let g = GlobalTelemetry {
            sim_status: Some("READY".to_string()),
            internet_connection: Some("Connected".to_string()),
            ..Default::default()
        };

        let merged =
            GlobalTelemetry::from_telemetry_and_global(&TelemetryData::default(), &g, None, None);

        assert_eq!(merged.sim_status, Some("READY".to_string()));
        assert_eq!(merged.internet_connection, Some("Connected".to_string()));
    }

    #[test]
    fn fresh_values_replace_stale_ones() {
        let g = GlobalTelemetry {
            ipv4: Some("10.0.0.1".to_string()),
            pci: Some("751".to_string()),
            ..Default::default()
        };
        let t = TelemetryData {
            ipv4: Some("10.0.0.2".to_string()),
            pci: Some("250".to_string()),
            ..Default::default()
        };

        let merged = GlobalTelemetry::from_telemetry_and_global(&t, &g, None, None);

        assert_eq!(merged.ipv4, Some("10.0.0.2".to_string()));
        assert_eq!(merged.pci, Some("250".to_string()));
    }

    #[test]
    fn offline_reset_serializes_latest_fields_as_null() {
        // 前端依赖 JSON null 判定「本轮未采到」，这里锁定该契约
        let g = GlobalTelemetry {
            ipv4: Some("10.0.0.1".to_string()),
            ..Default::default()
        };
        let merged =
            GlobalTelemetry::from_telemetry_and_global(&TelemetryData::default(), &g, None, None);
        let json: serde_json::Value = serde_json::from_str(&serialize_global(&merged, "delta"))
            .expect("序列化结果必须是合法 JSON");

        assert!(json["data"]["ipv4"].is_null());
    }
}
