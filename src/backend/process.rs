//! `atcmd_rs` 子进程 IO 驱动与 AT 响应通用处理（零锁 — Actor 串行化保证互斥）。

use std::time::Duration;

use serde::Serialize;

use crate::at::{decode_hex_ucs2, parser::ParsedLine};
use crate::logger::push_log;

pub fn spawn_atcmd_rs(
    serial_path: &str,
    cmd: &str,
    sms_message: Option<&str>,
    hex_body: Option<&str>,
) -> Result<tokio::process::Child, String> {
    let mut command = tokio::process::Command::new("atcmd_rs");
    command.arg("-p").arg(serial_path);
    if let Some(message) = sms_message {
        command.arg("--message").arg(message);
    }
    if let Some(hex) = hex_body {
        command.arg("--hex-body").arg(hex);
    }
    command
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("atcmd_rs启动失败: {}", e))
}

/// IMEI (AT+EGMR) 响应解析结果
#[derive(Debug, PartialEq, Serialize)]
pub struct ImeiParseResult {
    pub kind: String,
    pub success: bool,
    pub imei: Option<String>,
}

/// 从 AT+EGMR 原始响应中提取 IMEI 数字串（如 `+EGMR: "866355057849136"`）
pub fn extract_egmr_imei(raw: &str) -> Option<String> {
    let marker = "+EGMR:";
    let pos = raw.find(marker)?;
    let rest = raw[pos + marker.len()..]
        .trim_start()
        .trim_start_matches('"');
    let imei: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if imei.is_empty() { None } else { Some(imei) }
}

/// 解析 AT+EGMR 响应（读取/写入 IMEI），返回 None 表示非 EGMR 响应
pub fn parse_egmr_response(raw: &str) -> Option<ImeiParseResult> {
    // 1. 优先尝试提取 IMEI：只要包含 +EGMR: 响应头，无论是否有命令回显均判定为读取成功
    if let Some(imei) = extract_egmr_imei(raw) {
        return Some(ImeiParseResult {
            kind: "read".to_string(),
            success: true,
            imei: Some(imei),
        });
    }

    // 2. 带有 AT+EGMR 命令回显的场景
    if raw.contains("AT+EGMR") {
        let is_write = raw.contains("AT+EGMR=1,7");
        let kind = if is_write { "write" } else { "read" };
        let has_error = at_exec_failed(raw);
        let success = !has_error && raw.contains("OK");
        return Some(ImeiParseResult {
            kind: kind.to_string(),
            success,
            imei: None,
        });
    }

    None
}

pub fn at_response_has_error(raw: &str) -> bool {
    raw.lines()
        .any(|line| matches!(crate::at::parser::parse_single_line(line), Some(ParsedLine::Error)))
}

/// 判断 `exec_raw_at` 的返回值是否表示命令执行失败。
/// `exec_raw_at` 成功时返回原始 AT 响应（不会以 ERROR 开头）；
/// 失败时统一包装为 `ERROR: ...` 前缀。再叠加原始响应中的 ERROR 行兜底检测。
pub fn at_exec_failed(raw: &str) -> bool {
    raw.trim_start().starts_with("ERROR") || at_response_has_error(raw)
}

pub fn at_response_preview(raw: &str) -> &str {
    raw.lines()
        .find(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !matches!(
                    crate::at::parser::parse_single_line(trimmed),
                    Some(ParsedLine::Ok | ParsedLine::Error)
                )
                && !trimmed.starts_with("AT+")
        })
        .unwrap_or(raw)
}

pub async fn send_at_command_inner_with_options(
    serial_path: &str,
    cmd: &str,
    timeout: Duration,
    sms_message: Option<&str>,
    hex_body: Option<&str>,
) -> Result<String, String> {
    let start = std::time::Instant::now();

    let child = spawn_atcmd_rs(serial_path, cmd, sms_message, hex_body)?;

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("atcmd_rs执行失败: {}", e)),
        Err(_) => return Err(format!("AT命令超时({}s): {}", timeout.as_secs(), cmd)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "atcmd_rs失败 (exit={}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();

    if at_response_has_error(&raw) {
        let elapsed = start.elapsed();
        push_log(
            "WARN",
            "AT",
            &format!("[AT] {} 返回 ERROR ({}ms)", cmd, elapsed.as_millis()),
        );
        return Err(format!("AT命令返回错误: {}", raw.trim()));
    }

    let elapsed = start.elapsed();
    let preview = at_response_preview(&raw);
    push_log(
        "INFO",
        "AT",
        &format!(
            "{} 成功 ({}ms): {}",
            cmd,
            elapsed.as_millis(),
            preview.trim()
        ),
    );
    Ok(raw)
}

pub async fn send_at_command_inner_with_timeout(
    serial_path: &str,
    cmd: &str,
    timeout: Duration,
) -> Result<String, String> {
    send_at_command_inner_with_options(serial_path, cmd, timeout, None, None).await
}

pub async fn send_at_command_inner(serial_path: &str, cmd: &str) -> Result<String, String> {
    send_at_command_inner_with_timeout(serial_path, cmd, Duration::from_secs(10)).await
}

pub async fn send_sms_command_inner(
    serial_path: &str,
    cmd: &str,
    message: &str,
    hex_body: Option<&str>,
) -> Result<String, String> {
    // 当使用 hex_body（UCS2 原始字节）时，不传递 sms_message，
    // 避免 atcmd_rs 的 --message 抢占 `>` prompt 导致 hex_body 被跳过
    let sms_message = if hex_body.is_some() {
        None
    } else {
        Some(message)
    };
    send_at_command_inner_with_options(
        serial_path,
        cmd,
        Duration::from_secs(30),
        sms_message,
        hex_body,
    )
    .await
}

/// 三段式回退获取运营商名称：QSPN → COPS → QENG MCC/MNC
pub async fn fetch_network_provider(serial_path: &str) -> String {
    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QSPN").await {
        if let Some(provider) = resp
            .lines()
            .find_map(|l| match crate::at::parser::parse_single_line(l) {
                Some(ParsedLine::Qspn(qspn)) if !qspn.fnn.is_empty() && qspn.fnn != "????" => {
                    let decoded = decode_hex_ucs2(&qspn.fnn);
                    Some(if decoded.is_empty() { qspn.fnn } else { decoded })
                }
                _ => None,
            })
        {
            return provider;
        }
    }

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+COPS?").await {
        if let Some(provider) = resp
            .lines()
            .find_map(|l| match crate::at::parser::parse_single_line(l) {
                Some(ParsedLine::Cops(cops)) => {
                    if let Some(oper) = cops.oper {
                        let trimmed = oper.trim();
                        if !trimmed.is_empty() && trimmed != "????" {
                            let decoded = decode_hex_ucs2(trimmed);
                            return Some(if decoded.is_empty() {
                                trimmed.to_string()
                            } else {
                                decoded
                            });
                        }
                    }
                    None
                }
                _ => None,
            })
        {
            return provider;
        }
    }

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QENG=\"servingcell\"").await {
        if let Some(mccmnc) =
            resp.lines()
                .find_map(|l| match crate::at::parser::parse_single_line(l) {
                    Some(ParsedLine::QengServingCell(cell)) => {
                        Some(format!("{}{}", cell.mcc, cell.mnc))
                    }
                    _ => None,
                })
        {
            return match mccmnc.as_str() {
                "46000" | "46002" | "46007" => "中国移动".to_string(),
                "46001" => "中国联通".to_string(),
                "46003" | "46011" => "中国电信".to_string(),
                _ => "Unknown".to_string(),
            };
        }
    }

    "Unknown".to_string()
}

/// 将 QCAINFO 的 band 字段分类为 (NR 频段号, LTE 频段号)。
/// 兼容三种固件输出：`"NR5G BAND 41"`、`"LTE BAND 3"`、裸数字（>100 视为 NR）。
fn classify_qcainfo_band(band: &str) -> (Option<u32>, Option<u32>) {
    if let Some(num) = band
        .strip_prefix("NR5G BAND ")
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        (Some(num), None)
    } else if let Some(num) = band
        .strip_prefix("LTE BAND ")
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        (None, Some(num))
    } else if let Ok(band_num) = band.parse::<u32>() {
        if band_num > 100 {
            (Some(band_num), None)
        } else {
            (None, Some(band_num))
        }
    } else {
        (None, None)
    }
}

pub async fn query_device_bands(serial_path: &str) -> String {
    let mut nr_bands: Vec<String> = Vec::new();
    let mut lte_bands: Vec<String> = Vec::new();

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QENG=\"servingcell\"").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if let Some(ParsedLine::QengServingCell(cell)) =
                crate::at::parser::parse_single_line(trimmed)
            {
                if cell.band.is_empty() || cell.band == "-" {
                    continue;
                }
                if cell.rat.contains("NR5G") {
                    if !nr_bands.contains(&format!("n{}", cell.band)) {
                        nr_bands.push(format!("n{}", cell.band));
                    }
                } else if cell.rat == "LTE" && !lte_bands.contains(&format!("B{}", cell.band)) {
                    lte_bands.push(format!("B{}", cell.band));
                }
            }
        }
    }

    if let Ok(resp) = send_at_command_inner(serial_path, "AT+QCAINFO").await {
        for line in resp.lines() {
            let trimmed = line.trim();
            if let Some(ParsedLine::Qcainfo(entry)) =
                crate::at::parser::parse_single_line(trimmed)
            {
                let (nr, lte) = classify_qcainfo_band(&entry.band);
                if let Some(num) = nr {
                    let label = format!("n{}", num);
                    if !nr_bands.contains(&label) {
                        nr_bands.push(label);
                    }
                } else if let Some(num) = lte {
                    let label = format!("B{}", num);
                    if !lte_bands.contains(&label) {
                        lte_bands.push(label);
                    }
                }
            }
        }
    }

    nr_bands.sort_by_key(|b| b.trim_start_matches('n').parse::<u32>().unwrap_or(999));
    lte_bands.sort_by_key(|b| b.trim_start_matches('B').parse::<u32>().unwrap_or(999));

    let mut result = Vec::new();
    if !nr_bands.is_empty() {
        result.push(format!("NR {}", nr_bands.join("/")));
    }
    if !lte_bands.is_empty() {
        result.push(format!("LTE {}", lte_bands.join("/")));
    }

    if result.is_empty() {
        "--".to_string()
    } else {
        result.join(", ")
    }
}

pub async fn send_at_get_line(serial_path: &str, cmd: &str) -> Option<String> {
    send_at_command_inner(serial_path, cmd)
        .await
        .ok()
        .and_then(|resp| {
            resp.lines().find_map(|l| {
                let trimmed = l.trim();
                if !trimmed.is_empty()
                    && !trimmed.contains("OK")
                    && !trimmed.contains("ERROR")
                    && !trimmed.starts_with("AT+")
                    && !trimmed.starts_with("+CME ERROR:")
                {
                    Some(trimmed.to_string())
                } else {
                    None
                }
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_egmr_read_success() {
        let raw = "AT+EGMR=0,7\r\r\n+EGMR: \"866355057849136\"\r\n\r\nOK\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "read".to_string(),
                success: true,
                imei: Some("866355057849136".to_string()),
            })
        );
    }

    #[test]
    fn parse_egmr_read_without_echo_success() {
        // 模组 ATE0（无回显）时的真实响应
        let raw = "\r\n+EGMR: \"866355057849136\"\r\n\r\nOK\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "read".to_string(),
                success: true,
                imei: Some("866355057849136".to_string()),
            })
        );
    }

    #[test]
    fn parse_egmr_write_success() {
        let raw = "AT+EGMR=1,7,\"866355057849136\"\r\r\nOK\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "write".to_string(),
                success: true,
                imei: None,
            })
        );
    }

    #[test]
    fn parse_egmr_read_fail() {
        // exec_raw_at 失败时会包装成 "ERROR: AT命令返回错误: <raw>"
        let raw = "ERROR: AT命令返回错误: AT+EGMR=0,7\r\r\nERROR\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "read".to_string(),
                success: false,
                imei: None,
            })
        );
    }

    #[test]
    fn parse_egmr_write_fail() {
        let raw = "ERROR: AT命令返回错误: AT+EGMR=1,7,\"123\"\r\r\nERROR\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "write".to_string(),
                success: false,
                imei: None,
            })
        );
    }

    #[test]
    fn parse_egmr_read_fail_plain_error() {
        // 防御性覆盖：裸 ERROR 响应（不经过 exec_raw_at 包装）
        let raw = "AT+EGMR=0,7\r\r\nERROR\r\n";
        assert_eq!(
            parse_egmr_response(raw),
            Some(ImeiParseResult {
                kind: "read".to_string(),
                success: false,
                imei: None,
            })
        );
    }

    #[test]
    fn parse_egmr_non_egmr_returns_none() {
        let raw = "AT+CSQ\r\r\n+CSQ: 25,99\r\n\r\nOK\r\n";
        assert_eq!(parse_egmr_response(raw), None);
    }

    #[test]
    fn classify_qcainfo_band_variants() {
        assert_eq!(classify_qcainfo_band("NR5G BAND 41"), (Some(41), None));
        // 部分固件带 "LTE BAND " 前缀，不可因 parse::<u32>() 失败而丢失
        assert_eq!(classify_qcainfo_band("LTE BAND 3"), (None, Some(3)));
        // 裸数字默认按 LTE 处理（NR 输出带 "NR5G BAND " 前缀，避免 n41/B41 歧义）
        assert_eq!(classify_qcainfo_band("3"), (None, Some(3)));
        // 仅超大数值（>100，如 mmWave n257）才回退判定为 NR
        assert_eq!(classify_qcainfo_band("257"), (Some(257), None));
        assert_eq!(classify_qcainfo_band("-"), (None, None));
    }
}
