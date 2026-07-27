use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};

#[cfg(test)]
use std::io::BufRead;
use std::process;

// These are the exact terminators extracted from the original Compal firmware.
// The original code iterated through these to know when the modem finished replying.
const TERMINATORS: &[&str] = &[
    "+CME ERROR:",
    "+CMS ERROR:",
    "BUSY\r\n",
    "ERROR\r\n",
    "NO ANSWER\r\n",
    "NO CARRIER\r\n",
    "NO DIALTONE\r\n",
    "OK\r\n",
];

#[derive(Parser)]
#[command(name = "atcmd_rs", about = "AT command tool for Quectel modem")]
struct Cli {
    /// AT command to send to the modem
    at_command: String,

    /// SMS body to write after the modem returns the '>' prompt
    #[arg(short = 'm', long = "message")]
    sms_message: Option<String>,

    /// SMD device path
    #[arg(short = 'p', long = "path", default_value = "/dev/smd11")]
    device_path: String,
}

/// Check whether a line from the modem matches a known terminator.
fn line_is_terminator(line: &str) -> bool {
    TERMINATORS.iter().any(|&t| line.starts_with(t))
}

fn main() {
    let cli = Cli::parse();

    // Open the serial device. Linux TTY drivers natively handle the RTS/CTS
    // flow control upon opening, bypassing the need for manual ioctl hacks
    // seen in the original `atcmd` binary.
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&cli.device_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fopen({}) failed: {}", cli.device_path, e);
            process::exit(1);
        }
    };

    // Send the command + CRLF in a single write to prevent interleaving from
    // other processes on the same SMD device (a real concern on multi-actor systems).
    let mut cmd_buf = cli.at_command.as_bytes().to_vec();
    cmd_buf.extend_from_slice(b"\r\n");
    if let Err(e) = file.write_all(&cmd_buf) {
        eprintln!("failed to send '{}' to modem: {}", cli.at_command, e);
        process::exit(1);
    }
    let _ = file.flush();

    let mut read_buf = [0_u8; 256];
    let mut line = Vec::with_capacity(256);
    let mut sms_written = false;
    let mut prompt_buf = Vec::with_capacity(4);

    loop {
        match file.read(&mut read_buf) {
            Ok(0) => {
                eprintln!("EOF from modem");
                break;
            }
            Ok(n) => {
                let chunk = &read_buf[..n];
                if let Err(e) = io::stdout().write_all(chunk) {
                    eprintln!("Error writing stdout: {}", e);
                    break;
                }
                io::stdout().flush().ok();

                for &b in chunk {
                    if let Some(message) = &cli.sms_message {
                        if !sms_written {
                            prompt_buf.push(b);
                            if prompt_buf.len() > 4 {
                                prompt_buf.remove(0);
                            }
                            if prompt_buf.ends_with(b"\r\n> ") || prompt_buf.ends_with(b"\r\n>") {
                                if let Err(e) = file.write_all(message.as_bytes()) {
                                    eprintln!("failed to send SMS body to modem: {}", e);
                                    process::exit(1);
                                }
                                if let Err(e) = file.write_all(&[0x1A]) {
                                    eprintln!("failed to send SMS terminator to modem: {}", e);
                                    process::exit(1);
                                }
                                let _ = file.flush();
                                sms_written = true;
                            }
                        }
                    }

                    line.push(b);
                    if b == b'\n' {
                        let line_text = String::from_utf8_lossy(&line);
                        if line_is_terminator(&line_text) {
                            return;
                        }
                        line.clear();
                    } else if line.len() >= 4096 {
                        line.clear();
                    }
                }
            }
            Err(e) => {
                eprintln!("Error reading from modem: {}", e);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Terminator 匹配测试 ───

    #[test]
    fn test_terminator_ok() {
        assert!(line_is_terminator("OK\r\n"));
    }

    #[test]
    fn test_terminator_error() {
        assert!(line_is_terminator("ERROR\r\n"));
    }

    #[test]
    fn test_terminator_cme_error() {
        assert!(line_is_terminator("+CME ERROR: 50\r\n"));
        assert!(line_is_terminator("+CME ERROR: unknown\r\n"));
        assert!(line_is_terminator("+CME ERROR:\r\n"));
    }

    #[test]
    fn test_terminator_cms_error() {
        assert!(line_is_terminator("+CMS ERROR: 500\r\n"));
        assert!(line_is_terminator("+CMS ERROR: memory full\r\n"));
    }

    #[test]
    fn test_terminator_busy() {
        assert!(line_is_terminator("BUSY\r\n"));
    }

    #[test]
    fn test_terminator_no_answer() {
        assert!(line_is_terminator("NO ANSWER\r\n"));
    }

    #[test]
    fn test_terminator_no_carrier() {
        assert!(line_is_terminator("NO CARRIER\r\n"));
    }

    #[test]
    fn test_terminator_no_dialtone() {
        assert!(line_is_terminator("NO DIALTONE\r\n"));
    }

    #[test]
    fn test_terminator_data_lines_not_triggered() {
        // 正常数据行不会被误判为 terminator
        assert!(!line_is_terminator("+CPIN: READY\r\n"));
        assert!(!line_is_terminator("+QENG: \"servingcell\",\"NR5G-SA\",\"TDD\"\r\n"));
        assert!(!line_is_terminator("+CSQ: 25,0\r\n"));
        assert!(!line_is_terminator("AT+CGMR\r\n"));
        assert!(!line_is_terminator("ATD*99#\r\n"));
    }

    #[test]
    fn test_terminator_partial_match_safe() {
        // 前缀相似但不是完整 terminator，不会被错误匹配
        assert!(!line_is_terminator("OK_SOMETHING\r\n"));
        assert!(!line_is_terminator("ERROR_SOMETHING\r\n"));
        assert!(!line_is_terminator("BUSY_SIGNAL\r\n"));
    }

    #[test]
    fn test_terminator_without_crlf() {
        // Unix 风格 \n 结尾：\r\n 类 terminator 不匹配，但 +CME/+CMS 前缀类仍匹配
        assert!(!line_is_terminator("OK\n"));
        assert!(!line_is_terminator("ERROR\n"));
        // +CME ERROR: / +CMS ERROR: 定义不含 \r\n，因此即使无 \r\n 也匹配
        assert!(line_is_terminator("+CME ERROR: 50\n"));
        // 完全无换行也不匹配
        assert!(!line_is_terminator("OK"));
        assert!(!line_is_terminator("ERROR"));
    }

    #[test]
    fn test_terminator_empty_or_garbage() {
        assert!(!line_is_terminator(""));
        assert!(!line_is_terminator("\r\n"));
        assert!(!line_is_terminator(" \r\n"));
        assert!(!line_is_terminator("\t\r\n"));
    }

    // ─── 完整读取循环（模拟串口响应） ───

    /// 模拟 line_is_terminator 驱动的读取循环
    fn simulate_read_loop(data: &[u8]) -> Vec<String> {
        let mut reader = std::io::BufReader::new(data);
        let mut line = String::new();
        let mut lines = Vec::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    lines.push(line.clone());
                    if line_is_terminator(&line) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        lines
    }

    #[test]
    fn test_read_loop_ok_terminates() {
        let lines = simulate_read_loop(b"+CPIN: READY\r\n+QENG: ...\r\nOK\r\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "OK\r\n");
    }

    #[test]
    fn test_read_loop_error_terminates() {
        let lines = simulate_read_loop(b"AT+CMD\r\nERROR\r\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "ERROR\r\n");
    }

    #[test]
    fn test_read_loop_cme_error_terminates() {
        let lines = simulate_read_loop(b"AT+CMD\r\n+CME ERROR: 50\r\n");
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("+CME ERROR:"));
    }

    #[test]
    fn test_read_loop_cms_error_terminates() {
        let lines = simulate_read_loop(b"AT+CMGS=\"+8613800000000\"\r\n> \r\n+CMS ERROR: 500\r\n");
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("+CMS ERROR:"));
    }

    #[test]
    fn test_read_loop_no_carrier_terminates() {
        let lines = simulate_read_loop(b"ATD\r\nNO CARRIER\r\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "NO CARRIER\r\n");
    }

    #[test]
    fn test_read_loop_eof_without_terminator() {
        // EOF 未遇到 terminator 也应正常退出
        let lines = simulate_read_loop(b"+CPIN: READY\r\n+CSQ: 25,0\r\n");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_read_loop_empty_response() {
        let lines = simulate_read_loop(b"");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_read_loop_only_terminator() {
        let lines = simulate_read_loop(b"OK\r\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "OK\r\n");
    }

    #[test]
    fn test_read_loop_multiline_before_ok() {
        let data = b"+QENG: \"servingcell\",\"NOCONN\",\"NR5G-SA\",\"TDD\",460,00,39074C001,751,72002F,504990,41,12,-65,-11,22,1,-\r\n+QCAINFO: \"PCC\",504990,12,\"NR5G BAND 41\",751\r\n+CGPADDR: 1,\"10.202.165.254\"\r\nOK\r\n";
        let lines = simulate_read_loop(data);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("+QENG:"));
        assert!(lines[1].starts_with("+QCAINFO:"));
        assert!(lines[2].starts_with("+CGPADDR:"));
        assert_eq!(lines[3], "OK\r\n");
    }

    #[test]
    fn test_read_loop_with_echo() {
        // 模组回显 AT 命令本身，然后返回响应
        let data = b"AT+CPIN?\r\n+CPIN: READY\r\nOK\r\n";
        let lines = simulate_read_loop(data);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "AT+CPIN?\r\n");
        assert_eq!(lines[1], "+CPIN: READY\r\n");
        assert_eq!(lines[2], "OK\r\n");
    }
}
