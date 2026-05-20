use std::fs;
use std::process::Command;

fn main() {
    // 1. 修复监控路径：与实际读取的 ./src 目录保持一致
    println!("cargo:rerun-if-changed=src/index.html");
    println!("cargo:rerun-if-changed=src/input.css");

    // 2. 确保目标输出目录 templates/ 存在，防止写入失败
    if let Err(e) = fs::create_dir_all("./templates") {
        panic!("🚨 无法创建 templates 目录: {}", e);
    }

    let npx_cmd = if cfg!(target_os = "windows") {
        "npx.cmd"
    } else {
        "npx"
    };

    // 3. 无损复制 HTML 作为 Askama 模板
    if let Err(e) = fs::copy("./src/index.html", "./templates/dist_index.html") {
        panic!("🚨 复制 index.html 失败: {}", e);
    }

    // 4. 调用 Tailwind CLI（优先执行 v4 的 @tailwindcss/cli）
    let status = Command::new(npx_cmd)
        .args([
            "@tailwindcss/cli", // v4 专属编译器，用于解析 input.css 中的新版 @import
            "-i",
            "./src/input.css",
            "-o",
            "./templates/style.css",
            "--minify",
        ])
        .status();

    // 5. 兼容性回退机制
    match status {
        Ok(s) if s.success() => {}
        _ => {
            // 如果本地未全局配置 v4 脚手架，回退至基础的 v3 CLI 命令尝试
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
                    "🚨 Tailwind CSS 编译失败。请确保执行了 `npm install -D @tailwindcss/cli` 安装 v4 编译器。"
                );
            }
        }
    }
}
