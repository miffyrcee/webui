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

    // Send the command + CRLF in a single write.
    let mut cmd_buf = cli.at_command.as_bytes().to_vec();
    cmd_buf.extend_from_slice(b"\r\n");
    if let Err(e) = file.write_all(&cmd_buf) {
        eprintln!("failed to send '{}' to modem: {}", cli.at_command, e);
        process::exit(1);
    }
    let _ = file.flush();

    if let Err(e) = run_io(&mut file, cli.sms_message.as_deref()) {
        eprintln!("I/O error: {}", e);
        process::exit(1);
    }
}

/// Core read/write loop. Reads modem output byte-by-chunk, echoes to stdout,
/// writes SMS body on detecting `\r\n>` prompt, and breaks on terminator.
fn run_io(device: &mut (impl Read + Write), sms_message: Option<&str>) -> io::Result<()> {
    let mut read_buf = [0_u8; 256];
    let mut line = Vec::with_capacity(256);
    let mut sms_written = false;
    let mut prompt_buf = [0u8; 4];

    loop {
        let n = device.read(&mut read_buf)?;
        if n == 0 {
            eprintln!("EOF from modem");
            return Ok(());
        }

        let chunk = &read_buf[..n];
        io::stdout().write_all(chunk)?;
        io::stdout().flush()?;

        for &b in chunk {
            if let Some(message) = sms_message {
                if !sms_written {
                    prompt_buf.copy_within(1.., 0);
                    prompt_buf[3] = b;
                    if prompt_buf == *b"\r\n> " || prompt_buf[1..] == *b"\r\n>" {
                        device.write_all(message.as_bytes())?;
                        device.write_all(&[0x1A])?;
                        device.flush()?;
                        sms_written = true;
                    }
                }
            }

            line.push(b);
            if b == b'\n' {
                let line_text = String::from_utf8_lossy(&line);
                if line_is_terminator(&line_text) {
                    return Ok(());
                }
                line.clear();
            } else if line.len() >= 4096 {
                line.clear();
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

    // ─── SMS 发送流程模拟 ───

    struct SimulatedModem {
        read_data: io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl SimulatedModem {
        fn new(output: &[u8]) -> Self {
            Self {
                read_data: io::Cursor::new(output.to_vec()),
                written: Vec::new(),
            }
        }
    }

    impl Read for SimulatedModem {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.read_data.read(buf)
        }
    }

    impl Write for SimulatedModem {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_sms_sends_body_after_prompt() {
        // 模组回显命令 → 返回 > 提示符 → 返回 OK
        let mut modem = SimulatedModem::new(b"AT+CMGS=\"+8613800000000\"\r\n\r\n> Hello, OK\r\n");
        run_io(&mut modem, Some("Hello")).unwrap();
        // 写入的数据 = "Hello" + Ctrl+Z
        assert_eq!(modem.written, b"Hello\x1A", "SMS body + Ctrl+Z should be written");
    }

    #[test]
    fn test_sms_prompt_with_trailing_space() {
        // 典型 prompt：\r\n> （带尾随空格）
        let mut modem = SimulatedModem::new(b"\r\n> OK\r\n");
        run_io(&mut modem, Some("test msg")).unwrap();
        assert_eq!(modem.written, b"test msg\x1A");
    }

    #[test]
    fn test_sms_prompt_without_trailing_space() {
        // 某些模组返回 \r\n> 后紧接字符
        let mut modem = SimulatedModem::new(b"\r\n>OK\r\n");
        run_io(&mut modem, Some("hi")).unwrap();
        assert_eq!(modem.written, b"hi\x1A");
    }

    #[test]
    fn test_sms_no_prompt_no_write() {
        // 无 > 提示符，不应写入短信
        let mut modem = SimulatedModem::new(b"+CPIN: READY\r\nOK\r\n");
        run_io(&mut modem, Some("should not send")).unwrap();
        assert!(modem.written.is_empty(), "no > prompt, no write");
    }

    #[test]
    fn test_sms_only_writes_once() {
        // 多个 > 只触发一次写入
        let mut modem = SimulatedModem::new(b"> first\r\n> second\r\nOK\r\n");
        run_io(&mut modem, Some("once")).unwrap();
        assert_eq!(modem.written, b"once\x1A", "SMS body written only once");
    }

    #[test]
    fn test_sms_cms_error_after_send() {
        // 写完后模组返回 CMS ERROR
        let mut modem = SimulatedModem::new(b"\r\n> \r\n+CMS ERROR: 500\r\n");
        run_io(&mut modem, Some("msg")).unwrap();
        assert_eq!(modem.written, b"msg\x1A");
    }

    #[test]
    fn test_sms_gt_in_data_does_not_trigger() {
        // `>` 在数据行中间不应触发
        let mut modem = SimulatedModem::new(b"+QENG: \"5G\"\r\nOK\r\n");
        run_io(&mut modem, Some("no")).unwrap();
        assert!(modem.written.is_empty());
    }

    #[test]
    fn test_normal_command_no_message_no_write() {
        // 无 sms_message 时不会写入任何内容
        let mut modem = SimulatedModem::new(b"OK\r\n");
        run_io(&mut modem, None).unwrap();
        assert!(modem.written.is_empty());
    }

    #[test]
    fn test_sms_long_message() {
        // 长消息写入
        let long_msg = "A".repeat(160);
        let mut modem = SimulatedModem::new(b"\r\n> OK\r\n");
        run_io(&mut modem, Some(&long_msg)).unwrap();
        let expected = [long_msg.as_bytes(), &[0x1A]].concat();
        assert_eq!(modem.written, expected);
    }

    #[test]
    fn test_sms_multiline_response_before_prompt() {
        // 提示符前有多行响应
        let mut modem = SimulatedModem::new(b"+CMGF: 1\r\n\r\n> OK\r\n");
        run_io(&mut modem, Some("hello")).unwrap();
        assert_eq!(modem.written, b"hello\x1A");
    }
}
