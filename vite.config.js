import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import tailwindcss from '@tailwindcss/vite';

// 多页构建：主控台 + 登录页
// 产物输出到 dist/，由 rust-embed 在 cargo build 时编译期嵌入二进制。
// 显式基于配置文件位置解析路径，避免 ESM 下 __dirname 不可用、以及 root 变更导致的相对路径歧义。
const fromConfig = (rel) => fileURLToPath(new URL(rel, import.meta.url));

export default defineConfig({
  root: fromConfig('./frontend'),
  plugins: [tailwindcss()],
  build: {
    outDir: fromConfig('./dist'),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: fromConfig('./frontend/index.html'),
        login: fromConfig('./frontend/login.html'),
      },
    },
  },
});
