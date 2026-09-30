import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import tailwindcss from '@tailwindcss/vite';
import { viteSingleFile } from 'vite-plugin-singlefile';

// 单文件构建：viteSingleFile 会把入口的 JS/CSS 全部内联回 HTML，
// 产物只有 dist/index.html 与 dist/login.html 两个自包含文件（不再有 dist/assets/）。
// 随后由 build.rs 触发、Rust 侧用 include_str! 在编译期嵌入二进制。
//
// ⚠️ 为什么是两个入口分两次构建，而不是一次多页构建：
// viteSingleFile 依赖 `output.codeSplitting = false`（Vite 8/Rolldown）才能把代码合并成
// 单块，而 Rolldown 明确禁止 "多入口 + 关闭代码分割"。硬凑多入口会让 Vue 之类被抽成
// 共享 chunk，入口脚本里残留裸 `import "./vendor-xxx.js"`，内联进 HTML 后必然 404。
// 因此这里用 `--mode login` 跑第二遍，每遍都是单入口，产物各自完整自包含。
//
// 显式基于配置文件位置解析路径，避免 ESM 下 __dirname 不可用、以及 root 变更导致的相对路径歧义。
const fromConfig = (rel) => fileURLToPath(new URL(rel, import.meta.url));

export default defineConfig(({ mode }) => {
  const isLogin = mode === 'login';

  return {
    root: fromConfig('./frontend'),
    plugins: [vue(), tailwindcss(), viteSingleFile()],
    build: {
      outDir: fromConfig('./dist'),
      // 第二遍（登录页）不能清空目录，否则会把上一遍的 index.html 删掉
      emptyOutDir: !isLogin,
      rollupOptions: {
        input: isLogin
          ? { login: fromConfig('./frontend/login.html') }
          : { index: fromConfig('./frontend/index.html') },
      },
    },
  };
});
