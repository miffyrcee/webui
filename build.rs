//! 构建脚本：调用 Vite 编译 `frontend/` → `dist/`，产物再由 rust-embed 在编译期嵌入二进制。
//!
//! 这里不再触碰 `templates/` 目录（该目录由独立流程维护）。

use std::path::Path;
use std::process::Command;

/// 前端源码与构建配置：任一变化都会重新触发 `npm run build`
const WATCHED_INPUTS: &[&str] = &["frontend", "vite.config.js", "package.json"];

fn main() {
    for path in WATCHED_INPUTS {
        println!("cargo:rerun-if-changed={}", path);
    }

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

    if built {
        return;
    }

    // Node/npm 缺失或构建失败：若已存在可用产物则降级放行，否则终止并给出可操作提示
    if Path::new("dist/index.html").exists() {
        println!(
            "cargo:warning=前端构建失败或未找到 {}，回退使用已存在的 dist/ 产物",
            npm
        );
        return;
    }

    panic!(
        "前端构建失败：请安装 Node.js 并执行 `npm install`，确认 `{} run build` 能正常产出 dist/ 后重试。",
        npm
    );
}
