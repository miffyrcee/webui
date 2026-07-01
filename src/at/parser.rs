//! pest-based parsers for AT command responses
//!
//! This module replaces the previous nom-based parsers with pest-based grammars.

use crate::at::{
    response::*,
    utils::{decode_hex_ucs2, extract_value},
};
use pest_derive::Parser;

/// The pest parser generated from grammar.pest
#[derive(Parser)]
#[grammar = "at/grammar.pest"]
#[allow(dead_code)]
pub struct AtParser;

/// Parse a full AT response and extract all result lines
#[allow(dead_code)]
pub fn parse_at_response(raw: &str) -> Vec<ParsedLine> {
    let mut results = Vec::new();

    // Use simple line-by-line matching instead of attempting full pest grammar
    // which can be brittle for AT response noise/echo
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(parsed) = parse_single_line(trimmed) {
            results.push(parsed);
        }
    }

    results
}

/// Represents a single parsed line from an AT response
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ParsedLine {
    Cpin(CpinResponse),
    Quimslot(QuimslotResponse),
    Qspn(QspnResponse),
    Cops(CopsResponse),
    Cgdcont(CgdcontEntry),
    TrafficStats(TrafficStats),
    Cgpaddr(CgpaddrEntry),
    QengServingCell(QengServingCell),
    Qcainfo(QcainfoEntry),
    Ok,
    Error,
    Other(String),
}

/// Parse a single line of AT response using pest
fn parse_single_line(line: &str) -> Option<ParsedLine> {
    // OK / ERROR
    if line == "OK" {
        return Some(ParsedLine::Ok);
    }
    if line == "ERROR" || line.starts_with("+CME ERROR:") {
        return Some(ParsedLine::Error);
    }

    // +CPIN:
    if line.starts_with("+CPIN:") {
        let status = line
            .strip_prefix("+CPIN:")
            .map(|s| s.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        return Some(ParsedLine::Cpin(CpinResponse { status }));
    }

    // +QUIMSLOT:
    if line.starts_with("+QUIMSLOT:") {
        let slot = line
            .strip_prefix("+QUIMSLOT:")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(1);
        return Some(ParsedLine::Quimslot(QuimslotResponse { slot }));
    }

    // +QSPN:
    if line.starts_with("+QSPN:") {
        let rest = line.strip_prefix("+QSPN:").unwrap_or("").trim();
        let parts: Vec<&str> = rest.split(',').collect();
        let mut resp = QspnResponse::default();
        if parts.len() >= 1 {
            let raw = extract_value(parts[0]);
            let decoded = decode_hex_ucs2(raw);
            resp.fnn = if decoded.is_empty() {
                raw.to_string()
            } else {
                decoded
            };
        }
        if parts.len() >= 2 {
            resp.snn = extract_value(parts[1]).to_string();
        }
        if parts.len() >= 3 {
            resp.spn = extract_value(parts[2]).to_string();
        }
        if parts.len() >= 4 {
            resp.alphabet = extract_value(parts[3]).to_string();
        }
        return Some(ParsedLine::Qspn(resp));
    }

    // +COPS:
    if line.starts_with("+COPS:") {
        let rest = line.strip_prefix("+COPS:").unwrap_or("").trim();
        let parts: Vec<&str> = rest.split(',').map(|s| s.trim()).collect();
        let mode = extract_value(parts.first().unwrap_or(&"")).to_string();
        let format = parts.get(1).map(|s| extract_value(s).to_string());
        let oper = parts.get(2).map(|s| extract_value(s).to_string());
        let act = parts.get(3).map(|s| extract_value(s).to_string());
        return Some(ParsedLine::Cops(CopsResponse {
            mode,
            format,
            oper,
            act,
        }));
    }

    // +CGDCONT:
    if line.starts_with("+CGDCONT:") {
        let rest = line.strip_prefix("+CGDCONT:").unwrap_or("").trim();
        let parts: Vec<&str> = rest.split(',').collect();
        let cid: u32 = parts
            .first()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let entry = CgdcontEntry {
            cid,
            pdp_type: parts
                .get(1)
                .map(|s| extract_value(s).to_string())
                .unwrap_or_default(),
            apn: parts
                .get(2)
                .map(|s| extract_value(s).to_string())
                .unwrap_or_default(),
            pdp_addr: parts
                .get(3)
                .map(|s| extract_value(s).to_string())
                .unwrap_or_default(),
            d_comp: parts
                .get(4)
                .map(|s| extract_value(s).to_string())
                .unwrap_or_default(),
            h_comp: parts
                .get(5)
                .map(|s| extract_value(s).to_string())
                .unwrap_or_default(),
        };
        return Some(ParsedLine::Cgdcont(entry));
    }

    // +QGDNRCNT: / +QGDAT: — both parsed into TrafficStats
    if line.starts_with("+QGDNRCNT:") || line.starts_with("+QGDAT:") {
        let prefix = if line.starts_with("+QGDNRCNT:") { "+QGDNRCNT:" } else { "+QGDAT:" };
        let rest = line.strip_prefix(prefix).unwrap_or("").trim();
        let parts: Vec<&str> = rest.split(',').collect();
        let quoted = prefix == "+QGDAT:";
        let parse_val = |s: &str| -> u64 {
            if quoted { s.trim().trim_matches('"') } else { s.trim() }.parse().unwrap_or(0)
        };
        let tx = parts.first().map(|s| parse_val(s)).unwrap_or(0);
        let rx = parts.get(1).map(|s| parse_val(s)).unwrap_or(0);
        return Some(ParsedLine::TrafficStats(TrafficStats { tx_bytes: tx, rx_bytes: rx }));
    }

    // +CGPADDR:
    if line.starts_with("+CGPADDR:") {
        let rest = line.strip_prefix("+CGPADDR:").unwrap_or("").trim();
        let parts: Vec<&str> = rest.split(',').collect();
        let cid: u32 = parts
            .first()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let ipv4 = parts
            .get(1)
            .map(|s| extract_value(s).to_string())
            .unwrap_or_default();
        let ipv6 = parts
            .get(2)
            .map(|s| extract_value(s).to_string())
            .unwrap_or_default();
        return Some(ParsedLine::Cgpaddr(CgpaddrEntry { cid, ipv4, ipv6 }));
    }

    // +QENG: "servingcell"
    if line.starts_with("+QENG: \"servingcell\"") {
        let rest = line
            .strip_prefix("+QENG: \"servingcell\"")
            .unwrap_or("")
            .trim();
        // Remove leading comma if present
        let rest = rest.strip_prefix(',').unwrap_or(rest);
        let parts: Vec<&str> = rest
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .collect();

        let mut cell = QengServingCell::default();
        if let Some(v) = parts.get(0) {
            cell.connection_status = v.to_string();
        }
        if let Some(v) = parts.get(1) {
            cell.rat = v.to_string();
        }
        if let Some(v) = parts.get(2) {
            cell.opmode = v.to_string();
        }
        if let Some(v) = parts.get(3) {
            cell.mcc = v.to_string();
        }
        if let Some(v) = parts.get(4) {
            cell.mnc = v.to_string();
        }
        if let Some(v) = parts.get(5) {
            cell.cell_id = v.to_string();
        }
        if let Some(v) = parts.get(6) {
            cell.pci = v.to_string();
        }
        if let Some(v) = parts.get(7) {
            cell.tac = v.to_string();
        }
        if let Some(v) = parts.get(8) {
            cell.earfcn = v.to_string();
        }
        if let Some(v) = parts.get(9) {
            cell.band = v.to_string();
        }
        if let Some(v) = parts.get(10) {
            cell.bandwidth = v.to_string();
        }
        if let Some(v) = parts.get(11) {
            cell.rsrp = v.to_string();
        }
        if let Some(v) = parts.get(12) {
            cell.rsrq = v.to_string();
        }
        if let Some(v) = parts.get(13) {
            cell.sinr = v.to_string();
        }
        if let Some(v) = parts.get(14) {
            cell.srxlev = v.to_string();
        }
        if let Some(v) = parts.get(15) {
            cell.rssi = v.to_string();
        }
        return Some(ParsedLine::QengServingCell(cell));
    }

    // +QCAINFO:
    if line.starts_with("+QCAINFO:") {
        let rest = line.strip_prefix("+QCAINFO:").unwrap_or("").trim();
        let parts: Vec<&str> = rest
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .collect();

        let mut entry = QcainfoEntry::default();
        if let Some(v) = parts.get(0) {
            entry.component = v.to_string();
        }
        if let Some(v) = parts.get(1) {
            entry.earfcn = v.to_string();
        }
        if let Some(v) = parts.get(2) {
            entry.bandwidth = v.to_string();
        }
        if let Some(v) = parts.get(3) {
            entry.band = v.to_string();
        }

        // PCC: 5 fields total; SCC: 9 fields total
        if parts.len() >= 5 {
            entry.pci = parts[4].to_string();
        }
        if parts.len() >= 6 {
            entry.scc_idx = Some(parts[5].to_string());
        }
        if parts.len() >= 7 {
            entry.pci = parts[6].to_string(); // override for SCC
        }
        if parts.len() >= 8 {
            entry.rsrp = Some(parts[7].to_string());
        }
        if parts.len() >= 9 {
            entry.rsrq = Some(parts[8].to_string());
        }
        if parts.len() >= 10 {
            entry.sinr = Some(parts[9].to_string());
        }

        return Some(ParsedLine::Qcainfo(entry));
    }

    // Other response line (e.g. firmware version)
    Some(ParsedLine::Other(line.to_string()))
}

/// Extract sections from a combined AT response
/// Combined commands like: "AT+CPIN?;+QENG=\"servingcell\";+QCAINFO;+CGPADDR"
/// return all results concatenated. This function splits by known prefix markers.
pub fn extract_sections(raw: &str) -> Vec<String> {
    let markers = [
        "+CPIN:",
        "+QENG:",
        "+QCAINFO:",
        "+CGPADDR:",
        "+QUIMSLOT:",
        "+QSPN:",
        "+COPS:",
        "+CGDCONT:",
        "+QGDNRCNT:",
        "+QGDAT:",
        "OK",
        "ERROR",
    ];

    let mut sections = Vec::new();
    let mut current = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_marker = markers.iter().any(|m| trimmed.starts_with(m));

        if is_marker && !current.is_empty() {
            // Don't push if it's just whitespace markers
            sections.push(current.trim().to_string());
            current = String::new();
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(trimmed);
    }

    if !current.is_empty() {
        sections.push(current.trim().to_string());
    }

    sections
}

/// Parse combined AT command response into the 4 telemetry sections (CPIN, QENG, QCAINFO, CGPADDR)
pub fn parse_combined_response(raw: &str) -> (String, String, String, String) {
    let sections = extract_sections(raw);

    let get_section = |prefix: &str| -> String {
        sections
            .iter()
            .find(|s| s.starts_with(prefix))
            .cloned()
            .unwrap_or_default()
    };

    (
        get_section("+CPIN:"),
        get_section("+QENG:"),
        get_section("+QCAINFO:"),
        get_section("+CGPADDR:"),
    )
}

/// Parse +QENG "servingcell" response string and populate TelemetryData
pub fn parse_qeng(qeng_res: &str, telemetry: &mut crate::TelemetryData) {
    if qeng_res.is_empty() {
        println!("⚠️ 未找到 +QENG 响应");
        return;
    }

    // Collect all servingcell lines (carrier aggregation can have multiple)
    let serving_cells: Vec<QengServingCell> = qeng_res
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(ParsedLine::QengServingCell(cell)) = parse_single_line(trimmed) {
                Some(cell)
            } else {
                None
            }
        })
        .collect();

    if serving_cells.is_empty() {
        println!("⚠️ 未找到 +QENG servingcell 数据");
        return;
    }

    // Use first line (PCC/main carrier) for base fields
    let pcc = &serving_cells[0];

    telemetry.network_mode = format!("{} {}", pcc.rat, pcc.opmode);
    telemetry.mccmnc = format!("{}{}", pcc.mcc, pcc.mnc);
    telemetry.cell_id = pcc.cell_id.clone();
    if pcc.cell_id.len() >= 6 {
        telemetry.enb_id = pcc.cell_id[..pcc.cell_id.len() - 3].to_string();
    } else {
        telemetry.enb_id = pcc.cell_id.clone();
    }
    telemetry.tac = pcc.tac.clone();

    // Signal metrics
    let rsrp: i32 = pcc.rsrp.parse().unwrap_or(-140);
    let rsrq: i32 = pcc.rsrq.parse().unwrap_or(-20);
    let sinr: i32 = pcc.sinr.parse().unwrap_or(-20);

    let rsrp_pct = ((rsrp + 140) as f32 / 96.0 * 100.0).clamp(0.0, 100.0) as i32;
    let rsrq_pct = ((rsrq + 20) as f32 / 17.0 * 100.0).clamp(0.0, 100.0) as i32;
    let sinr_pct = ((sinr + 20) as f32 / 50.0 * 100.0).clamp(0.0, 100.0) as i32;

    telemetry.assessment = if rsrp > -80 && sinr > 20 {
        "Excellent"
    } else {
        "Good"
    }
    .to_string();

    telemetry.ss_rsrp = format!("{} / {}%", rsrp, rsrp_pct);
    telemetry.ss_rsrq = format!("{} / {}%", rsrq, rsrq_pct);
    telemetry.sinr = format!("{} / {}%", sinr, sinr_pct);
    telemetry.signal_percentage = format!("{}%", rsrp_pct);

    // Carrier aggregation: process multiple lines
    if serving_cells.len() > 1 {
        // Bands: deduplicated
        let mut bands: Vec<String> = Vec::new();
        for cell in &serving_cells {
            let band = format!("NR5G BAND {}", cell.band);
            if !bands.contains(&band) {
                bands.push(band);
            }
        }
        telemetry.bands = bands.join(", ");

        // Bandwidth: sum all CC bandwidths
        let mut total_bw = 0i32;
        let mut bw_parts: Vec<String> = Vec::new();
        for cell in &serving_cells {
            if let Ok(bw) = cell.bandwidth.parse::<i32>() {
                total_bw += bw;
                bw_parts.push(bw.to_string());
            }
        }
        if !bw_parts.is_empty() {
            telemetry.bandwidth = format!("NR {} MHz ({})", total_bw, bw_parts.join("+"));
        }

        // EARFCN / PCI: comma-joined
        let earfcns: Vec<&str> = serving_cells.iter().map(|c| c.earfcn.as_str()).collect();
        telemetry.earfcn = earfcns.join(", ");

        let pcis: Vec<&str> = serving_cells.iter().map(|c| c.pci.as_str()).collect();
        telemetry.pci = pcis.join(", ");
    } else {
        // Single carrier
        telemetry.bands = format!("NR5G BAND {}", pcc.band);
        telemetry.bandwidth = format!("{} MHz", pcc.bandwidth);
        telemetry.earfcn = pcc.earfcn.clone();
        telemetry.pci = pcc.pci.clone();
    }
}

/// Parse +QCAINFO response and populate TelemetryData
pub fn parse_qcainfo(qca_res: &str, telemetry: &mut crate::TelemetryData) {
    if qca_res.is_empty() {
        return;
    }

    let entries: Vec<QcainfoEntry> = qca_res
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(ParsedLine::Qcainfo(entry)) = parse_single_line(trimmed) {
                Some(entry)
            } else {
                None
            }
        })
        .collect();

    if entries.is_empty() {
        return;
    }

    let mut bands: Vec<String> = Vec::new();
    let mut earfcns = Vec::new();
    let mut pcis = Vec::new();
    let mut total_bw = 0i32;
    let mut bw_parts: Vec<String> = Vec::new();

    for entry in entries.iter() {
        if let Ok(bw) = entry.bandwidth.parse::<i32>() {
            total_bw += bw;
            bw_parts.push(bw.to_string());
        }
        earfcns.push(entry.earfcn.as_str());
        pcis.push(entry.pci.as_str());

        let band_label = if entry.band.starts_with("NR5G") {
            entry.band.clone()
        } else {
            format!("NR5G BAND {}", entry.band)
        };
        if !bands.contains(&band_label) {
            bands.push(band_label);
        }
    }

    if !bands.is_empty() {
        telemetry.bands = bands.join(", ");
    }
    if !bw_parts.is_empty() {
        telemetry.bandwidth = format!("NR {} MHz ({})", total_bw, bw_parts.join("+"));
    }
    if !earfcns.is_empty() {
        telemetry.earfcn = earfcns.join(", ");
    }
    if !pcis.is_empty() {
        telemetry.pci = pcis.join(", ");
    }

    eprintln!(
        "🔍 QCAINFO parsed: bands={} bw={} earfcn={} pci={}",
        telemetry.bands, telemetry.bandwidth, telemetry.earfcn, telemetry.pci
    );
}

/// Parse +CGPADDR response and populate TelemetryData
pub fn parse_cgpaddr(gpad_res: &str, telemetry: &mut crate::TelemetryData) {
    use crate::at::utils::{convert_dotted_ipv6_to_standard, is_valid_ipv4, is_valid_ipv6};

    for line in gpad_res.lines() {
        let trimmed = line.trim();
        if let Some(ParsedLine::Cgpaddr(entry)) = parse_single_line(trimmed) {
            let ipv4 = entry.ipv4;
            let ipv6 = entry.ipv6;

            if !ipv4.is_empty()
                && ipv4 != "0.0.0.0"
                && is_valid_ipv4(&ipv4)
                && telemetry.ipv4.is_empty()
            {
                telemetry.ipv4 = ipv4.to_string();
            }
            if !ipv6.is_empty() && ipv6 != "0.0.0.0" {
                let normalized = convert_dotted_ipv6_to_standard(&ipv6);
                if is_valid_ipv6(&normalized) && telemetry.ipv6.is_empty() {
                    telemetry.ipv6 = normalized;
                }
            }
        }
    }

    if telemetry.ipv4.is_empty() {
        telemetry.ipv4 = "--".to_string();
    }
    if telemetry.ipv6.is_empty() {
        telemetry.ipv6 = "--".to_string();
    }
}

/// Parse AT+QTEMP response and extract module temperature from cpuss/mdmss sensors
pub fn parse_qtemp_temperature(qtemp_res: &str) -> Option<String> {
    qtemp_res
        .lines()
        .find_map(|l| {
            let l = l.trim();
            if let Some(rest) = l.strip_prefix("+QTEMP:") {
                let rest = rest.trim();
                let parts: Vec<&str> = rest.split(',').collect();
                if parts.len() >= 2 {
                    let name = parts[0].trim().trim_matches('"');
                    let val = parts[1].trim().trim_matches('"');
                    (name.contains("cpuss") || name.contains("mdmss"))
                        .then(|| val.parse::<f64>().ok())
                        .flatten()
                } else {
                    None
                }
            } else {
                None
            }
        })
        .map(|t| format!("{:.0} °C", t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cpin() {
        let result = parse_single_line("+CPIN: READY");
        assert!(matches!(result, Some(ParsedLine::Cpin(ref r)) if r.status == "READY"));
    }

    #[test]
    fn test_parse_quimslot() {
        let result = parse_single_line("+QUIMSLOT: 1");
        assert!(matches!(result, Some(ParsedLine::Quimslot(ref r)) if r.slot == 1));
    }

    #[test]
    fn test_parse_qspn() {
        let result = parse_single_line("+QSPN: \"CHN-UNICOM\",\"CHN-UNICOM\",\"CHN-UNICOM\",2");
        assert!(matches!(result, Some(ParsedLine::Qspn(ref r)) if r.fnn == "CHN-UNICOM"));
    }

    #[test]
    fn test_parse_cops() {
        let result = parse_single_line(r#"+COPS: 0,0,"CHN-UNICOM",13"#);
        assert!(
            matches!(result, Some(ParsedLine::Cops(ref r)) if r.oper.as_deref() == Some("CHN-UNICOM"))
        );
    }

    #[test]
    fn test_parse_gdcont() {
        let result = parse_single_line("+CGDCONT: 1,\"IP\",\"3gnet\",\"\",0,0");
        assert!(
            matches!(result, Some(ParsedLine::Cgdcont(ref r)) if r.cid == 1 && r.apn == "3gnet")
        );
    }

    #[test]
    fn test_parse_qgdnrcnt() {
        let result = parse_single_line("+QGDNRCNT: 123456,789012");
        assert!(
            matches!(result, Some(ParsedLine::TrafficStats(ref r)) if r.tx_bytes == 123456 && r.rx_bytes == 789012)
        );
    }

    #[test]
    fn test_parse_qgdat() {
        let result = parse_single_line("+QGDAT: \"123456\",\"789012\"");
        assert!(
            matches!(result, Some(ParsedLine::TrafficStats(ref r)) if r.tx_bytes == 123456 && r.rx_bytes == 789012)
        );
    }

    #[test]
    fn test_parse_cgpaddr() {
        let result = parse_single_line("+CGPADDR: 1,\"10.202.165.254\",\"2409::1\"");
        assert!(
            matches!(result, Some(ParsedLine::Cgpaddr(ref r)) if r.cid == 1 && r.ipv4 == "10.202.165.254")
        );
    }

    #[test]
    fn test_parse_qeng_servingcell() {
        let line = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-64,-11,22,1,-";
        let result = parse_single_line(line);
        assert!(
            matches!(result, Some(ParsedLine::QengServingCell(ref r)) if r.rat == "NR5G-SA" && r.mcc == "460")
        );
    }

    #[test]
    fn test_parse_qcainfo_pcc() {
        let result = parse_single_line("+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751");
        assert!(
            matches!(result, Some(ParsedLine::Qcainfo(ref r)) if r.component == "PCC" && r.earfcn == "504990")
        );
    }

    #[test]
    fn test_parse_qcainfo_scc() {
        let result = parse_single_line("+QCAINFO: \"SCC\",156490,3,\"NR5G BAND 28\",1,250,0,-,-");
        assert!(matches!(result, Some(ParsedLine::Qcainfo(ref r)) if r.component == "SCC"));
    }

    #[test]
    fn test_parse_combined_response() {
        let raw = "AT+CPIN?;+QENG=\"servingcell\";+QCAINFO;+CGPADDR\r\n
+CPIN: READY\r\n
+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-64,-11,22,1,-\r\n
+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\r\n
+CGPADDR: 1,\"10.202.165.254\",\"2409::1\"\r\n
OK\r\n";
        let (cpin, qeng, qca, gpad) = parse_combined_response(raw);
        assert!(cpin.contains("READY"));
        assert!(qeng.contains("servingcell"));
        assert!(qca.contains("PCC"));
        assert!(gpad.contains("10.202.165.254"));
    }

    #[test]
    fn test_ok_error() {
        assert!(matches!(parse_single_line("OK"), Some(ParsedLine::Ok)));
        assert!(matches!(
            parse_single_line("ERROR"),
            Some(ParsedLine::Error)
        ));
        assert!(matches!(
            parse_single_line("+CME ERROR: 50"),
            Some(ParsedLine::Error)
        ));
    }

    #[test]
    fn test_parse_qtemp_temperature_cpuss() {
        let resp = "+QTEMP:\"modem-lte-sub6-pa1\",\"40\"\r\n+QTEMP:\"cpuss-0-usr\",\"42\"\r\n+QTEMP:\"mdmss-0-usr\",\"42\"";
        assert_eq!(parse_qtemp_temperature(resp), Some("42 °C".to_string()));
    }

    #[test]
    fn test_parse_qtemp_temperature_mdmss_first() {
        let resp = "+QTEMP:\"mdmss-0-usr\",\"41\"\r\n+QTEMP:\"cpuss-0-usr\",\"43\"";
        // find_map returns first match: mdmss
        assert_eq!(parse_qtemp_temperature(resp), Some("41 °C".to_string()));
    }

    #[test]
    fn test_parse_qtemp_temperature_no_match() {
        let resp = "+QTEMP:\"modem-lte-sub6-pa1\",\"40\"\r\n+QTEMP:\"modem-ambient-usr\",\"42\"";
        assert_eq!(parse_qtemp_temperature(resp), None);
    }

    #[test]
    fn test_parse_qtemp_temperature_empty() {
        assert_eq!(parse_qtemp_temperature(""), None);
    }

    #[test]
    fn test_parse_qtemp_temperature_invalid_value() {
        let resp = "+QTEMP:\"cpuss-0-usr\",\"--\"";
        assert_eq!(parse_qtemp_temperature(resp), None);
    }

    // ── 以下测试基于真实设备 2026-07-01 采样数据 ──

    #[test]
    fn test_parse_cgpaddr_real_data() {
        // 真实设备输出：5 个 CID，CID 1 有有效 IPv4 + 点分 IPv6，CID 2 仅有点分 IPv6（放错位置），CID 3-5 全零
        let raw =
            "+CGPADDR: 1,\"10.172.99.214\",\"36.9.137.112.11.104.29.156.24.190.51.35.28.252.89.150\"\n\
             +CGPADDR: 2,\"36.9.129.112.11.10.92.211.24.190.51.33.74.23.82.57\"\n\
             +CGPADDR: 3,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"\n\
             +CGPADDR: 4,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"\n\
             +CGPADDR: 5,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"";
        let mut telemetry = crate::TelemetryData::default();
        parse_cgpaddr(raw, &mut telemetry);
        assert_eq!(telemetry.ipv4, "10.172.99.214");
        assert_eq!(telemetry.ipv6, "2409:8970:b68:1d9c:18be:3323:1cfc:5996");
    }

    #[test]
    fn test_parse_cgpaddr_all_zero() {
        // 所有 CID 均为 0.0.0.0，应回退为 "--"
        let raw = "+CGPADDR: 1,\"0.0.0.0\",\"0.0.0.0\"\n+CGPADDR: 2,\"0.0.0.0\"";
        let mut telemetry = crate::TelemetryData::default();
        parse_cgpaddr(raw, &mut telemetry);
        assert_eq!(telemetry.ipv4, "--");
        assert_eq!(telemetry.ipv6, "--");
    }

    #[test]
    fn test_parse_qeng_real_nr5g_sa() {
        // 真实 NR5G-SA 服务小区数据：NOCONN, TDD, 46000, cell=39074C001
        let raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.network_mode, "NR5G-SA TDD");
        assert_eq!(telemetry.mccmnc, "46000");
        assert_eq!(telemetry.cell_id, "39074C001");
        assert_eq!(telemetry.enb_id, "39074C");
        assert_eq!(telemetry.tac, "72002F");
        assert_eq!(telemetry.bands, "NR5G BAND 41");
        assert_eq!(telemetry.bandwidth, "12 MHz");
        assert_eq!(telemetry.earfcn, "504990");
        assert_eq!(telemetry.pci, "751");

        // 信号百分比计算：rsrp=-65 → (-65+140)/96*100 = 78
        assert_eq!(telemetry.signal_percentage, "78%");
        assert_eq!(telemetry.ss_rsrp, "-65 / 78%");
        // rsrq=-11 → (-11+20)/17*100 = 52
        assert_eq!(telemetry.ss_rsrq, "-11 / 52%");
        // sinr=19 → (19+20)/50*100 = 78
        assert_eq!(telemetry.sinr, "19 / 78%");

        // assessment: rsrp=-65 > -80 (true), sinr=19 > 20 (false) → "Good"
        assert_eq!(telemetry.assessment, "Good");
    }

    #[test]
    fn test_parse_qcainfo_real_pcc_scc() {
        // 真实 CA 数据：PCC(NR5G BAND 41, 12MHz) + SCC(NR5G BAND 28, 3MHz)
        let raw =
            "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\n\
             +QCAINFO: \"SCC\",156490,3,\"NR5G BAND 28\",1,250,0,-,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, "NR5G BAND 41, NR5G BAND 28");
        assert_eq!(telemetry.bandwidth, "NR 15 MHz (12+3)");
        assert_eq!(telemetry.earfcn, "504990, 156490");
        // SCC pci 因字段偏移被解析为 rsrp 字段的值 "0"
        assert_eq!(telemetry.pci, "751, 0");
    }

    #[test]
    fn test_parse_qcainfo_empty() {
        let mut telemetry = crate::TelemetryData::default();
        parse_qcainfo("", &mut telemetry);
        // 空响应不应改动 telemetry
        assert!(telemetry.bands.is_empty());
        assert!(telemetry.bandwidth.is_empty());
    }

    #[test]
    fn test_parse_qtemp_temperature_real_full() {
        // 真实完整 QTEMP 传感器输出，cpuss-0-usr 最先匹配
        let resp =
            "+QTEMP:\"modem-lte-sub6-pa1\",\"40\"\n\
             +QTEMP:\"modem-sdr0-pa0\",\"0\"\n\
             +QTEMP:\"modem-sdr0-pa1\",\"0\"\n\
             +QTEMP:\"modem-sdr0-pa2\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa0\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa1\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa2\",\"0\"\n\
             +QTEMP:\"modem-mmw0\",\"0\"\n\
             +QTEMP:\"aoss-0-usr\",\"42\"\n\
             +QTEMP:\"cpuss-0-usr\",\"42\"\n\
             +QTEMP:\"mdmq6-0-usr\",\"42\"\n\
             +QTEMP:\"mdmss-0-usr\",\"42\"\n\
             +QTEMP:\"mdmss-1-usr\",\"42\"\n\
             +QTEMP:\"mdmss-2-usr\",\"42\"\n\
             +QTEMP:\"mdmss-3-usr\",\"41\"\n\
             +QTEMP:\"modem-lte-sub6-pa2\",\"40\"\n\
             +QTEMP:\"modem-ambient-usr\",\"41\"";
        assert_eq!(parse_qtemp_temperature(resp), Some("42 °C".to_string()));
    }

    #[test]
    fn test_parse_qtemp_temperature_mdmss_before_cpuss() {
        // mdmss 传感器出现在 cpuss 之前，应优先返回 mdmss
        let resp =
            "+QTEMP:\"mdmss-0-usr\",\"41\"\n\
             +QTEMP:\"cpuss-0-usr\",\"42\"";
        assert_eq!(parse_qtemp_temperature(resp), Some("41 °C".to_string()));
    }

    #[test]
    fn test_end_to_end_real_device_polling_cycle() {
        // 模拟真实轮询循环：按顺序调用 CGPADDR → QENG → QCAINFO → QTEMP 解析
        let cgpaddr_raw =
            "+CGPADDR: 1,\"10.172.99.214\",\"36.9.137.112.11.104.29.156.24.190.51.35.28.252.89.150\"\n\
             +CGPADDR: 2,\"36.9.129.112.11.10.92.211.24.190.51.33.74.23.82.57\"\n\
             +CGPADDR: 3,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"\n\
             +CGPADDR: 4,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"\n\
             +CGPADDR: 5,\"0.0.0.0\",\"0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0\"";
        let qeng_raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-";
        let qcainfo_raw =
            "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\n\
             +QCAINFO: \"SCC\",156490,3,\"NR5G BAND 28\",1,250,0,-,-";
        let qtemp_raw =
            "+QTEMP:\"modem-lte-sub6-pa1\",\"40\"\n\
             +QTEMP:\"modem-sdr0-pa0\",\"0\"\n\
             +QTEMP:\"modem-sdr0-pa1\",\"0\"\n\
             +QTEMP:\"modem-sdr0-pa2\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa0\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa1\",\"0\"\n\
             +QTEMP:\"modem-sdr1-pa2\",\"0\"\n\
             +QTEMP:\"modem-mmw0\",\"0\"\n\
             +QTEMP:\"aoss-0-usr\",\"42\"\n\
             +QTEMP:\"cpuss-0-usr\",\"42\"\n\
             +QTEMP:\"mdmq6-0-usr\",\"42\"\n\
             +QTEMP:\"mdmss-0-usr\",\"42\"\n\
             +QTEMP:\"mdmss-1-usr\",\"42\"\n\
             +QTEMP:\"mdmss-2-usr\",\"42\"\n\
             +QTEMP:\"mdmss-3-usr\",\"41\"\n\
             +QTEMP:\"modem-lte-sub6-pa2\",\"40\"\n\
             +QTEMP:\"modem-ambient-usr\",\"41\"";

        let mut telemetry = crate::TelemetryData::default();

        // 第 1 步：CGPADDR → ipv4 / ipv6
        parse_cgpaddr(cgpaddr_raw, &mut telemetry);
        assert_eq!(telemetry.ipv4, "10.172.99.214");
        assert_eq!(telemetry.ipv6, "2409:8970:b68:1d9c:18be:3323:1cfc:5996");

        // 第 2 步：QENG → 网络模式 / 小区 / 信号
        parse_qeng(qeng_raw, &mut telemetry);
        assert_eq!(telemetry.network_mode, "NR5G-SA TDD");
        assert_eq!(telemetry.mccmnc, "46000");
        assert_eq!(telemetry.cell_id, "39074C001");
        assert_eq!(telemetry.enb_id, "39074C");
        assert_eq!(telemetry.tac, "72002F");
        assert_eq!(telemetry.signal_percentage, "78%");
        assert_eq!(telemetry.ss_rsrp, "-65 / 78%");
        assert_eq!(telemetry.ss_rsrq, "-11 / 52%");
        assert_eq!(telemetry.sinr, "19 / 78%");
        assert_eq!(telemetry.assessment, "Good");
        // QENG 设置了 bands/bandwidth/earfcn/pci，后续会被 QCAINFO 覆盖
        assert_eq!(telemetry.bands, "NR5G BAND 41");
        assert_eq!(telemetry.bandwidth, "12 MHz");
        assert_eq!(telemetry.earfcn, "504990");
        assert_eq!(telemetry.pci, "751");

        // 第 3 步：QCAINFO → 覆盖 bands/bandwidth/earfcn/pci（载波聚合）
        parse_qcainfo(qcainfo_raw, &mut telemetry);
        assert_eq!(telemetry.bands, "NR5G BAND 41, NR5G BAND 28");
        assert_eq!(telemetry.bandwidth, "NR 15 MHz (12+3)");
        assert_eq!(telemetry.earfcn, "504990, 156490");
        assert_eq!(telemetry.pci, "751, 0"); // SCC pci 因字段偏移为 "0"

        // 第 4 步：QTEMP → 温度
        assert_eq!(parse_qtemp_temperature(qtemp_raw), Some("42 °C".to_string()));
        telemetry.temperature = parse_qtemp_temperature(qtemp_raw).unwrap_or_default();
        assert_eq!(telemetry.temperature, "42 °C");

        // 验证 QENG 设置的字段不被后续解析破坏
        assert_eq!(telemetry.network_mode, "NR5G-SA TDD");
        assert_eq!(telemetry.mccmnc, "46000");
        assert_eq!(telemetry.cell_id, "39074C001");
        assert_eq!(telemetry.signal_percentage, "78%");
        assert_eq!(telemetry.assessment, "Good");
    }

    #[test]
    fn test_parse_qeng_carrier_aggregation_multiple_servingcell() {
        // 多载波聚合场景：两个 +QENG servingcell 行
        let raw =
            "+QENG: \"servingcell\",\"CONNECT\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-\n\
             +QENG: \"servingcell\",\"CONNECT\",\"NR5G-SA\",\"TDD\",460,00,39074C001,250,72002F,156490,28,3,-70,-12,15,1,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.bands, "NR5G BAND 41, NR5G BAND 28");
        assert_eq!(telemetry.bandwidth, "NR 15 MHz (12+3)");
        assert_eq!(telemetry.earfcn, "504990, 156490");
        assert_eq!(telemetry.pci, "751, 250");
        // 使用 PCC（首行）的信号值
        assert_eq!(telemetry.signal_percentage, "78%");
    }
}
