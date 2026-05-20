use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=frontend/index.html");
    println!("cargo:rerun-if-changed=frontend/input.css");

    let npx_cmd = if cfg!(target_os = "windows") {
        "npx.cmd"
    } else {
        "npx"
    };

    // 1. 无损复制 HTML 作为 Askama 的模板模板
    let _ = fs::copy("./src/index.html", "./templates/dist_index.html");

    // 2. 调用 Tailwind CLI，指定输出到 templates/style.css，让它回归一个纯粹的 CSS 文件
    let status = Command::new(npx_cmd)
        .args([
            "tailwindcss",
            "-i",
            "./src/input.css",
            "-o",
            "./templates/style.css",
            "--content",
            "./frontend/index.html",
            "--minify",
        ])
        .status();

    if status.is_err() || !status.unwrap().success() {
        panic!("🚨 Tailwind CSS 编译失败，请检查 Node.js 环境！");
    }
}
