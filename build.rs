//! 构建脚本：调用 Vite 编译 `frontend/` → `dist/`（单文件 HTML），
//! 产物再由 `src/web/assets.rs` 的 `include_str!` 在编译期嵌入二进制。
//!
//! 因为 `include_str!` 要求文件在编译期必须存在，本脚本的最后一道防线是：
//! 一旦前端构建不可用（未装 Node.js / npm 失败 / 首次构建且无 dist/），
//! 就写入占位 HTML，避免 rustc 直接报 "couldn't read ... dist/index.html"。
//!
//! 这里不再触碰 `templates/` 目录（该目录由独立流程维护）。

use std::path::Path;
use std::process::Command;

/// 前端源码与构建配置：任一变化都会重新触发 `npm run build`
const WATCHED_INPUTS: &[&str] = &["frontend", "vite.config.js", "package.json"];

/// `include_str!` 依赖的编译期产物
const REQUIRED_OUTPUTS: &[&str] = &["dist/index.html", "dist/login.html"];

fn all_outputs_present() -> bool {
    REQUIRED_OUTPUTS.iter().all(|p| Path::new(p).exists())
}

fn main() {
    for path in WATCHED_INPUTS {
        println!("cargo:rerun-if-changed={}", path);
    }
    // 注意：不要把 dist/ 下的产物也列进 rerun-if-changed。本脚本每次运行都会让
    // vite 重写这两个文件，mtime 一变 cargo 就又判定"需重跑"，于是每次 cargo build
    // 都白跑一遍 npm（实测确认）。代价是：dist/ 手工删除且 target/ 仍热时，
    // 本脚本命中缓存不重跑，include_str! 会以"找不到 dist/index.html"中断 ——
    // 此时 touch 任一 frontend 文件或 cargo clean -p quectel-webui 即可恢复。

    let npm = if cfg!(target_os = "windows") {
        "npm.cmd"
    } else {
        "npm"
    };

    let built = Command::new(npm)
        .args(["run", "build"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if built && all_outputs_present() {
        return;
    }

    // Node/npm 缺失或构建失败：若已存在完整产物则降级放行（可能是上次成功构建留下的）
    if all_outputs_present() {
        println!("cargo:warning=前端构建失败或未找到 {}，回退使用已存在的 dist/ 产物", npm);
        return;
    }

    // 编译期兜底占位：确保未装 Node.js 时 cargo check/build 也不会因 include_str! 找不到文件而中断
    std::fs::create_dir_all("dist").ok();
    if !Path::new("dist/index.html").exists() {
        std::fs::write(
            "dist/index.html",
            "<!DOCTYPE html><html lang=\"zh-CN\"><body><h1>WebUI 正在构建，请先执行 npm install &amp;&amp; npm run build</h1></body></html>",
        )
        .ok();
    }
    if !Path::new("dist/login.html").exists() {
        std::fs::write(
            "dist/login.html",
            "<!DOCTYPE html><html lang=\"zh-CN\"><body><h1>登录页正在构建，请先执行 npm install &amp;&amp; npm run build</h1></body></html>",
        )
        .ok();
    }

    println!(
        "cargo:warning=已生成临时占位页面（非真实前端产物），请在最终打包前执行 `npm install && npm run build`。"
    );
}
