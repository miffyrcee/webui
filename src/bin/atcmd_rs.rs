use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::process;

// 完整保留原固件提取的所有终结符全集
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
    at_command: String,

    #[arg(short = 'p', long = "path", default_value = "/dev/smd11")]
    device_path: String,
}

fn main() {
    let cli = Cli::parse();

    let mut file = match OpenOptions::new().read(true).write(true).open(&cli.device_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fopen({}) failed: {}", cli.device_path, e);
            process::exit(1);
        }
    };

    // 1. 如果发的是 Ctrl+Z (\x1a) 或 ESC (\x1b)，不追加 \r\n；普通命令追加 \r\n
    let mut cmd = cli.at_command.into_bytes();
    if !cmd.ends_with(b"\x1a") && !cmd.ends_with(b"\x1b") {
        cmd.extend_from_slice(b"\r\n");
    }

    if let Err(e) = file.write_all(&cmd) {
        eprintln!("failed to send to modem: {}", e);
        process::exit(1);
    }
    let _ = file.flush();

    // 2. 块读取（解决无换行的 > 提示符死锁问题）
    let mut buf = [0u8; 512];
    let mut resp = String::new();

    while let Ok(n) = file.read(&mut buf) {
        if n == 0 { break; }
        let chunk = String::from_utf8_lossy(&buf[..n]);
        print!("{}", chunk);
        io::stdout().flush().ok();

        resp.push_str(&chunk);

        let trimmed = resp.trim_end();

        // 优先检查短信提示符 '>'（因为它末尾没有 \n 换行）
        if trimmed.ends_with('>') {
            break;
        }

        // 完整检查 TERMINATORS 数组中的所有正常与异常终结符
        // 包括：+CME ERROR:, +CMS ERROR:, BUSY, ERROR, NO ANSWER, NO CARRIER, NO DIALTONE, OK
        if resp.lines().any(|line| {
            let l = line.trim();
            TERMINATORS.iter().any(|&term| l.starts_with(term))
        }) {
            break;
        }
    }
}
