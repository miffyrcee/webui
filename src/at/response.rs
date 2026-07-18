/// Response types for parsed AT command results
use serde::Serialize;

/// Parsed +CPIN response
#[derive(Debug, Clone, Serialize)]
pub struct CpinResponse {
    /// e.g. "READY", "SIM PIN", "SIM PUK", etc.
    pub status: String,
}

/// Parsed +QUIMSLOT response
#[derive(Debug, Clone, Serialize)]
pub struct QuimslotResponse {
    pub slot: u32,
}

/// Parsed +QSPN response fields
#[derive(Debug, Clone, Default, Serialize)]
pub struct QspnResponse {
    pub fnn: String,   // Full Network Name
    pub snn: String,   // Short Network Name
    pub spn: String,   // Service Provider Name
    pub alphabet: String,
}

/// Parsed +COPS response fields
#[derive(Debug, Clone, Serialize)]
pub struct CopsResponse {
    pub mode: String,
    pub format: Option<String>,
    pub oper: Option<String>,
    pub act: Option<String>,
}

/// Parsed +CGDCONT entry
#[derive(Debug, Clone, Serialize)]
pub struct CgdcontEntry {
    pub cid: u32,
    pub pdp_type: String,
    pub apn: String,
    pub pdp_addr: String,
    pub d_comp: String,
    pub h_comp: String,
}

/// Parsed traffic stats (used by both +QGDNRCNT and +QGDAT)
#[derive(Debug, Clone, Serialize)]
pub struct TrafficStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

/// Parsed +CGPADDR entry
#[derive(Debug, Clone, Serialize)]
pub struct CgpaddrEntry {
    pub cid: u32,
    pub ipv4: String,
    pub ipv6: String,
}

/// A parsed +QENG: "servingcell" line (single component carrier)
#[derive(Debug, Clone, Default, Serialize)]
pub struct QengServingCell {
    pub connection_status: String,  // e.g. "CONNECT", "NOCONN"
    pub rat: String,                // e.g. "NR5G-SA", "NR5G-NSA", "LTE"
    pub opmode: String,             // e.g. "TDD", "FDD"
    pub mcc: String,
    pub mnc: String,
    pub cell_id: String,
    pub pci: String,
    pub tac: String,
    pub earfcn: String,
    pub band: String,
    pub bandwidth: String,
    pub rsrp: String,
    pub rsrq: String,
    pub sinr: String,
    pub srxlev: String,
    pub rssi: String,
}

/// Parsed +QCAINFO entry (one PCC or SCC line)
#[derive(Debug, Clone, Default, Serialize)]
pub struct QcainfoEntry {
    pub component: String,  // "PCC" or "SCC"
    pub earfcn: String,
    pub bandwidth: String,
    pub band: String,
    pub pci: String,
    pub scc_idx: Option<String>,
    pub rsrp: Option<String>,
    pub rsrq: Option<String>,
    pub sinr: Option<String>,
}

/// Parsed +CGCONTRDP response (APN info from active context)
#[derive(Debug, Clone, Default, Serialize)]
pub struct CgcontrdpResponse {
    pub apn: String,
}

/// Parsed +QTEMP single sensor line
#[derive(Debug, Clone, Serialize)]
pub struct QtempResponse {
    pub sensor: String,
    pub temperature: Option<f64>,
}

/// Parsed +CNUM response
#[derive(Debug, Clone, Default, Serialize)]
pub struct CnumResponse {
    pub number: String,
}

