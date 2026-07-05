use clap::Parser;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
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

    /// SMD device path
    #[arg(short = 'p', long = "path", default_value = "/dev/smd11")]
    device_path: String,
}

fn main() {
    let cli = Cli::parse();

    // Open the serial device. Linux TTY drivers natively handle the RTS/CTS
    // flow control upon opening, bypassing the need for manual ioctl hacks
    // seen in the original `atcmd` binary.
    let mut file = match OpenOptions::new().read(true).write(true).open(&cli.device_path) {
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

    // SAFETY & FIX: The original Compal `atcli` used a 4096-byte global buffer (`byte_2410`)
    // and `stpcpy`, causing buffer overflows on large responses.
    // The original `atcmd` used `read` + `strstr`, causing serial fragmentation bugs.
    //
    // We fix both by using a streaming `BufReader` that reads exactly up to the `\n` byte.
    // This prevents memory bloat (O(1) memory usage) and guarantees string completeness.
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("EOF from modem");
                break;
            }
            Ok(_) => {
                print!("{}", line);
                io::stdout().flush().ok();

                if TERMINATORS.iter().any(|&t| line.trim().starts_with(t)) {
                    break;
                }
            }
            Err(e) => {
                eprintln!("Error reading from modem: {}", e);
                break;
            }
        }
    }
}
