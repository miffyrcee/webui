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

    // 🟢 步骤 1：先无损地把真实的 HTML 源码复制到目标模板区，确保骨架绝对存在
    if let Err(_) = fs::copy("./frontend/index.html", "./templates/dist_index.html") {
        panic!("🚨 无法复制前端源文件，请检查目录结构！");
    }

    // 🟢 步骤 2：调用 Tailwind CLI，让它【不要指定 -o】！直接将编译压缩后的纯 CSS
    // 追加（Append）到 dist_index.html 的末尾，包裹在 <style> 标签里
    let output = Command::new(npx_cmd)
        .args([
            "tailwindcss",
            "-i",
            "./frontend/input.css",
            "--content",
            "./frontend/index.html",
            "--minify",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let css_content = String::from_utf8_lossy(&out.stdout);

            // 构建一个标准的内联 style 字符串
            let inline_style = format!("\n<style>\n{}\n</style>", css_content);

            // 以追加模式写入目标文件末尾，Askama 同样能完美解析
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open("./templates/dist_index.html")
                .unwrap();

            use std::io::Write;
            file.write_all(inline_style.as_bytes()).unwrap();
            println!("✅ 前端样式已通过后端管道完美融合！");
        }
        _ => {
            panic!("🚨 Tailwind 编译失败，请检查 Node.js 环境！");
        }
    }
}
