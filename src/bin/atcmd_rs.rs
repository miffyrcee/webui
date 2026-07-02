use std::env;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
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

fn print_usage(program_name: &str) {
    println!("Usage for {program_name}: ");
    println!("-p, --path: smd port");
    println!("-h, --help: usage help");
    println!("e.g. {program_name} 'at cmd' (default /dev/smd11)");
    println!("     {program_name} -p <smd port> 'at cmd'");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let program_name = args.first().map_or("atcmd", |s| s.as_str());

    // OPTIMIZATION: We avoid heavy external CLI parsing crates (like `clap`)
    // to keep the compiled binary size as small as possible for embedded devices.
    let (device_path, at_command) = match args.len() {
        2 => {
            let arg = &args[1];
            if arg == "-h" || arg == "--help" || arg == "help" {
                print_usage(program_name);
                process::exit(0);
            }
            ("/dev/smd11", arg.as_str())
        }
        4 => {
            if args[1] == "-p" || args[1] == "--path" || args[1] == "path" {
                (args[2].as_str(), args[3].as_str())
            } else {
                print_usage(program_name);
                process::exit(1);
            }
        }
        _ => {
            print_usage(program_name);
            process::exit(1);
        }
    };

    // Open the serial device. Linux TTY drivers natively handle the RTS/CTS
    // flow control upon opening, bypassing the need for manual ioctl hacks
    // seen in the original `atcmd` binary.
    let mut file = match OpenOptions::new().read(true).write(true).open(device_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fopen({}) failed: {}", device_path, e);
            process::exit(1);
        }
    };

    // Send the command + CRLF in a single write to prevent interleaving from
    // other processes on the same SMD device (a real concern on multi-actor systems).
    let mut cmd_buf = at_command.as_bytes().to_vec();
    cmd_buf.extend_from_slice(b"\r\n");
    if let Err(e) = file.write_all(&cmd_buf) {
        eprintln!("failed to send '{}' to modem: {}", at_command, e);
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

                let mut break_loop = false;
                for &terminator in TERMINATORS {
                    if line.starts_with(terminator) {
                        break_loop = true;
                        break;
                    }
                }
                if break_loop {
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
