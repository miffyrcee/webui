use std::env;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;

const DEFAULT_SERIAL_PORT: &str = "/dev/smd11";

/// Serial device handle wrapping a raw tokio file descriptor.
enum SerialHandle {
    Raw(tokio::fs::File),
}

impl SerialHandle {
    // Open the serial device. Linux TTY drivers natively handle the RTS/CTS
    // flow control upon opening, bypassing the need for manual ioctl hacks
    // seen in the original `atcmd` binary.
    async fn open(path: &str) -> Option<Self> {
        match tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .await
        {
            Ok(file) => Some(SerialHandle::Raw(file)),
            Err(e) => {
                eprintln!("❌ 打开串口设备失败: {} ({})", path, e);
                None
            }
        }
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut written = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        while written < buf.len() {
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "写入超时 (5s)，已写入 {}/{} 字节",
                    written,
                    buf.len()
                ));
            }
            match self {
                SerialHandle::Raw(f) => match f.write(&buf[written..]).await {
                    Ok(0) => sleep(Duration::from_millis(10)).await,
                    Ok(n) => written += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(ref e) if e.raw_os_error() == Some(16) => {
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(e) => return Err(format!("写入失败: {}", e)),
                },
            }
        }
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            SerialHandle::Raw(f) => {
                match tokio::time::timeout(Duration::from_millis(500), f.read(buf)).await {
                    Ok(Ok(n)) => Ok(n),
                    Ok(Err(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
                    Ok(Err(e)) => Err(format!("读取失败: {}", e)),
                    Err(_) => Err("读取超时 (500ms)".to_string()),
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let serial_path =
        env::var("AT_SERIAL_PORT").unwrap_or_else(|_| DEFAULT_SERIAL_PORT.to_string());

    if args.len() < 2 {
        eprintln!("用法: atcmd_rs <AT 命令>");
        eprintln!("  或:  atcmd_rs -i               (交互模式)");
        eprintln!("  或:  AT_SERIAL_PORT=/dev/smd11 atcmd_rs AT+CSQ");
        std::process::exit(1);
    }

    // Interactive mode
    if args[1] == "-i" {
        println!("🔌 AT Command Interactive Mode");
        println!("   设备: {}", serial_path);
        println!("   输入 AT 命令并回车，输入 exit 或 quit 退出");
        interactive_mode(&serial_path).await;
        return;
    }

    // One-shot mode: join all positional args as one command
    let cmd = args[1..].join(" ");

    let Some(mut serial) = SerialHandle::open(&serial_path).await else {
        std::process::exit(1);
    };

    match send_at_command(&mut serial, &cmd).await {
        Ok(resp) => {
            println!("{}", resp);
            if resp.contains("ERROR") {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("❌ AT 命令失败: {}", e);
            std::process::exit(1);
        }
    }
}

/// 交互模式：循环读取用户输入并发送 AT 命令
async fn interactive_mode(serial_path: &str) {
    let Some(mut serial) = SerialHandle::open(serial_path).await else {
        return;
    };

    let mut line = String::new();
    loop {
        line.clear();
        print!("atcmd> ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        if std::io::stdin().read_line(&mut line).is_err() || line.trim().is_empty() {
            continue;
        }

        let cmd = line.trim();
        match cmd {
            "exit" | "quit" => {
                println!("再见！");
                break;
            }
            _ => {}
        }

        match send_at_command(&mut serial, cmd).await {
            Ok(resp) => print!("{}", resp),
            Err(e) => eprintln!("❌ {}", e),
        }
    }
}

/// 发送 AT 命令并返回完整响应
async fn send_at_command(serial: &mut SerialHandle, cmd: &str) -> Result<String, String> {
    let full_cmd = format!("{}\r\n", cmd);

    serial.write_all(full_cmd.as_bytes()).await?;

    sleep(Duration::from_millis(200)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 1024];
    let mut response = Vec::with_capacity(2048);
    let mut consecutive_zeros = 0u32;

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "AT 命令总超时 (5s)，已读取 {} 字节",
                response.len()
            ));
        }

        match serial.read(&mut buf).await {
            Ok(0) => {
                consecutive_zeros += 1;
                if consecutive_zeros > 3 {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
            Ok(n) => {
                consecutive_zeros = 0;
                response.extend_from_slice(&buf[..n]);

                if response.len() >= 6 && response[response.len() - 6..] == *b"\r\nOK\r\n" {
                    break;
                }
                if response.len() >= 9 && response[response.len() - 9..] == *b"\r\nERROR\r\n" {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(String::from_utf8_lossy(&response).into_owned())
}
