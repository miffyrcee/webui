use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/index.html");
    println!("cargo:rerun-if-changed=src/input.css");

    if let Err(e) = fs::create_dir_all("./templates") {
        panic!("无法创建 templates 目录: {}", e);
    }

    let npx_cmd = if cfg!(target_os = "windows") {
        "npx.cmd"
    } else {
        "npx"
    };

    // 调用 Tailwind CLI 编译 CSS
    let status = Command::new(npx_cmd)
        .args([
            "@tailwindcss/cli",
            "-i",
            "./src/input.css",
            "-o",
            "./templates/style.css",
            "--minify",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => {
            let fallback_status = Command::new(npx_cmd)
                .args([
                    "tailwindcss",
                    "-i",
                    "./src/input.css",
                    "-o",
                    "./templates/style.css",
                    "--minify",
                ])
                .status();

            if fallback_status.is_err() || !fallback_status.unwrap().success() {
                panic!(
                    "Tailwind CSS 编译失败。请确保执行了 `npm install -D @tailwindcss/cli` 安装 v4 编译器。"
                );
            }
        }
    }
}
