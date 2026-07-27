use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::process;

// 终止符列表（纯 ASCII，支持字节切片零分配匹配）
const EXACT_TERMINATORS: &[&[u8]] = &[
    b"BUSY",
    b"ERROR",
    b"NO ANSWER",
    b"NO CARRIER",
    b"NO DIALTONE",
    b"OK",
];

const PREFIX_TERMINATORS: &[&[u8]] = &[b"+CME ERROR:", b"+CMS ERROR:"];

/// 去除字节切片首尾的 ASCII 空白字符 (\r, \n, \t, space)
fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// 检查字节行 trim 后是否匹配已知终止符（零分配）
fn line_is_terminator(line: &[u8]) -> bool {
    let trimmed = trim_ascii(line);
    if trimmed.is_empty() {
        return false;
    }
    EXACT_TERMINATORS.iter().any(|&t| trimmed == t)
        || PREFIX_TERMINATORS.iter().any(|&t| trimmed.starts_with(t))
}

#[derive(Parser)]
#[command(name = "atcmd_rs", about = "AT command tool for Quectel modem")]
struct Cli {
    /// AT command to send to the modem
    at_command: String,

    /// SMS body to write after the modem returns the '>' prompt
    #[arg(short = 'm', long = "message")]
    sms_message: Option<String>,

    /// Hex-encoded raw SMS body (alternative to --message, for UCS2 etc.)
    #[arg(long = "hex-body")]
    hex_body: Option<String>,

    /// SMD device path
    #[arg(short = 'p', long = "path", default_value = "/dev/smd11")]
    device_path: String,
}

fn main() {
    let cli = Cli::parse();

    // Qualcomm SMD 字符设备不支持 O_NONBLOCK 标志。
    // 尝试以非阻塞模式打开会导致 read() 行为不可控，无法通过 WouldBlock 实现非阻塞轮询。
    // 因此 SMD 阻塞读必须用其他方案解决，例如：专用线程 + channel 封装，
    // 或 tokio::spawn_blocking + 取消机制，而不是依赖 O_NONBLOCK。
    // 参见: https://qualcomm.com/smd (Shared Memory Driver)
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&cli.device_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open({}) failed: {}", cli.device_path, e);
            process::exit(1);
        }
    };

    let mut cmd_buf = cli.at_command.as_bytes().to_vec();
    cmd_buf.extend_from_slice(b"\r\n");
    if let Err(e) = file.write_all(&cmd_buf) {
        eprintln!("failed to send '{}' to modem: {}", cli.at_command, e);
        process::exit(1);
    }
    let _ = file.flush();

    if let Err(e) = run_io(&mut file, cli.sms_message.as_deref(), cli.hex_body.as_deref()) {
        eprintln!("I/O error: {}", e);
        process::exit(1);
    }
}

/// 核心 I/O 读写循环
fn run_io(device: &mut (impl Read + Write), sms_message: Option<&str>, hex_body: Option<&str>) -> io::Result<()> {
    // 增大读取缓冲区到 4096 字节，提高接收大量短信时的读取效率
    let mut read_buf = [0_u8; 4096];
    let mut line = Vec::with_capacity(512);
    let mut sms_written = false;

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
            // 【核心修复】：将 '\r' 和 '\n' 都作为行分隔符！
            // 只要遇到 '\r' 或 '\n'，立即检查 line 缓冲区中的文本是否是 OK / ERROR
            if b == b'\n' || b == b'\r' {
                if !line.is_empty() {
                    if line_is_terminator(&line) {
                        return Ok(());
                    }
                    line.clear();
                }
            } else {
                line.push(b);

                // 检测短信 Prompt `>`（只有 line 很短时才可能匹配）
                if let Some(message) = sms_message {
                    if !sms_written && line.len() <= 2 && trim_ascii(&line) == b">" {
                        device.write_all(message.as_bytes())?;
                        device.write_all(&[0x1A])?; // Ctrl+Z
                        device.flush()?;
                        sms_written = true;
                    }
                }
                if let Some(hex) = hex_body {
                    if !sms_written && line.len() <= 2 && trim_ascii(&line) == b">" {
                        let bytes = hex::decode(hex)
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        device.write_all(&bytes)?;
                        device.write_all(&[0x1A])?; // Ctrl+Z
                        device.flush()?;
                        sms_written = true;
                    }
                }

                // 防止异常长数据导致 line 无限膨胀
                if line.len() >= 8192 {
                    line.clear();
                }
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
        assert!(line_is_terminator(b"OK\r\n"));
        assert!(line_is_terminator(b"OK\n"));
        assert!(line_is_terminator(b"OK\r"));
        assert!(line_is_terminator(b"OK"));
    }

    #[test]
    fn test_terminator_error() {
        assert!(line_is_terminator(b"ERROR\r\n"));
        assert!(line_is_terminator(b"ERROR\n"));
    }

    #[test]
    fn test_terminator_cme_error() {
        assert!(line_is_terminator(b"+CME ERROR: 50\r\n"));
        assert!(line_is_terminator(b"+CME ERROR: 50\n"));
    }

    #[test]
    fn test_terminator_cms_error() {
        assert!(line_is_terminator(b"+CMS ERROR: 500\r\n"));
    }

    #[test]
    fn test_terminator_busy() {
        assert!(line_is_terminator(b"BUSY\r\n"));
    }

    #[test]
    fn test_terminator_no_answer() {
        assert!(line_is_terminator(b"NO ANSWER\r\n"));
    }

    #[test]
    fn test_terminator_no_carrier() {
        assert!(line_is_terminator(b"NO CARRIER\r\n"));
    }

    #[test]
    fn test_terminator_no_dialtone() {
        assert!(line_is_terminator(b"NO DIALTONE\r\n"));
    }

    #[test]
    fn test_terminator_data_lines_not_triggered() {
        assert!(!line_is_terminator(b"+CPIN: READY\r\n"));
        assert!(!line_is_terminator(b"+CMGL: 0,\"REC READ\",\"10086\"\r\n"));
        assert!(!line_is_terminator(b"AT+CGMR\r\n"));
    }

    #[test]
    fn test_terminator_partial_match_safe() {
        assert!(!line_is_terminator(b"OK_SOMETHING\r\n"));
        assert!(!line_is_terminator(b"ERROR_SOMETHING\r\n"));
    }

    #[test]
    fn test_terminator_empty_or_garbage() {
        assert!(!line_is_terminator(b""));
        assert!(!line_is_terminator(b"\r\n"));
        assert!(!line_is_terminator(b" \r\n"));
    }

    // ─── 模拟缺失 \n 时的正常终止 ───

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
    fn test_read_loop_without_lf_before_ok() {
        // 模拟短信正文只有 \r 没有 \n 紧接着 OK\r\n 的情况，依然能正常识别退出
        let mut modem = SimulatedModem::new(b"+CMGL: 0,\"REC READ\"\rSMS_TEXT_WITHOUT_LF\rOK\r\n");
        run_io(&mut modem, None, None).unwrap();
    }

    #[test]
    fn test_sms_prompt_trigger() {
        // 模拟 SMS prompt `>` 后写入消息 + Ctrl+Z，然后 OK 退出
        let mut modem = SimulatedModem::new(b"\r\n> \r\nOK\r\n");
        run_io(&mut modem, Some("Hello Modem"), None).unwrap();
        assert_eq!(modem.written, b"Hello Modem\x1A");
    }

    #[test]
    fn test_hex_body_trigger() {
        // 模拟 SMS prompt `>` 后写入 hex 解码原始字节 + Ctrl+Z
        // "你好" 的 UCS2 十六进制编码（UTF-16 BE）
        let mut modem = SimulatedModem::new(b"\r\n> \r\nOK\r\n");
        run_io(&mut modem, None, Some("4F60597D")).unwrap();
        assert_eq!(modem.written, b"\x4F\x60\x59\x7D\x1A");
    }
}
