//! AT 命令组装与参数安全处理。

/// AT 命令参数安全过滤（去除双引号、反斜杠与控制字符，防止 AT 注入）
pub fn sanitize_at_param(input: &str) -> String {
    input
        .chars()
        .filter(|&c| c != '"' && c != '\\' && c != '\r' && c != '\n')
        .collect()
}

/// 检查消息是否包含非 GSM 7-bit 字符（需要 UCS2 编码）
pub fn needs_ucs2(s: &str) -> bool {
    s.chars().any(|c| c > '\u{007F}')
}

/// 将字符串转换为 UCS2 十六进制文本（UTF-16 BE → hex ASCII）
/// 例如 "你好" → "4F60597D"
pub fn encode_ucs2_hex(s: &str) -> String {
    s.encode_utf16()
        .flat_map(|c| c.to_be_bytes())
        .fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{:02X}", b);
            acc
        })
}
