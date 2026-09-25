//! Parsers for AT command responses.

use crate::at::{
    response::*,
    utils::decode_hex_ucs2,
};

fn split_at_fields(body: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in body.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current.trim().to_string());

    fields
}

fn fields_after_prefix(line: &str, prefix: &str) -> Option<Vec<String>> {
    line.strip_prefix(prefix).map(|body| split_at_fields(body.trim()))
}

/// 判断 APN 是否为可用的数据 APN（跳过 ims 等信令 APN）
pub fn is_valid_data_apn(apn: &str) -> bool {
    !apn.is_empty()
        && !apn.contains("placeholder")
        && !apn.starts_with("apn")
        && !apn.eq_ignore_ascii_case("ims")
}

/// Represents a single parsed line from an AT response
#[derive(Debug, Clone)]
pub enum ParsedLine {
    Cpin(CpinResponse),
    Quimslot(QuimslotResponse),
    Qspn(QspnResponse),
    Cops(CopsResponse),
    Cgdcont(CgdcontEntry),
    TrafficStats(TrafficStats),
    Cgpaddr(CgpaddrEntry),
    QengServingCell(QengServingCell),
    QengNeighbourCell(QengNeighbourCell),
    Qcainfo(QcainfoEntry),
    Cnum(CnumResponse),
    Qmbncfg(QmbncfgEntry),
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

/// Parse a single line of AT response using prefix matching + AT CSV field split.
pub fn parse_single_line(line: &str) -> Option<ParsedLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "OK" {
        return Some(ParsedLine::Ok);
    }
    if trimmed == "ERROR" || trimmed.starts_with("+CME ERROR:") || trimmed.starts_with("+CMS ERROR:") {
        return Some(ParsedLine::Error);
    }

    if let Some(values) = fields_after_prefix(trimmed, "+CPIN:") {
        return Some(ParsedLine::Cpin(CpinResponse { status: values.into_iter().next().unwrap_or_default() }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QUIMSLOT:") {
        return Some(ParsedLine::Quimslot(QuimslotResponse { slot: values.first().and_then(|s| s.parse().ok()).unwrap_or(1) }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QSPN:") {
        let mut resp = QspnResponse::default();
        if let Some(v) = values.first() {
            let decoded = decode_hex_ucs2(v);
            resp.fnn = if decoded.is_empty() { v.clone() } else { decoded };
        }
        if let Some(v) = values.get(1) { resp.snn = v.clone(); }
        if let Some(v) = values.get(2) { resp.spn = v.clone(); }
        if let Some(v) = values.get(3) { resp.alphabet = v.clone(); }
        return Some(ParsedLine::Qspn(resp));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+COPS:") {
        return Some(ParsedLine::Cops(CopsResponse {
            mode: values.first().cloned().unwrap_or_default(),
            format: values.get(1).cloned(),
            oper: values.get(2).cloned(),
            act: values.get(3).cloned(),
        }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+CGDCONT:") {
        return Some(ParsedLine::Cgdcont(CgdcontEntry {
            cid: values.first().and_then(|s| s.parse().ok()).unwrap_or(0),
            pdp_type: values.get(1).cloned().unwrap_or_default(),
            apn: values.get(2).cloned().unwrap_or_default(),
            pdp_addr: values.get(3).cloned().unwrap_or_default(),
            d_comp: values.get(4).cloned().unwrap_or_default(),
            h_comp: values.get(5).cloned().unwrap_or_default(),
        }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QGDNRCNT:").or_else(|| fields_after_prefix(trimmed, "+QGDAT:")) {
        return Some(ParsedLine::TrafficStats(TrafficStats {
            tx_bytes: values.first().and_then(|s| s.parse().ok()).unwrap_or(0),
            rx_bytes: values.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
        }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+CGPADDR:") {
        return Some(ParsedLine::Cgpaddr(CgpaddrEntry {
            cid: values.first().and_then(|s| s.parse().ok()).unwrap_or(0),
            ipv4: values.get(1).cloned().unwrap_or_default(),
            ipv6: values.get(2).cloned().unwrap_or_default(),
        }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QENG:") {
        let tag = values.first().map(String::as_str).unwrap_or_default();
        let get = |i: usize| values.get(i).cloned().unwrap_or_default();

        if tag == "servingcell" {
            let mut cell = QengServingCell::default();
            // RAT 决定字段布局：LTE 无 TAC 且带宽分 UL/DL 两段
            let is_nr = get(2).contains("NR5G");

            cell.connection_status = get(1);
            cell.rat = get(2);
            cell.opmode = get(3);
            cell.mcc = get(4);
            cell.mnc = get(5);
            cell.cell_id = get(6);
            cell.pci = get(7);

            if is_nr {
                // NR5G: pci,tac,arfcn,band,bw,rsrp,rsrq,sinr,srxlev,rssi
                cell.tac = get(8);
                cell.earfcn = get(9);
                cell.band = get(10);
                cell.bandwidth = get(11);
                cell.rsrp = get(12);
                cell.rsrq = get(13);
                cell.sinr = get(14);
                cell.srxlev = get(15);
                cell.rssi = get(16);
            } else {
                // LTE: pci,earfcn,band,ul_bw,dl_bw,tac,rsrp,rsrq,rssi,sinr（带宽取 DL 段）
                cell.earfcn = get(8);
                cell.band = get(9);
                cell.bandwidth = get(11);
                cell.tac = get(12);
                cell.rsrp = get(13);
                cell.rsrq = get(14);
                cell.rssi = get(15);
                cell.sinr = get(16);
            }
            return Some(ParsedLine::QengServingCell(cell));
        }

        // 5G NSA (EN-DC) 下模组会额外返回两行 servingcell 数据，二者都以 RAT 作为
        // 首个字段、没有 "servingcell" 前缀，字段布局也与 SA 不同，需单独解析：
        //   +QENG: "LTE",<is_tdd>,<mcc>,<mnc>,<cellid>,<pci>,<earfcn>,<band>,<ul_bw>,<dl_bw>,<tac>,<rsrp>,<rsrq>,<rssi>,<sinr>,<srxlev>
        //   +QENG: "NR5G-NSA",<mcc>,<mnc>,<pci>,<rsrp>,<sinr>,<rsrq>,<arfcn>,<band>,<bw>
        if tag == "LTE" {
            return Some(ParsedLine::QengServingCell(QengServingCell {
                rat: "LTE".to_string(),
                opmode: get(1),
                mcc: get(2),
                mnc: get(3),
                cell_id: get(4),
                pci: get(5),
                earfcn: get(6),
                band: get(7),
                bandwidth: get(9),
                tac: get(10),
                rsrp: get(11),
                rsrq: get(12),
                rssi: get(13),
                sinr: get(14),
                srxlev: get(15),
                ..Default::default()
            }));
        }
        if tag == "NR5G-NSA" {
            return Some(ParsedLine::QengServingCell(QengServingCell {
                rat: "NR5G-NSA".to_string(),
                mcc: get(1),
                mnc: get(2),
                pci: get(3),
                rsrp: get(4),
                sinr: get(5),
                rsrq: get(6),
                earfcn: get(7),
                band: get(8),
                bandwidth: get(9),
                ..Default::default()
            }));
        }

        if tag == "neighbourcell" {
            return Some(ParsedLine::QengNeighbourCell(QengNeighbourCell {
                rat: get(1),
                mcc: get(2),
                mnc: get(3),
                pci: get(4),
                earfcn: get(5),
                rsrp: get(6),
                rsrq: get(7),
                sinr: get(8),
                srxlev: get(9),
            }));
        }
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QCAINFO:") {
        let mut entry = QcainfoEntry::default();
        entry.component = values.first().cloned().unwrap_or_default();
        entry.earfcn = values.get(1).cloned().unwrap_or_default();
        entry.bandwidth = values.get(2).cloned().unwrap_or_default();
        entry.band = values.get(3).cloned().unwrap_or_default();
        if entry.component == "SCC" {
            entry.scc_idx = values.get(4).cloned();
            entry.pci = values.get(5).cloned().unwrap_or_default();
            entry.rsrp = values.get(6).cloned();
            entry.rsrq = values.get(7).cloned();
            entry.sinr = values.get(8).cloned();
        } else {
            entry.pci = values.get(4).cloned().unwrap_or_default();
        }
        return Some(ParsedLine::Qcainfo(entry));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+CNUM:") {
        return Some(ParsedLine::Cnum(CnumResponse { number: values.get(1).cloned().unwrap_or_default() }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QCCID:") {
        return Some(ParsedLine::Qccid(values.into_iter().next().unwrap_or_default()));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+CIMI:") {
        return Some(ParsedLine::Cimi(values.into_iter().next().unwrap_or_default()));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QTEMP:") {
        return Some(ParsedLine::Qtemp(QtempResponse {
            sensor: values.first().cloned().unwrap_or_default(),
            temperature: values.get(1).and_then(|v| v.parse::<f64>().ok()),
        }));
    }
    if let Some(values) = fields_after_prefix(trimmed, "+QMBNCFG:") {
        if values.first().map(String::as_str) == Some("List") {
            // 兼容多种 List 行格式（不同固件字段数量/顺序不同）：
            //   A) +QMBNCFG: "List",<index>,<state>,"<name>"
            //   B) +QMBNCFG: "List",<index>,"<name>",<state>
            //   C) +QMBNCFG: "List",<index>,<state>,<status>,"<name>",<version>,<date>（实测 RM520N）
            // 判定规则：
            //   - 字段数≥5 且第 3 字段（state 之后）仍为纯数字 → 格式 C，name 在第 4 字段；
            //   - 否则第 2 字段为纯数字 → 格式 A（name 在末位）；否则 → 格式 B。
            if values.len() >= 5
                && values.get(3).map(|v| v.parse::<u32>().is_ok()).unwrap_or(false)
            {
                let name = values.get(4).cloned().unwrap_or_default();
                if !name.is_empty() {
                    let index = values.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let state = values.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    return Some(ParsedLine::Qmbncfg(QmbncfgEntry { index, state, name }));
                }
                return None;
            }
            if values.get(2).map(|v| v.parse::<u32>().is_ok()).unwrap_or(false) {
                let name = values.get(3).cloned().unwrap_or_default();
                if !name.is_empty() {
                    let index = values.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let state = values.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    return Some(ParsedLine::Qmbncfg(QmbncfgEntry { index, state, name }));
                }
                return None;
            } else {
                let name = values.get(2).cloned().unwrap_or_default();
                if !name.is_empty() {
                    let index = values.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let state = values.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                    return Some(ParsedLine::Qmbncfg(QmbncfgEntry { index, state, name }));
                }
                return None;
            }
        }
    }

    Some(ParsedLine::Other(trimmed.to_string()))
}

/// Write accumulated carrier aggregation info (bands, bandwidth, earfcn, pci) to telemetry.
fn set_carrier_telemetry(
    bands: &[String],
    bw_parts: &[String],
    total_bw: f64,
    earfcns: &[String],
    pcis: &[String],
    is_nr: bool,
    telemetry: &mut crate::actor::telemetry::TelemetryData,
) {
    if !bands.is_empty() {
        telemetry.bands = Some(bands.join(", "));
    }
    if !bw_parts.is_empty() {
        let prefix = if is_nr { "NR " } else { "" };
        telemetry.bandwidth = if bw_parts.len() > 1 {
            Some(format!("{}{} MHz ({})", prefix, total_bw, bw_parts.join("+")))
        } else {
            Some(format!("{}{} MHz", prefix, total_bw))
        };
    }
    if !earfcns.is_empty() {
        telemetry.earfcn = Some(earfcns.join(", "));
    }
    if !pcis.is_empty() {
        telemetry.pci = Some(pcis.join(", "));
    }
}

/// Parse +QENG "servingcell" response string and populate TelemetryData
pub fn parse_qeng(qeng_res: &str, telemetry: &mut crate::actor::telemetry::TelemetryData) {
    if qeng_res.is_empty() {
        println!("未找到 +QENG 响应");
        return;
    }

    // Collect all servingcell lines (carrier aggregation can have multiple)。
    // 无 RAT 的行（如 NSA 的 `+QENG: "servingcell","NOCONN"` 状态行）不含任何小区
    // 数据，必须剔除，否则会被当成主小区把其余字段全部覆盖成空值。
    let serving_cells: Vec<QengServingCell> = qeng_res
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            match parse_single_line(trimmed) {
                Some(ParsedLine::QengServingCell(cell)) if !cell.rat.is_empty() => Some(cell),
                _ => None,
            }
        })
        .collect();

    if serving_cells.is_empty() {
        println!("未找到 +QENG servingcell 数据");
        return;
    }

    // Use first line (PCC/main carrier) for base fields
    let pcc = &serving_cells[0];

    // 5G NSA (EN-DC) 下 PCC 是 LTE 锚点，若照搬 pcc.rat 会显示成 "LTE FDD"，
    // 用户会误以为自己没连上 5G，因此检测到 NR 从载波时显式标注组网形态。
    let has_nsa_carrier = serving_cells.iter().any(|c| c.rat == "NR5G-NSA");
    telemetry.network_mode = Some(if has_nsa_carrier && pcc.rat == "LTE" {
        "NR5G-NSA (LTE 锚点)".to_string()
    } else {
        format!("{} {}", pcc.rat, pcc.opmode)
    });
    telemetry.mccmnc = Some(format!("{}{}", pcc.mcc, pcc.mnc));
    telemetry.cell_id = Some(pcc.cell_id.clone());
    // 3GPP 规范：LTE ECI 为 28-bit（最多 7 位 Hex，末 2 位是 8-bit Sector ID），
    // NR NCI 为 36-bit（最多 9 位 Hex，末 3 位是 12-bit Sector ID），
    // 因此基站编号的截断长度必须随制式变化 —— 写死 -3 会算错 LTE 的 eNB ID。
    // cell_id 来自模组 AT 响应，通常为 ASCII 十六进制字符串；
    // 先整体判断 is_ascii() 再切片，避免多字节 UTF-8 乱码导致切片边界 panic
    let sector_hex_len = if pcc.rat.contains("NR5G") { 3 } else { 2 };
    telemetry.enb_id = if pcc.cell_id.is_empty() {
        None
    } else if pcc.cell_id.is_ascii() && pcc.cell_id.len() > sector_hex_len {
        Some(pcc.cell_id[..pcc.cell_id.len() - sector_hex_len].to_string())
    } else {
        Some(pcc.cell_id.clone())
    };
    telemetry.tac = if pcc.tac.is_empty() {
        None
    } else {
        Some(pcc.tac.clone())
    };

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

    // Collect carrier aggregation info
    let mut bands: Vec<String> = Vec::new();
    let mut earfcns = Vec::new();
    let mut pcis = Vec::new();
    let mut total_bw = 0.0f64;
    let mut bw_parts: Vec<String> = Vec::new();

    for cell in &serving_cells {
        let is_nr = cell.rat.contains("NR5G");
        let band = if is_nr {
            format!("NR5G BAND {}", cell.band)
        } else {
            format!("LTE BAND {}", cell.band)
        };
        if !bands.contains(&band) {
            bands.push(band);
        }
        if let Ok(code) = cell.bandwidth.parse::<i32>() {
            let actual_bw = if is_nr { decode_nr_bandwidth(code) } else { decode_lte_bandwidth(code) };
            total_bw += actual_bw;
            bw_parts.push(actual_bw.to_string());
        }
        earfcns.push(cell.earfcn.clone());
        pcis.push(cell.pci.clone());
    }

    // 带宽前缀由主载波（PCC）制式决定，不可写死为 NR
    let is_nr_pcc = pcc.rat.contains("NR5G");
    set_carrier_telemetry(&bands, &bw_parts, total_bw, &earfcns, &pcis, is_nr_pcc, telemetry);
}

/// Parse +QCAINFO response and populate TelemetryData
pub fn parse_qcainfo(qca_res: &str, telemetry: &mut crate::actor::telemetry::TelemetryData) {
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
            let actual_bw = if is_nr { decode_nr_bandwidth(code) } else { decode_lte_bandwidth(code) };
            total_bw += actual_bw;
            bw_parts.push(actual_bw.to_string());
        }
        earfcns.push(entry.earfcn.clone());
        pcis.push(entry.pci.clone());

        let band_label = if entry.band.starts_with("NR5G") || entry.band.starts_with("LTE") {
            entry.band.clone()
        } else {
            format!("LTE BAND {}", entry.band)
        };
        if !bands.contains(&band_label) {
            bands.push(band_label);
        }
    }

    let is_nr_agg = entries.iter().any(|e| e.band.starts_with("NR5G"));
    set_carrier_telemetry(&bands, &bw_parts, total_bw, &earfcns, &pcis, is_nr_agg, telemetry);

    crate::logger::push_log("INFO", "QCAINFO", &format!(
        "QCAINFO parsed: bands={:?} bw={:?} earfcn={:?} pci={:?}",
        telemetry.bands, telemetry.bandwidth, telemetry.earfcn, telemetry.pci
    ));
}

/// Parse +CGPADDR response and populate TelemetryData
pub fn parse_cgpaddr(gpad_res: &str, telemetry: &mut crate::actor::telemetry::TelemetryData) {
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
    for line in creg_raw.lines() {
        if let Some(values) = fields_after_prefix(line.trim(), "+CREG:") {
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
    for line in csq_raw.lines() {
        if let Some(values) = fields_after_prefix(line.trim(), "+CSQ:") {
            if let Some(rssi_str) = values.first() {
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
                                // 3GPP TS 27.007 <AcT> 定义：
                                // 0/1 GSM, 2 UTRAN, 3 GSM w/ EGPRS,
                                // 4/5/6 UTRAN w/ HSDPA/HSUPA, 7 E-UTRAN(LTE),
                                // 8 EC-GSM-IoT(eMTC), 9 NB-IoT,
                                // 10/11/12/13 5GC 相关(NR5G / EN-DC)
                                let tech = match parts[4].trim() {
                                    "0" | "1" | "3" => "GSM",
                                    "2" | "4" | "5" | "6" => "WCDMA",
                                    "7" => "LTE",
                                    "8" => "eMTC",
                                    "9" => "NB-IoT",
                                    "10" | "11" | "12" | "13" => "NR5G",
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

/// Parse +QENG: "neighbourcell" response and return a list of neighbour cells
pub fn parse_qeng_neighbour(raw: &str) -> Vec<QengNeighbourCell> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            match parse_single_line(trimmed) {
                Some(ParsedLine::QengNeighbourCell(cell)) => Some(cell),
                _ => None,
            }
        })
        .collect()
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

    #[test]
    fn test_parse_qmbncfg_list() {
        let result = parse_single_line("+QMBNCFG: \"List\",0,1,\"RM520NGLAAR01A02M4G_01.004\"");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 0 && r.state == 1 && r.name == "RM520NGLAAR01A02M4G_01.004")
        );
    }

    #[test]
    fn test_parse_qmbncfg_ignores_non_list() {
        let result = parse_single_line("+QMBNCFG: \"AutoSel\",0");
        assert!(matches!(result, Some(ParsedLine::Other(_))));
    }

    #[test]
    fn test_parse_qmbncfg_ignores_empty_name() {
        let result = parse_single_line("+QMBNCFG: \"List\",0,0,\"\"");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_qmbncfg_malformed_numeric() {
        let result = parse_single_line("+QMBNCFG: \"List\",X,1,\"RM520NGLAAR01A02M4G_01.004\"");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 0 && r.state == 1 && r.name == "RM520NGLAAR01A02M4G_01.004")
        );
    }

    #[test]
    fn test_parse_qmbncfg_list_format_b() {
        // 部分固件（如 RM502Q-AE）返回 "List",<index>,"<name>",<state> 顺序
        let result = parse_single_line("+QMBNCFG: \"List\",4,\"VoLTE_OPNMKT_CT\",1");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 4 && r.state == 1 && r.name == "VoLTE_OPNMKT_CT")
        );
    }

    #[test]
    fn test_parse_qmbncfg_list_format_a_numeric_name() {
        // 格式 A 下 name 为纯数字时不应被误当成 state
        let result = parse_single_line("+QMBNCFG: \"List\",0,1,\"12345\"");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 0 && r.state == 1 && r.name == "12345")
        );
    }

    #[test]
    fn test_parse_qmbncfg_list_format_c_with_extra_fields() {
        // 实测 RM520N 固件：+QMBNCFG: "List",<index>,<state>,<status>,"<name>",<version>,<date>
        let result = parse_single_line("+QMBNCFG: \"List\",0,1,1,\"VoLTE_OPNMKT_CT\",0x0A0113E0,202204211");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 0 && r.state == 1 && r.name == "VoLTE_OPNMKT_CT")
        );
    }

    #[test]
    fn test_parse_qmbncfg_list_format_c_short() {
        // 格式 C 无版本/日期尾字段
        let result = parse_single_line("+QMBNCFG: \"List\",3,0,0,\"CZO2_Commercial\"");
        assert!(
            matches!(result, Some(ParsedLine::Qmbncfg(ref r))
                if r.index == 3 && r.state == 0 && r.name == "CZO2_Commercial")
        );
    }

    #[test]
    fn test_parse_qmbncfg_list_format_c_multiple_entries() {
        // 真实多条混合：非激活项 name 不应再被误读为 state
        let lines = "+QMBNCFG: \"List\",1,0,0,\"Volte_OpenMkt-Commercial-CMCC\",0x0A012010,202212151\n\
                     +QMBNCFG: \"List\",2,0,0,\"VoLTE-CU\",0x0A011561,202204211";
        let mut names = Vec::new();
        for line in lines.lines() {
            if let Some(ParsedLine::Qmbncfg(e)) = parse_single_line(line) {
                names.push(e.name);
            }
        }
        assert_eq!(names, vec!["Volte_OpenMkt-Commercial-CMCC", "VoLTE-CU"]);
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
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_cgpaddr(raw, &mut telemetry);
        assert_eq!(telemetry.ipv4, Some("10.172.99.214".to_string()));
        assert_eq!(telemetry.ipv6, Some("2409:8970:b68:1d9c:18be:3323:1cfc:5996".to_string()));
    }

    #[test]
    fn test_parse_cgpaddr_all_zero() {
        // 所有 CID 均为 0.0.0.0，应回退为 "--"
        let raw = "+CGPADDR: 1,\"0.0.0.0\",\"0.0.0.0\"\n+CGPADDR: 2,\"0.0.0.0\"";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_cgpaddr(raw, &mut telemetry);
        assert!(telemetry.ipv4.is_none());
        assert!(telemetry.ipv6.is_none());
    }

    #[test]
    fn test_parse_qeng_real_nr5g_sa() {
        // 真实 NR5G-SA 服务小区数据：NOCONN, TDD, 46000, cell=39074C001
        let raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.network_mode, Some("NR5G-SA TDD".to_string()));
        assert_eq!(telemetry.mccmnc, Some("46000".to_string()));
        assert_eq!(telemetry.cell_id, Some("39074C001".to_string()));
        assert_eq!(telemetry.enb_id, Some("39074C".to_string()));
        assert_eq!(telemetry.tac, Some("72002F".to_string()));
        assert_eq!(telemetry.bands, Some("NR5G BAND 41".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 100 MHz".to_string()));
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
    fn test_parse_qeng_non_ascii_cell_id_no_panic() {
        // 固件异常输出非 ASCII 乱码 cell_id（"abéé" 含 2 字节 UTF-8 字符），
        // 旧代码 `cell_id[..len-3]` 切片边界 len-3=3 会落在 é(字节 2-3) 中间导致 panic。
        let raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,abéé,751,72002F,504990,41,12,-65,-11,19,1,-";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);
        // 不应 panic，非 ASCII 时 enb_id 回退为完整 cell_id
        assert_eq!(telemetry.cell_id, Some("abéé".to_string()));
        assert_eq!(telemetry.enb_id, Some("abéé".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_real_pcc_scc() {
        // 真实 CA 数据：PCC(NR5G BAND 41, 12=100MHz) + SCC(NR5G BAND 28, 3=20MHz)
        let raw =
            "+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\n\
             +QCAINFO: \"SCC\",156490,3,\"NR5G BAND 28\",1,250,0,-,-";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
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
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("NR5G BAND 41".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 100 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990".to_string()));
        assert_eq!(telemetry.pci, Some("751".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_empty() {
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
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

        let mut telemetry = crate::actor::telemetry::TelemetryData::default();

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
        assert_eq!(telemetry.bandwidth, Some("NR 100 MHz".to_string()));
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
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("NR5G BAND 41, NR5G BAND 28".to_string()));
        assert_eq!(telemetry.bandwidth, Some("NR 120 MHz (100+20)".to_string()));
        assert_eq!(telemetry.earfcn, Some("504990, 156490".to_string()));
        assert_eq!(telemetry.pci, Some("751, 250".to_string()));
        // 使用 PCC（首行）的信号值
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
    }

    #[test]
    fn test_parse_qeng_neighbour_lte_and_nr() {
        // 混合 LTE + NR5G 邻区真实场景
        let raw =
            "+QENG: \"neighbourcell\",\"LTE\",460,01,123,6300,-95,-8,-9,27\n\
             +QENG: \"neighbourcell\",\"LTE\",460,01,124,6299,-102,-10,-11,25\n\
             +QENG: \"neighbourcell\",\"NR5G\",460,01,456,500000,-90,-6,-7,28\n\
             OK\r\n";
        let cells = parse_qeng_neighbour(raw);
        assert_eq!(cells.len(), 3);

        // LTE 邻区 1
        assert_eq!(cells[0].rat, "LTE");
        assert_eq!(cells[0].mcc, "460");
        assert_eq!(cells[0].mnc, "01");
        assert_eq!(cells[0].pci, "123");
        assert_eq!(cells[0].earfcn, "6300");
        assert_eq!(cells[0].rsrp, "-95");
        assert_eq!(cells[0].rsrq, "-8");
        assert_eq!(cells[0].sinr, "-9");
        assert_eq!(cells[0].srxlev, "27");

        // NR5G 邻区
        assert_eq!(cells[2].rat, "NR5G");
        assert_eq!(cells[2].pci, "456");
        assert_eq!(cells[2].earfcn, "500000");
        assert_eq!(cells[2].rsrp, "-90");
    }

    #[test]
    fn test_parse_qeng_neighbour_empty() {
        let cells = parse_qeng_neighbour("");
        assert!(cells.is_empty());
    }

    #[test]
    fn test_parse_qeng_neighbour_no_match() {
        // servingcell 行不应被误解析为 neighbourcell
        let raw = "+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,19,1,-";
        let cells = parse_qeng_neighbour(raw);
        assert!(cells.is_empty());
    }

    #[test]
    fn test_parse_qeng_lte_servingcell() {
        // 移远 LTE servingcell 标准布局（含 TAC，频宽分 UL/DL 两段）
        // "servingcell",state,LTE,FDD,mcc,mnc,cellid,pci,earfcn,band,ul_bw,dl_bw,tac,rsrp,rsrq,rssi,sinr
        let raw = "+QENG: \"servingcell\",\"CONNECT\",\"LTE\",\"FDD\",460,00,39074C001,751,6300,3,20,20,DE10,-65,-11,-70,19";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.network_mode, Some("LTE FDD".to_string()));
        assert_eq!(telemetry.bands, Some("LTE BAND 3".to_string()));
        // 无 NR 前缀
        assert_eq!(telemetry.bandwidth, Some("20 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("6300".to_string()));
        assert_eq!(telemetry.pci, Some("751".to_string()));
        // 下标 12 为 TAC
        assert_eq!(telemetry.tac, Some("DE10".to_string()));
        // 下表 13/14/15/16 分别为 RSRP/RSRQ/RSSI/SINR
        assert_eq!(telemetry.ss_rsrp, Some("-65 / 78%".to_string()));
        assert_eq!(telemetry.ss_rsrq, Some("-11 / 52%".to_string()));
        assert_eq!(telemetry.sinr, Some("19 / 78%".to_string()));
        // 不得把十六进制 TAC 当作 RSRP 导致信号跌为 0%
        assert_eq!(telemetry.signal_percentage, Some("78%".to_string()));
    }

    #[test]
    fn test_parse_qeng_lte_enb_id_is_20bit() {
        // LTE ECI 为 28-bit / 7 位 Hex，前 5 位是 eNB ID（此处 5F1EA）。
        // 旧实现统一砍掉末尾 3 位会得到 "5F1E"，基站编号整体算错。
        let raw = "+QENG: \"servingcell\",\"CONNECT\",\"LTE\",\"FDD\",460,01,5F1EA15,12,1650,3,20,20,DE10,-65,-11,-70,19";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert_eq!(telemetry.cell_id, Some("5F1EA15".to_string()));
        assert_eq!(telemetry.enb_id, Some("5F1EA".to_string()));
    }

    #[test]
    fn test_parse_qeng_nsa_en_dc_three_lines() {
        // 真实 EN-DC 输出：状态行 + LTE 锚点行 + NR5G-NSA 从载波行。
        // 旧实现只认首字段为 "servingcell" 的行，后两行全部被丢弃，
        // NSA 下界面所有信号/频段/小区指标都会变成 --。
        let raw = "+QENG: \"servingcell\",\"NOCONN\"\n\
                   +QENG: \"LTE\",\"FDD\",460,01,5F1EA15,12,1650,3,20,20,DE10,-99,-12,-67,11,9\n\
                   +QENG: \"NR5G-NSA\",460,01,747,-71,13,-11,627264,78,12,1";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        // 主小区仍是 LTE 锚点，但组网形态必须标注为 NSA
        assert_eq!(telemetry.network_mode, Some("NR5G-NSA (LTE 锚点)".to_string()));
        assert_eq!(telemetry.mccmnc, Some("46001".to_string()));
        assert_eq!(telemetry.cell_id, Some("5F1EA15".to_string()));
        assert_eq!(telemetry.enb_id, Some("5F1EA".to_string()));
        assert_eq!(telemetry.tac, Some("DE10".to_string()));

        // 信号取 LTE 锚点：rsrp=-99 → 42%，rsrq=-12 → 47%，sinr=11 → 62%
        assert_eq!(telemetry.signal_percentage, Some("42%".to_string()));
        assert_eq!(telemetry.ss_rsrp, Some("-99 / 42%".to_string()));
        assert_eq!(telemetry.ss_rsrq, Some("-12 / 47%".to_string()));
        assert_eq!(telemetry.sinr, Some("11 / 62%".to_string()));

        // 锚点与从载波都进聚合列表：LTE B3 20MHz + NR n78 100MHz
        assert_eq!(telemetry.bands, Some("LTE BAND 3, NR5G BAND 78".to_string()));
        assert_eq!(telemetry.bandwidth, Some("120 MHz (20+100)".to_string()));
        assert_eq!(telemetry.earfcn, Some("1650, 627264".to_string()));
        assert_eq!(telemetry.pci, Some("12, 747".to_string()));
    }

    #[test]
    fn test_parse_qeng_state_only_line_leaves_telemetry_untouched() {
        // 脱网时模组只回状态行，不含 RAT/小区数据，不得用它覆盖出空值字段
        let raw = "+QENG: \"servingcell\",\"SEARCH\"";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qeng(raw, &mut telemetry);

        assert!(telemetry.network_mode.is_none());
        assert!(telemetry.cell_id.is_none());
        assert!(telemetry.enb_id.is_none());
        assert!(telemetry.signal_percentage.is_none());
        assert!(telemetry.bands.is_none());
    }

    #[test]
    fn test_parse_cops_scan_act_mapping() {
        // 真实扫频：LTE 基站 AcT=7，NR5G 基站 AcT=11/12/13，2G/3G 为 0/2
        let raw = "+COPS: (2,\"CHN-UNICOM\",\"UNICOM\",\"46001\",7),\
                   (1,\"CHN-MOBILE\",\"CMCC\",\"46000\",7),\
                   (1,\"CHN-UNICOM\",\"UNICOM\",\"46001\",11),\
                   (3,\"CHN-MOBILE\",\"CMCC\",\"46000\",12),\
                   (1,\"CMCC\",\"CMCC\",\"46000\",2),\
                   (3,\"CMCC\",\"CMCC\",\"46000\",0)";
        let nets = parse_cops_scan(raw);
        assert_eq!(nets.len(), 6);
        assert_eq!(nets[0]["technology"], "LTE");
        assert_eq!(nets[1]["technology"], "LTE");
        assert_eq!(nets[2]["technology"], "NR5G");
        assert_eq!(nets[3]["technology"], "NR5G");
        assert_eq!(nets[4]["technology"], "WCDMA");
        assert_eq!(nets[5]["technology"], "GSM");
    }

    #[test]
    fn test_parse_qcainfo_lte_band_label() {
        // LTE 下 QCAINFO 的 band 字段为裸数字，不应被拼成 "NR5G BAND 3"
        let raw = "+QCAINFO: \"PCC\",6300,100,\"3\",751";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("LTE BAND 3".to_string()));
        assert_eq!(telemetry.bandwidth, Some("20 MHz".to_string()));
        assert_eq!(telemetry.earfcn, Some("6300".to_string()));
    }

    #[test]
    fn test_parse_qcainfo_lte_prefixed_band_not_doubled() {
        // 部分固件已带 "LTE BAND " 前缀，不可再叠加
        let raw = "+QCAINFO: \"PCC\",6300,100,\"LTE BAND 3\",751";
        let mut telemetry = crate::actor::telemetry::TelemetryData::default();
        parse_qcainfo(raw, &mut telemetry);

        assert_eq!(telemetry.bands, Some("LTE BAND 3".to_string()));
    }
}

