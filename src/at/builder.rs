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

/// 短信正文清理：仅移除会提前终止正文的 Ctrl+Z (0x1A)。
/// 正文以 0x1A 结束，内部换行合法，故必须保留 `\r` / `\n`。
pub fn sanitize_sms_body(input: &str) -> String {
    input.chars().filter(|&c| c != '\u{1A}').collect()
}

/// 判断短信正文需要以 UCS2 原始字节下发（非 GSM 7-bit），返回其 hex 编码。
/// 返回 `None` 表示按普通文本（`--message`）发送。
pub fn sms_hex_body_if_needed(message: &str) -> Option<String> {
    if needs_ucs2(message) {
        Some(encode_ucs2_hex(message))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_sms_body_preserves_newlines() {
        // 多行短信的换行必须保留（此前被 sanitize_at_param 一并剥掉）
        assert_eq!(sanitize_sms_body("line1\nline2"), "line1\nline2");
        assert_eq!(sanitize_sms_body("a\r\nb"), "a\r\nb");
    }

    #[test]
    fn sanitize_sms_body_strips_terminator() {
        // 正文中的 0x1A 会导致提前发送，其后内容被当作 AT 指令执行
        assert_eq!(sanitize_sms_body("hi\u{1A}AT+CFUN=0"), "hiAT+CFUN=0");
    }

    #[test]
    fn sms_hex_body_none_for_gsm7bit() {
        assert_eq!(sms_hex_body_if_needed("hello world"), None);
    }

    #[test]
    fn sms_hex_body_ucs2_for_chinese() {
        assert_eq!(sms_hex_body_if_needed("你好").as_deref(), Some("4F60597D"));
    }
}
