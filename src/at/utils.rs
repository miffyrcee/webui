/// Utility functions for AT response processing

/// Decode UCS2 hex-encoded string (e.g. "4E2D56FD79FB52A8" -> "中国联通")
pub fn decode_hex_ucs2(hex: &str) -> String {
    if hex.len() % 4 != 0 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    let mut bytes = Vec::new();
    for i in (0..hex.len()).step_by(4) {
        if let Ok(code) = u16::from_str_radix(&hex[i..i + 4], 16) {
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
pub fn extract_value(s: &str) -> &str {
    s.trim().trim_matches('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_hex_ucs2() {
        assert_eq!(decode_hex_ucs2("4E2D56FD79FB52A8"), "中国移动");
        assert_eq!(decode_hex_ucs2("invalid"), "");
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
}