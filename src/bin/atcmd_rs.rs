use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::process;

// 终止符列表（纯文本，不绑定 \r\n）
const TERMINATORS: &[&str] = &[
    "+CME ERROR:",
    "+CMS ERROR:",
    "BUSY",
    "ERROR",
    "NO ANSWER",
    "NO CARRIER",
    "NO DIALTONE",
    "OK",
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

/// 检查传入文本的 trim 后内容是否匹配已知 terminator
fn line_is_terminator(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    TERMINATORS.iter().any(|&t| {
        if t.ends_with(':') {
            trimmed.starts_with(t)
        } else {
            trimmed == t
        }
    })
}

fn main() {
    let cli = Cli::parse();

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

    if let Err(e) = run_io(&mut file, cli.sms_message.as_deref()) {
        eprintln!("I/O error: {}", e);
        process::exit(1);
    }
}

/// 核心 I/O 读写循环
fn run_io(device: &mut (impl Read + Write), sms_message: Option<&str>) -> io::Result<()> {
    // 增大读取缓冲区到 4096 字节，提高接收大量短信时的读取效率
    let mut read_buf = [0_u8; 4096];
    let mut line = Vec::with_capacity(512);
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

            // 【核心修复】：将 '\r' 和 '\n' 都作为行分隔符！
            // 只要遇到 '\r' 或 '\n'，立即检查 line 缓冲区中的文本是否是 OK / ERROR
            if b == b'\n' || b == b'\r' {
                if !line.is_empty() {
                    let line_text = String::from_utf8_lossy(&line);
                    if line_is_terminator(&line_text) {
                        return Ok(());
                    }
                    line.clear();
                }
            } else {
                line.push(b);
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
        assert!(line_is_terminator("OK\r\n"));
        assert!(line_is_terminator("OK\n"));
        assert!(line_is_terminator("OK\r"));
        assert!(line_is_terminator("OK"));
    }

    #[test]
    fn test_terminator_error() {
        assert!(line_is_terminator("ERROR\r\n"));
        assert!(line_is_terminator("ERROR\n"));
    }

    #[test]
    fn test_terminator_cme_error() {
        assert!(line_is_terminator("+CME ERROR: 50\r\n"));
        assert!(line_is_terminator("+CME ERROR: 50\n"));
    }

    #[test]
    fn test_terminator_cms_error() {
        assert!(line_is_terminator("+CMS ERROR: 500\r\n"));
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
    fn test_terminator_without_crlf() {
        assert!(line_is_terminator("OK\n"));
        assert!(line_is_terminator("ERROR\n"));
        assert!(line_is_terminator("+CME ERROR: 50\n"));
        assert!(line_is_terminator("OK"));
        assert!(line_is_terminator("ERROR"));
    }

    #[test]
    fn test_terminator_data_lines_not_triggered() {
        assert!(!line_is_terminator("+CPIN: READY\r\n"));
        assert!(!line_is_terminator("+CMGL: 0,\"REC READ\",\"10086\"\r\n"));
        assert!(!line_is_terminator("AT+CGMR\r\n"));
    }

    #[test]
    fn test_terminator_partial_match_safe() {
        assert!(!line_is_terminator("OK_SOMETHING\r\n"));
        assert!(!line_is_terminator("ERROR_SOMETHING\r\n"));
    }

    #[test]
    fn test_terminator_empty_or_garbage() {
        assert!(!line_is_terminator(""));
        assert!(!line_is_terminator("\r\n"));
        assert!(!line_is_terminator(" \r\n"));
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
        run_io(&mut modem, None).unwrap();
    }
}
