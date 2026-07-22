use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::process;

// 标准响应终结符全集
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
#[command(name = "atcmd_rs", about = "Pure Rust AT command tool for Quectel modem")]
struct Cli {
    /// 待发送的 AT 命令
    at_command: String,

    /// 串口设备路径
    #[arg(short = 'p', long = "path", default_value = "/dev/smd11")]
    device_path: String,
}

fn main() {
    let cli = Cli::parse();

    // 1. 打开串口设备
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

    // 2. 构造指令：若为 Ctrl+Z (\x1a) 或 ESC (\x1b) 结尾不追加 \r\n；普通指令追加 \r\n
    let mut cmd_buf = cli.at_command.as_bytes().to_vec();
    if !cmd_buf.ends_with(b"\x1a") && !cmd_buf.ends_with(b"\x1b") {
        cmd_buf.extend_from_slice(b"\r\n");
    }

    if let Err(e) = file.write_all(&cmd_buf) {
        eprintln!("failed to send to modem: {}", e);
        process::exit(1);
    }
    let _ = file.flush();

    // 3. 循环增量读取与状态判断 (100% 纯 Safe Rust)
    let mut raw_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 1024];

    while let Ok(n) = file.read(&mut chunk) {
        if n == 0 {
            break; // 串口 EOF 关闭
        }

        // 实时输出并刷新 stdout，供外部进程 (WebUI) 实时读取
        let _ = io::stdout().write_all(&chunk[..n]);
        let _ = io::stdout().flush();

        raw_buf.extend_from_slice(&chunk[..n]);

        // 检查是否满足退出条件 (捕获到 OK/ERROR 或短信提示符 '>')
        if is_finished(&raw_buf) {
            break;
        }
    }
}

/// 精准判断 AT 响应是否发送完毕
fn is_finished(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }

    // --- 校验 1: 短信提示符 '>' 捕获 ---
    // 寻找最后一个 '\n'，获取尾部未完成的行
    let last_line = match buf.iter().rposition(|&b| b == b'\n') {
        Some(idx) => &buf[idx + 1..],
        None => buf,
    };

    let trimmed_last = trim_ascii(last_line);

    // 如果最后一行仅为 '>'（例如模组返回 "\r\n> "），说明触发了短信提示符，立刻退出
    if trimmed_last == b">" || (trimmed_last.starts_with(b">") && trimmed_last.len() <= 2) {
        return true;
    }

    // --- 校验 2: 标准终结符捕获 (OK / ERROR / +CME ERROR 等) ---
    let text = String::from_utf8_lossy(buf);
    for line in text.lines() {
        let trimmed = line.trim();
        if TERMINATORS.iter().any(|&term| trimmed.starts_with(term)) {
            return true;
        }
    }

    false
}

/// 辅助函数：去除字节切片首尾的 ASCII 空白符 (\r, \n, 空格, \t)
fn trim_ascii(slice: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = slice.len();

    while start < end
        && (slice[start] == b' '
            || slice[start] == b'\t'
            || slice[start] == b'\r'
            || slice[start] == b'\n')
    {
        start += 1;
    }
    while end > start
        && (slice[end - 1] == b' '
            || slice[end - 1] == b'\t'
            || slice[end - 1] == b'\r'
            || slice[end - 1] == b'\n')
    {
        end -= 1;
    }

    &slice[start..end]
}
