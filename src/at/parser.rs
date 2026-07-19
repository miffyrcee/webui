//! pest-based parsers for AT command responses
//!
//! This module replaces the previous nom-based parsers with pest-based grammars.

use crate::at::{
    response::*,
    utils::decode_hex_ucs2,
};
use pest::Parser;

/// The pest parser generated from grammar.pest
#[derive(pest_derive::Parser)]
#[grammar = "at/grammar.pest"]
#[allow(dead_code)]
pub struct AtParser;

/// Recursively extract all non-COMMA field values from a pest match pair,
/// trimming quotes from quoted values.
fn extract_values(pair: pest::iterators::Pair<'_, Rule>) -> Vec<String> {
    let mut result = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::COMMA {
            continue;
        }
        // Capture string value before moving `inner` into the recursive call
        let str_val = inner.as_str().trim_matches('"').to_string();
        let inner_values = extract_values(inner);
        if inner_values.is_empty() {
            if !str_val.is_empty() {
                result.push(str_val);
            }
        } else {
            result.extend(inner_values);
        }
    }
    result
}

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

/// Parse a combined AT response (e.g. "AT+CPIN?;+QENG=...;+QCAINFO;+CGPADDR")
/// into individual response sections by known prefix markers.
/// Extracts the first line matching each known prefix from the raw output
/// (which may include echo line, trailing OK, etc.).
#[allow(dead_code)]
pub fn parse_combined_response(raw: &str) -> (String, String, String, String) {
    let mut cpin = String::new();
    let mut qeng = String::new();
    let mut qca = String::new();
    let mut gpad = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("+CPIN:") {
            cpin = trimmed.to_string();
        } else if trimmed.starts_with("+QENG:") {
            qeng = trimmed.to_string();
        } else if trimmed.starts_with("+QCAINFO:") {
            if qca.is_empty() {
                qca = trimmed.to_string();
            }
        } else if trimmed.starts_with("+CGPADDR:") {
            if gpad.is_empty() {
                gpad = trimmed.to_string();
            }
        }
    }

    (cpin, qeng, qca, gpad)
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
    Cgcontrdp(CgcontrdpResponse),
    Cnum(CnumResponse),
    Qccid(String),
    Cimi(String),
    Qtemp(QtempResponse),
    Ok,
    Error,
    Other(String),
}

/// 将移远 5G NR 频宽索引代码转换为实际 MHz 数
fn decode_nr_bandwidth(code: i32) -> f64 {
    match code {
        0 => 5.0,
        1 => 10.0,
        2 => 15.0,
        3 => 20.0,
        4 => 25.0,
        5 => 30.0,
        6 => 40.0,
        7 => 50.0,
        8 => 60.0,
        9 => 70.0,
        10 => 80.0,
        11 => 90.0,
        12 => 100.0,
        13 => 200.0,
        14 => 400.0,
        15 => 35.0,
        16 => 45.0,
        other => other as f64,
    }
}

/// 将移远 LTE 频宽代码（Resource Blocks）转换为实际 MHz 数
fn decode_lte_bandwidth(code: i32) -> f64 {
    match code {
        6 => 1.4,
        15 => 3.0,
        25 => 5.0,
        50 => 10.0,
        75 => 15.0,
        100 => 20.0,
        other => other as f64,
    }
}

/// 使用结构化 AST 匹配构建 QcainfoEntry，彻底弃用扁平 extract_values 的位置索引
fn build_qcainfo_entry(pair: pest::iterators::Pair<'_, Rule>) -> QcainfoEntry {
    // 如果收到的是 qcainfo_resp，查找内部实际的 pcc/scc 子规则
    if pair.as_rule() == Rule::qcainfo_resp {
        for child in pair.into_inner() {
            if child.as_rule() == Rule::qcainfo_pcc || child.as_rule() == Rule::qcainfo_scc {
                return build_qcainfo_entry(child);
            }
        }
        return QcainfoEntry::default();
    }

    let mut entry = QcainfoEntry::default();
    match pair.as_rule() {
        Rule::qcainfo_pcc => {
            entry.component = "PCC".to_string();
            for field in pair.into_inner() {
                match field.as_rule() {
                    Rule::earfcn => entry.earfcn = field.as_str().to_string(),
                    Rule::bandwidth => entry.bandwidth = field.as_str().to_string(),
                    Rule::band => entry.band = field.as_str().trim_matches('"').to_string(),
                    Rule::pci => entry.pci = field.as_str().to_string(),
                    _ => {}
                }
            }
        }
        Rule::qcainfo_scc => {
            entry.component = "SCC".to_string();
            for field in pair.into_inner() {
                match field.as_rule() {
                    Rule::earfcn => entry.earfcn = field.as_str().to_string(),
                    Rule::bandwidth => entry.bandwidth = field.as_str().to_string(),
                    Rule::band => entry.band = field.as_str().trim_matches('"').to_string(),
                    Rule::pci => entry.pci = field.as_str().to_string(),
                    Rule::scc_idx => entry.scc_idx = Some(field.as_str().to_string()),
                    Rule::scc_pci => entry.pci = field.as_str().to_string(),
                    Rule::scc_rsrp => entry.rsrp = Some(field.as_str().to_string()),
                    Rule::scc_rsrq => entry.rsrq = Some(field.as_str().to_string()),
                    Rule::scc_sinr => entry.sinr = Some(field.as_str().to_string()),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    entry
}

/// Parse a single line of AT response using pest-based grammar
pub fn parse_single_line(line: &str) -> Option<ParsedLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    match AtParser::parse(Rule::at_response_line, trimmed) {
        Ok(pairs) => {
            for pair in pairs {
                // 通过子节点提取匹配各 AT 响应行
                for inner in pair.into_inner() {
                    return Some(match inner.as_rule() {
                        Rule::at_ok => ParsedLine::Ok,
                        Rule::at_error | Rule::at_cme_error => ParsedLine::Error,

                        Rule::cpin_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cpin(CpinResponse {
                                status: values.into_iter().next().unwrap_or_default(),
                            })
                        }

                        Rule::quimslot_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Quimslot(QuimslotResponse {
                                slot: values.first().and_then(|s| s.parse().ok()).unwrap_or(1),
                            })
                        }

                        Rule::qspn_resp => {
                            let values = extract_values(inner);
                            let mut resp = QspnResponse::default();
                            if let Some(v) = values.get(0) {
                                let decoded = decode_hex_ucs2(v);
                                resp.fnn = if decoded.is_empty() { v.clone() } else { decoded };
                            }
                            if let Some(v) = values.get(1) { resp.snn = v.clone(); }
                            if let Some(v) = values.get(2) { resp.spn = v.clone(); }
                            if let Some(v) = values.get(3) { resp.alphabet = v.clone(); }
                            ParsedLine::Qspn(resp)
                        }

                        Rule::cops_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cops(CopsResponse {
                                mode: values.get(0).cloned().unwrap_or_default(),
                                format: values.get(1).cloned(),
                                oper: values.get(2).cloned(),
                                act: values.get(3).cloned(),
                            })
                        }

                        Rule::cgdcont_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cgdcont(CgdcontEntry {
                                cid: values.get(0).and_then(|s| s.parse().ok()).unwrap_or(0),
                                pdp_type: values.get(1).cloned().unwrap_or_default(),
                                apn: values.get(2).cloned().unwrap_or_default(),
                                pdp_addr: values.get(3).cloned().unwrap_or_default(),
                                d_comp: values.get(4).cloned().unwrap_or_default(),
                                h_comp: values.get(5).cloned().unwrap_or_default(),
                            })
                        }

                        Rule::qgdnrcnt_resp | Rule::qgdat_resp => {
                            let values = extract_values(inner);
                            ParsedLine::TrafficStats(TrafficStats {
                                tx_bytes: values.get(0).and_then(|s| s.parse().ok()).unwrap_or(0),
                                rx_bytes: values.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
                            })
                        }

                        Rule::cgpaddr_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cgpaddr(CgpaddrEntry {
                                cid: values.get(0).and_then(|s| s.parse().ok()).unwrap_or(0),
                                ipv4: values.get(1).cloned().unwrap_or_default(),
                                ipv6: values.get(2).cloned().unwrap_or_default(),
                            })
                        }

                        Rule::qeng_servingcell => {
                            let values = extract_values(inner);
                            let mut cell = QengServingCell::default();
                            if let Some(v) = values.get(0) { cell.connection_status = v.clone(); }
                            if let Some(v) = values.get(1) { cell.rat = v.clone(); }
                            if let Some(v) = values.get(2) { cell.opmode = v.clone(); }
                            if let Some(v) = values.get(3) { cell.mcc = v.clone(); }
                            if let Some(v) = values.get(4) { cell.mnc = v.clone(); }
                            if let Some(v) = values.get(5) { cell.cell_id = v.clone(); }
                            if let Some(v) = values.get(6) { cell.pci = v.clone(); }
                            if let Some(v) = values.get(7) { cell.tac = v.clone(); }
                            if let Some(v) = values.get(8) { cell.earfcn = v.clone(); }
                            if let Some(v) = values.get(9) { cell.band = v.clone(); }
                            if let Some(v) = values.get(10) { cell.bandwidth = v.clone(); }
                            if let Some(v) = values.get(11) { cell.rsrp = v.clone(); }
                            if let Some(v) = values.get(12) { cell.rsrq = v.clone(); }
                            if let Some(v) = values.get(13) { cell.sinr = v.clone(); }
                            if let Some(v) = values.get(14) { cell.srxlev = v.clone(); }
                            if let Some(v) = values.get(15) { cell.rssi = v.clone(); }
                            ParsedLine::QengServingCell(cell)
                        }

                        Rule::qcainfo_resp => {
                            ParsedLine::Qcainfo(build_qcainfo_entry(inner))
                        }

                        Rule::cgcontrdp_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cgcontrdp(CgcontrdpResponse {
                                apn: values.get(2).cloned().unwrap_or_default(),
                            })
                        }

                        Rule::cnum_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cnum(CnumResponse {
                                number: values.get(1).cloned().unwrap_or_default(),
                            })
                        }

                        Rule::qccid_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Qccid(values.into_iter().next().unwrap_or_default())
                        }

                        Rule::cimi_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Cimi(values.into_iter().next().unwrap_or_default())
                        }

                        Rule::qtemp_resp => {
                            let values = extract_values(inner);
                            ParsedLine::Qtemp(QtempResponse {
                                sensor: values.get(0).cloned().unwrap_or_default(),
                                temperature: values.get(1)
                                    .and_then(|v| v.parse::<f64>().ok()),
                            })
                        }

                        Rule::cgmr_line => ParsedLine::Other(trimmed.to_string()),
                        _ => ParsedLine::Other(trimmed.to_string()),
                    });
                }
            }
            None
        }
        Err(_) => Some(ParsedLine::Other(trimmed.to_string())),
    }
}

/// Extract sections from a combined AT response
/// Combined commands like: "AT+CPIN?;+QENG=\"servingcell\";+QCAINFO;+CGPADDR"
/// return all results concatenated. This function splits by known prefix markers.
/// Parse +QENG "servingcell" response string and populate TelemetryData
pub fn parse_qeng(qeng_res: &str, telemetry: &mut crate::TelemetryData) {
    if qeng_res.is_empty() {
        println!("未找到 +QENG 响应");
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
        println!("未找到 +QENG servingcell 数据");
        return;
    }

    // Use first line (PCC/main carrier) for base fields
    let pcc = &serving_cells[0];

    telemetry.network_mode = Some(format!("{} {}", pcc.rat, pcc.opmode));
    telemetry.mccmnc = Some(format!("{}{}", pcc.mcc, pcc.mnc));
    telemetry.cell_id = Some(pcc.cell_id.clone());
    if pcc.cell_id.len() >= 6 {
        telemetry.enb_id = Some(pcc.cell_id[..pcc.cell_id.len() - 3].to_string());
    } else {
        telemetry.enb_id = Some(pcc.cell_id.clone());
    }
    telemetry.tac = Some(pcc.tac.clone());

    // Signal metrics
    let rsrp: i32 = pcc.rsrp.parse().unwrap_or(-140);
    let rsrq: i32 = pcc.rsrq.parse().unwrap_or(-20);
    let sinr: i32 = pcc.sinr.parse().unwrap_or(-20);

    let rsrp_pct = ((rsrp + 140) as f32 / 96.0 * 100.0).clamp(0.0, 100.0) as i32;
    let rsrq_pct = ((rsrq + 20) as f32 / 17.0 * 100.0).clamp(0.0, 100.0) as i32;
    let sinr_pct = ((sinr + 20) as f32 / 50.0 * 100.0).clamp(0.0, 100.0) as i32;

    telemetry.assessment = Some(if rsrp > -80 && sinr > 20 {
        "Excellent"
    } else {
        "Good"
    }
    .to_string());

    telemetry.ss_rsrp = Some(format!("{} / {}%", rsrp, rsrp_pct));
    telemetry.ss_rsrq = Some(format!("{} / {}%", rsrq, rsrq_pct));
    telemetry.sinr = Some(format!("{} / {}%", sinr, sinr_pct));
    telemetry.signal_percentage = Some(format!("{}%", rsrp_pct));

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
        telemetry.bands = Some(bands.join(", "));

        // Bandwidth: decode codes then sum across all CCs
        let mut total_bw = 0.0f64;
        let mut bw_parts: Vec<String> = Vec::new();
        for cell in &serving_cells {
            if let Ok(code) = cell.bandwidth.parse::<i32>() {
                let is_nr = cell.rat.starts_with("NR5G");
                let actual_bw = if is_nr {
                    decode_nr_bandwidth(code)
                } else {
                    decode_lte_bandwidth(code)
                };
                total_bw += actual_bw;
                bw_parts.push(actual_bw.to_string());
            }
        }
        if !bw_parts.is_empty() {
            telemetry.bandwidth = Some(format!("NR {} MHz ({})", total_bw, bw_parts.join("+")));
        }

        // EARFCN / PCI: comma-joined
        let earfcns: Vec<&str> = serving_cells.iter().map(|c| c.earfcn.as_str()).collect();
        telemetry.earfcn = Some(earfcns.join(", "));

        let pcis: Vec<&str> = serving_cells.iter().map(|c| c.pci.as_str()).collect();
        telemetry.pci = Some(pcis.join(", "));
    } else {
        // Single carrier
        telemetry.bands = Some(format!("NR5G BAND {}", pcc.band));
        let bw_value = pcc.bandwidth.parse::<i32>().map(|code| {
            if pcc.rat.starts_with("NR5G") {
                decode_nr_bandwidth(code)
            } else {
                decode_lte_bandwidth(code)
            }
        }).unwrap_or(0.0);
        telemetry.bandwidth = Some(format!("{} MHz", bw_value));
        telemetry.earfcn = Some(pcc.earfcn.clone());
        telemetry.pci = Some(pcc.pci.clone());
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
    let mut total_bw = 0.0f64;
    let mut bw_parts: Vec<String> = Vec::new();

    for entry in entries.iter() {
        if let Ok(code) = entry.bandwidth.parse::<i32>() {
            let is_nr = entry.band.starts_with("NR5G");
            let actual_bw = if is_nr {
                decode_nr_bandwidth(code)
            } else {
                decode_lte_bandwidth(code)
            };
            total_bw += actual_bw;
            bw_parts.push(actual_bw.to_string());
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
        telemetry.bands = Some(bands.join(", "));
    }
    if !bw_parts.is_empty() {
        if bw_parts.len() > 1 {
            telemetry.bandwidth = Some(format!("NR {} MHz ({})", total_bw, bw_parts.join("+")));
        } else {
            telemetry.bandwidth = Some(format!("NR {} MHz", total_bw));
        }
    }
    if !earfcns.is_empty() {
        telemetry.earfcn = Some(earfcns.join(", "));
    }
    if !pcis.is_empty() {
        telemetry.pci = Some(pcis.join(", "));
    }

    crate::push_log("INFO", "QCAINFO", &format!(
        "QCAINFO parsed: bands={:?} bw={:?} earfcn={:?} pci={:?}",
        telemetry.bands, telemetry.bandwidth, telemetry.earfcn, telemetry.pci
    ));
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
                && telemetry.ipv4.is_none()
            {
                telemetry.ipv4 = Some(ipv4.to_string());
            }
            if !ipv6.is_empty() && ipv6 != "0.0.0.0" {
                let normalized = convert_dotted_ipv6_to_standard(&ipv6);
                if is_valid_ipv6(&normalized) && telemetry.ipv6.is_none() {
                    telemetry.ipv6 = Some(normalized);
                }
            }
        }
    }

    // Don't set default "--" for Option<String>; keep as None
}

/// Parse AT+QTEMP response and extract module temperature from cpuss/mdmss sensors
pub fn parse_qtemp_temperature(qtemp_res: &str) -> Option<String> {
    qtemp_res
        .lines()
        .find_map(|l| {
            match parse_single_line(l) {
                Some(ParsedLine::Qtemp(qtemp)) => {
                    if qtemp.sensor.contains("cpuss") || qtemp.sensor.contains("mdmss") {
                        qtemp.temperature.map(|t| format!("{:.0} °C", t))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
}

/// Parse +CREG response to get network registration status
pub fn parse_net_status(creg_raw: &str) -> String {
    if let Ok(pairs) = AtParser::parse(Rule::creg_resp, creg_raw) {
        for pair in pairs {
            let values = extract_values(pair);
            if let Some(stat) = values.get(1) {
                return match stat.as_str() {
                    "1" => "Registered (home)",
                    "5" => "Registered (roaming)",
                    "2" | "3" | "4" => "Not registered",
                    _ => "Unknown",
                }
                .to_string();
            }
        }
    }
    "Unknown".to_string()
}

/// Parse +CSQ response to get signal quality in dBm
pub fn parse_signal_quality(csq_raw: &str) -> String {
    if let Ok(pairs) = AtParser::parse(Rule::csq_resp, csq_raw) {
        for pair in pairs {
            let values = extract_values(pair);
            if let Some(rssi_str) = values.get(0) {
                if let Ok(rssi) = rssi_str.parse::<i32>() {
                    if rssi == 99 {
                        return "Unknown".to_string();
                    }
                    let dbm = -113 + (rssi * 2);
                    return format!("{} dBm", dbm);
                }
            }
        }
    }
    "Unknown".to_string()
}

/// Parse AT+COPS=? scan result into network list
pub fn parse_cops_scan(resp: &str) -> Vec<serde_json::Value> {
    let mut networks = Vec::new();
    for line in resp.lines() {
        let line = line.trim();
        if !line.starts_with("+COPS:") { continue; }
        let body = line.strip_prefix("+COPS:").unwrap_or("").trim();
        let mut depth = 0i32;
        let mut start: Option<usize> = None;
        for (i, ch) in body.char_indices() {
            match ch {
                '(' => {
                    depth += 1;
                    if depth == 1 { start = Some(i + 1); }
                }
                ')' => {
                    if depth == 1 {
                        if let Some(s) = start {
                            let entry = &body[s..i];
                            let parts: Vec<&str> = entry.split(',').map(|s| s.trim().trim_matches('"')).collect();
                            if parts.len() >= 5 {
                                let tech = match parts[4].trim() {
                                    "0" | "1" => "GSM",
                                    "2" => "WCDMA",
                                    "3" => "LTE",
                                    "4" | "5" | "6" => "NR5G",
                                    _ => "Unknown",
                                };
                                let stat = match parts[0] {
                                    "0" => "Unknown",
                                    "1" => "Available",
                                    "2" => "Current",
                                    "3" => "Forbidden",
                                    _ => "Unknown",
                                };
                                networks.push(serde_json::json!({
                                    "operator": parts.get(1).unwrap_or(&""),
                                    "short_name": parts.get(2).unwrap_or(&""),
                                    "mccmnc": parts.get(3).unwrap_or(&""),
                                    "technology": tech,
                                    "status": stat,
                                    "band": "",
                                }));
                            }
                        }
                        start = None;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
    }
    networks
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
        assert_eq!(telemetry.ipv4, Some("10.172.99.214".to_string()));
        assert_eq!(telemetry.ipv6, Some("2409:8970:b68:1d9c:18be:3323:1cfc:5996".to_string()));
    }

    #[test]
    fn test_parse_cgpaddr_all_zero() {
        // 所有 CID 均为 0.0.0.0，应回退为 "--"
        let raw = "+CGPADDR: 1,\"0.0.0.0\",\"0.0.0.0\"\n+CGPADDR: 2,\"0.0.0.0\"";
        let mut telemetry = crate::TelemetryData::default();
        parse_cgpaddr(raw, &mut telemetry);
        assert!(telemetry.ipv4.is_none());
        assert!(telemetry.ipv6.is_none());
    }

    #[test]
    fn test_parse_qeng_real_nr5g_sa() {
        // 真实 NR5G-SA 服务小区数据：NOCONN, TDD, 46000, cell=39074C001
        let raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.network_mode, Some("NR5G-SA TDD".to_string()));
        assert_eq!(telemetry.mccmnc, Some("46000".to_string()));
        assert_eq!(telemetry.cell_id, Some("39074C001".to_string()));
        assert_eq!(telemetry.enb_id, Some("39074C".to_string()));
        assert_eq!(telemetry.tac, Some("72002F".to_string()));
        assert_eq!(telemetry.bands, Some("NR5G BAND 41".to_string()));
        assert_eq!(telemetry.bandwidth, Some("100 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990".to_string()));
        assert_eq!(telemetry.pci, Some("751".to_string()));

        // 信号百分比计算：rsrp=-65 → (-65+140)/96*100 = 78
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
        assert_eq!(telemetry.ss_rsrp, Some("-65 / 78%".to_string()));
        // rsrq=-11 → (-11+20)/17*100 = 52
        assert_eq!(telemetry.ss_rsrq, Some("-11 / 52%".to_string()));
        // sinr=19 → (19+20)/50*100 = 78
        assert_eq!(telemetry.sinr, Some("19 / 78%".to_string()));

        // assessment: rsrp=-65 > -80 (true), sinr=19 > 20 (false) → "Good"
        assert_eq!(telemetry.assessment, Some("Good".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_real_pcc_scc() {
        // 真实 CA 数据：PCC(NR5G BAND 41, 12=100MHz) + SCC(NR5G BAND 28, 3=20MHz)
        let raw =
            "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\n\
             +QCAINFO: \"SCC\",156490,3,\"NR5G BAND 28\",1,250,0,-,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("NR5G BAND 41, NR5G BAND 28".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 120 MHz (100+20)".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990, 156490".to_string()));
        // SCC pci 通过 grammar 修复正确解析为 250
        assert_eq!(telemetry.pci, Some("751, 250".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_single_carrier() {
        // 单载波场景：QCAINFO 仅返回一行 PCC，频宽不应有冗余括号
        let raw = "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751";
        let mut telemetry = crate::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("NR5G BAND 41".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 100 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990".to_string()));
        assert_eq!(telemetry.pci, Some("751".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_empty() {
        let mut telemetry = crate::TelemetryData::default();
        parse_qcainfo("", &mut telemetry);
        // 空响应不应改动 telemetry
        assert!(telemetry.bands.is_none());
        assert!(telemetry.bandwidth.is_none());
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
        assert_eq!(telemetry.ipv4, Some("10.172.99.214".to_string()));
        assert_eq!(telemetry.ipv6, Some("2409:8970:b68:1d9c:18be:3323:1cfc:5996".to_string()));

        // 第 2 步：QENG → 网络模式 / 小区 / 信号
        parse_qeng(qeng_raw, &mut telemetry);
        assert_eq!(telemetry.network_mode, Some("NR5G-SA TDD".to_string()));
        assert_eq!(telemetry.mccmnc, Some("46000".to_string()));
        assert_eq!(telemetry.cell_id, Some("39074C001".to_string()));
        assert_eq!(telemetry.enb_id, Some("39074C".to_string()));
        assert_eq!(telemetry.tac, Some("72002F".to_string()));
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
        assert_eq!(telemetry.ss_rsrp, Some("-65 / 78%".to_string()));
        assert_eq!(telemetry.ss_rsrq, Some("-11 / 52%".to_string()));
        assert_eq!(telemetry.sinr, Some("19 / 78%".to_string()));
        assert_eq!(telemetry.assessment, Some("Good".to_string()));
        // QENG 设置了 bands/bandwidth/earfcn/pci，后续会被 QCAINFO 覆盖
        assert_eq!(telemetry.bands, Some("NR5G BAND 41".to_string()));
        assert_eq!(telemetry.bandwidth, Some("100 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990".to_string()));
        assert_eq!(telemetry.pci, Some("751".to_string()));

        // 第 3 步：QCAINFO → 覆盖 bands/bandwidth/earfcn/pci（载波聚合）
        parse_qcainfo(qcainfo_raw, &mut telemetry);
        assert_eq!(telemetry.bands, Some("NR5G BAND 41, NR5G BAND 28".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 120 MHz (100+20)".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990, 156490".to_string()));
        assert_eq!(telemetry.pci, Some("751, 250".to_string())); // SCC pci 正确解析为 250

        // 第 4 步：QTEMP → 温度
        assert_eq!(parse_qtemp_temperature(qtemp_raw), Some("42 °C".to_string()));
        telemetry.temperature = parse_qtemp_temperature(qtemp_raw);
        assert_eq!(telemetry.temperature, Some("42 °C".to_string()));

        // 验证 QENG 设置的字段不被后续解析破坏
        assert_eq!(telemetry.network_mode, Some("NR5G-SA TDD".to_string()));
        assert_eq!(telemetry.mccmnc, Some("46000".to_string()));
        assert_eq!(telemetry.cell_id, Some("39074C001".to_string()));
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
        assert_eq!(telemetry.assessment, Some("Good".to_string()));
    }

    #[test]
    fn test_parse_qeng_carrier_aggregation_multiple_servingcell() {
        // 多载波聚合场景：两个 +QENG servingcell 行
        let raw =
            "+QENG: \"servingcell\",\"CONNECT\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-\n\
             +QENG: \"servingcell\",\"CONNECT\",\"NR5G-SA\",\"TDD\",460,00,39074C001,250,72002F,156490,28,3,-70,-12,15,1,-";
        let mut telemetry = crate::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("NR5G BAND 41, NR5G BAND 28".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 120 MHz (100+20)".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990, 156490".to_string()));
        assert_eq!(telemetry.pci, Some("751, 250".to_string()));
        // 使用 PCC（首行）的信号值
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
    }
}

