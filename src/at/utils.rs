/// Utility functions for AT response processing

/// Decode UCS2 hex-encoded string (e.g. "4E2D56FD79FB52A8" -> "中国联通")
///
/// 返回空串表示「这不是一段 UCS2 编码」，调用方应保留原文。
///
/// 注意入参常是**已经被转义过的普通短信正文**：形如 `1234`、`12345678` 的
/// 验证码同样满足「长度为 4 的倍数且全为十六进制」。若不加甄别地按 4 位一组
/// 解读，`1234` 会变成 U+1234（ሴ）之类的生僻字符。因此这里要求整串至少含有
/// 一个真正的 UCS2 特征字符：CJK/全角等宽字符，或 UCS2 下的 ASCII（高字节 0x00）。
pub fn decode_hex_ucs2(hex: &str) -> String {
    if hex.is_empty() || hex.len() % 4 != 0 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }

    // 整串由十进制数字构成 → 必然是验证码一类的普通正文。仅靠下面的字符区间
    // 判断并不够：`12345678` 的第二组 0x5678 恰好落在 CJK 区间内。
    if hex.chars().all(|c| c.is_ascii_digit()) {
        return String::new();
    }

    let mut codes = Vec::with_capacity(hex.len() / 4);
    for i in (0..hex.len()).step_by(4) {
        match u16::from_str_radix(&hex[i..i + 4], 16) {
            Ok(code) => codes.push(code),
            Err(_) => return String::new(),
        }
    }

    let has_cjk = codes.iter().any(|&c| {
        (0x4E00..=0x9FFF).contains(&c)     // CJK 统一表意文字
            || (0x3400..=0x4DBF).contains(&c) // 扩展 A
            || (0x3000..=0x303F).contains(&c) // CJK 标点
            || (0xFF00..=0xFFEF).contains(&c) // 全角/半角形式
            || (0x2000..=0x206F).contains(&c) // 常用标点
    });
    let has_ucs2_ascii = codes.iter().any(|&c| (0x0020..=0x007E).contains(&c));

    if !has_cjk && !has_ucs2_ascii {
        return String::new();
    }

    // 含 C0 控制字符的解读结果必定是误判（真短信不会有裸控制符）
    if codes
        .iter()
        .any(|&c| (0x0001..=0x0008).contains(&c) || (0x000E..=0x001F).contains(&c))
    {
        return String::new();
    }

    let mut bytes = Vec::with_capacity(codes.len() * 3);
    for code in codes {
        match code {
            0x0000..=0x007F => bytes.push(code as u8),
            0x0080..=0x07FF => {
                bytes.push(0xC0 | (code >> 6) as u8);
                bytes.push(0x80 | (code & 0x3F) as u8);
            }
            _ => {
                bytes.push(0xE0 | (code >> 12) as u8);
                bytes.push(0x80 | ((code >> 6) & 0x3F) as u8);
                bytes.push(0x80 | (code & 0x3F) as u8);
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// Format bytes into human-readable string (KB / MB / GB)
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.2} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Validate an IPv4 address (dotted decimal)
pub fn is_valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| {
        if p.is_empty() || p.len() > 3 {
            return false;
        }
        p.parse::<u8>().is_ok()
    })
}

/// Validate an IPv6 address (colon-hex format)
pub fn is_valid_ipv6(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
        return false;
    }
    let double_colon_count = s.as_bytes().windows(2).filter(|w| *w == b"::").count();
    if double_colon_count > 1 {
        return false;
    }
    if s.starts_with(':') && !s.starts_with("::") {
        return false;
    }
    if s.ends_with(':') && !s.ends_with("::") {
        return false;
    }
    let segments: Vec<&str> = s.split(':').filter(|seg| !seg.is_empty()).collect();
    if segments.is_empty() {
        return double_colon_count == 1;
    }
    if segments.len() > 8 {
        return false;
    }
    segments
        .iter()
        .all(|seg| seg.len() <= 4 && u16::from_str_radix(seg, 16).is_ok())
}

/// Convert dotted-decimal IPv6 (16 bytes) to standard colon-hex format
/// e.g. "36.9.137.112.10.181.36.74.24.179.107.247.91.255.29.48"
///   => "2409:8970:ab5:244a:18b3:6bf7:5bff:1d30"
pub fn convert_dotted_ipv6_to_standard(raw: &str) -> String {
    let bytes: Vec<u8> = raw
        .split('.')
        .filter_map(|s| s.parse::<u8>().ok())
        .collect();

    if bytes.len() != 16 {
        return raw.to_string();
    }

    let mut groups = Vec::with_capacity(8);
    for i in 0..8 {
        let hi = bytes[i * 2];
        let lo = bytes[i * 2 + 1];
        groups.push(format!("{:x}", (hi as u16) << 8 | lo as u16));
    }
    groups.join(":")
}

/// Extract a string value from a pair (it may be quoted or unquoted)
#[allow(dead_code)]
pub fn extract_value(s: &str) -> &str {
    s.trim().trim_matches('"')
}

// ============================================================================
// 从 main.rs 移入的通用工具函数
// ============================================================================

/// Normalize AT command by auto-prepending AT prefix if missing
pub fn normalize_at_command(cmd: &str) -> String {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if trimmed.starts_with("AT") || trimmed.starts_with("at")
        || trimmed == "A/" || trimmed == "a/"
        || trimmed == "+++"
    {
        trimmed.to_string()
    } else {
        format!("AT{}", trimmed)
    }
}

/// If field is empty, set to default value (kept for backward compatibility)
#[allow(dead_code)]
pub fn set_default(field: &mut String, default: &str) {
    if field.is_empty() {
        *field = default.to_string();
    }
}

/// Decode UCS-2 hex-encoded SMS body text in +CMGL AT responses
pub fn decode_cmgl_body(response: &str) -> String {
    let mut result = String::new();
    let mut in_body = false;

    for line in response.lines() {
        if line.starts_with("+CMGL:") {
            in_body = true;
            // 发件人号码：read_sms_list 已设置 AT+CSCS="GSM"，号码始终为 ASCII 格式
            // 无需（也不应）尝试 UCS2 Hex 解码，否则纯数字号码（如 1065896652051001）
            // 可能被误判为 UCS2 而产生乱码
            result.push_str(line);
            result.push('\n');
        } else if in_body {
            let trimmed = line.trim();
            // 只有 OK 才代表正文结束：空行属于正文内容的一部分，若在此终止，
            // 空行之后的行会被当作「正文外」原样透传，UCS2 正文将不再被解码。
            if trimmed == "OK" {
                in_body = false;
                result.push_str(line);
                result.push('\n');
            } else {
                let decoded = decode_hex_ucs2(trimmed);
                if decoded.is_empty() {
                    result.push_str(line);
                } else {
                    result.push_str(&decoded);
                }
                result.push('\n');
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_hex_ucs2() {
        assert_eq!(decode_hex_ucs2("4E2D56FD79FB52A8"), "中国移动");
        assert_eq!(decode_hex_ucs2("invalid"), "");
        // UCS2 编码的纯 ASCII 文本（高字节 0x00）仍应正常还原
        assert_eq!(decode_hex_ucs2("004F004B"), "OK");
    }

    #[test]
    fn test_decode_hex_ucs2_rejects_plain_digits() {
        // 4/8 位纯数字验证码同样满足「长度 4 的倍数且全为 hex」，
        // 必须原样返回空串让调用方保留原文，否则 "1234" 会变成 U+1234（ሴ）。
        // "12345678" 的第二组 0x5678 落在 CJK 区间，只靠区间判断会漏判。
        assert_eq!(decode_hex_ucs2("1234"), "");
        assert_eq!(decode_hex_ucs2("12345678"), "");
        assert_eq!(decode_hex_ucs2(""), "");
        // 非纯数字的普通 hex 文本同样不该被误判
        assert_eq!(decode_hex_ucs2("ABCDEF12"), "");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2.00 KB");
        assert_eq!(format_bytes(1_048_576), "1.00 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
    }

    #[test]
    fn test_is_valid_ipv4() {
        assert!(is_valid_ipv4("192.168.1.1"));
        assert!(!is_valid_ipv4("256.1.1.1"));
        assert!(!is_valid_ipv4(""));
    }

    #[test]
    fn test_is_valid_ipv6() {
        assert!(is_valid_ipv6("::1"));
        assert!(is_valid_ipv6("2001:db8::1"));
        assert!(!is_valid_ipv6(":::")); // invalid
    }

    #[test]
    fn test_convert_dotted_ipv6() {
        let input = "36.9.137.112.10.181.36.74.24.179.107.247.91.255.29.48";
        let expected = "2409:8970:ab5:244a:18b3:6bf7:5bff:1d30";
        assert_eq!(convert_dotted_ipv6_to_standard(input), expected);

        // not 16 bytes, returns original
        assert_eq!(convert_dotted_ipv6_to_standard("not-ipv6"), "not-ipv6");
    }

    #[test]
    fn test_convert_dotted_ipv6_real_cid1() {
        // 真实设备 CID 1 的点分 IPv6 → "36.9.137.112.11.104.29.156.24.190.51.35.28.252.89.150"
        let input = "36.9.137.112.11.104.29.156.24.190.51.35.28.252.89.150";
        let expected = "2409:8970:b68:1d9c:18be:3323:1cfc:5996";
        assert_eq!(convert_dotted_ipv6_to_standard(input), expected);
    }

    #[test]
    fn test_convert_dotted_ipv6_real_cid2() {
        // 真实设备 CID 2 的点分 IPv6（IPv6 被放入 IPv4 字段）
        let input = "36.9.129.112.11.10.92.211.24.190.51.33.74.23.82.57";
        let expected = "2409:8170:b0a:5cd3:18be:3321:4a17:5239";
        assert_eq!(convert_dotted_ipv6_to_standard(input), expected);
    }
}