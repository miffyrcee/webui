<script setup>
/**
 * 登录页：challenge-response 认证。
 *
 * 流程固定为 /api/get_nonce → sha256(nonce + username + sha256(password)) → /api/login，
 * 密码本身从不离开浏览器，服务端只见到一次性 nonce 的响应值。
 *
 * 主题机制与登录页既有样式保持一致：切换 html 的 `.dark` class
 * （style.css 里的登录页覆盖规则全部基于该类），localStorage 的 'theme'
 * 键与控制台共用，故两页之间的主题偏好是连续的。
 */
import { ref, onMounted } from 'vue';
import { sha256 } from './sha256.js';

const username = ref('admin');
const password = ref('');
const errorText = ref('');
const submitting = ref(false);

const THEME_KEY = 'theme';

function applyTheme(dark) {
  document.documentElement.classList.toggle('dark', dark);
}

function toggleTheme() {
  const dark = !document.documentElement.classList.contains('dark');
  applyTheme(dark);
  localStorage.setItem(THEME_KEY, dark ? 'dark' : 'light');
}

async function submit() {
  if (submitting.value) return;
  if (!username.value.trim() || !password.value) {
    errorText.value = '请填写用户名和密码';
    return;
  }

  errorText.value = '';
  submitting.value = true;
  try {
    const nonceResp = await fetch('/api/get_nonce');
    if (!nonceResp.ok) throw new Error('无法获取服务端随机数');
    const { nonce } = await nonceResp.json();

    const passHash = await sha256(password.value);
    const response = await sha256(nonce + username.value.trim() + passHash);

    const loginResp = await fetch('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: username.value.trim(), nonce, response }),
    });
    const result = await loginResp.json();

    if (loginResp.ok && result.success) {
      window.location.href = '/';
      return;
    }
    errorText.value = '用户名或密码错误';
  } catch (err) {
    errorText.value = `登录响应异常: ${err.message}`;
  } finally {
    submitting.value = false;
  }
}

onMounted(() => {
  const saved = localStorage.getItem(THEME_KEY);
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  applyTheme(saved === 'dark' || (!saved && prefersDark));
});
</script>

<template>
  <!-- 原先 .login-page 挂在 body 上；挂载到 #app 后由本组件自带这层
       flex 居中容器，保证几何装饰与卡片仍是同一套定位关系 -->
  <div class="login-page">
    <!-- 背景几何渐变容器 -->
    <div class="login-wallpaper-container"></div>

  <!-- 主题切换按钮 -->
  <button type="button" title="切换白天/夜晚模式" @click="toggleTheme"
    class="login-theme-toggle fixed top-5 right-5 z-50 p-2.5 rounded-xl bg-white/10 dark:bg-white/10 border border-white/15 dark:border-white/10 text-slate-300 dark:text-slate-400 hover:bg-white/20 hover:text-white transition-all duration-200 cursor-pointer">
    <svg aria-hidden="true" class="w-4 h-4 block dark:hidden" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364-6.364l-.707.707M6.343 17.657l-.707.707m0-12.728l.707.707m12.728 12.728l.707.707M12 8a4 4 0 100 8 4 4 0 000-8z" />
    </svg>
    <svg aria-hidden="true" class="w-4 h-4 hidden dark:block" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z" />
    </svg>
  </button>

  <!-- 登录毛玻璃卡片 -->
  <div class="argon-login-card mx-4">

    <!-- Brand Header -->
    <div class="text-center mb-7">
      <div class="inline-flex items-center justify-center gap-2 mb-1.5">
        <span class="text-[26px] font-black tracking-wider bg-gradient-to-r from-indigo-500 to-purple-500 bg-clip-text text-transparent">Argon</span>
        <span class="text-[10px] font-bold tracking-wider px-2 py-0.5 rounded bg-indigo-500/15 border border-indigo-500/30 text-indigo-400">RM520N</span>
      </div>
      <p class="text-[11px] font-semibold tracking-[1.5px] uppercase text-slate-400 dark:text-slate-400 m-0">5G NR CELLULAR CONTROL CENTER</p>
    </div>

    <!-- 登录表单 -->
    <form @submit.prevent="submit" class="space-y-5">
      <div>
        <label for="username" class="block text-[11px] font-semibold tracking-wider uppercase text-slate-300 dark:text-slate-400 mb-1.5">用户名</label>
        <input id="username" v-model="username" type="text" required placeholder="请输入用户名"
          class="w-full box-border bg-[#1a202c] border border-[#2d3748] text-white rounded-xl px-3.5 py-3 text-[14px] outline-none transition-all duration-200 placeholder:text-[#525f7f] focus:border-indigo-500 focus:bg-[#20293a] focus:ring-2 focus:ring-indigo-500/20" />
      </div>

      <div>
        <label for="password" class="block text-[11px] font-semibold tracking-wider uppercase text-slate-300 dark:text-slate-400 mb-1.5">密码</label>
        <input id="password" v-model="password" type="password" required placeholder="••••••••"
          class="w-full box-border bg-[#1a202c] border border-[#2d3748] text-white rounded-xl px-3.5 py-3 text-[14px] outline-none transition-all duration-200 placeholder:text-[#525f7f] focus:border-indigo-500 focus:bg-[#20293a] focus:ring-2 focus:ring-indigo-500/20" />
      </div>

      <!-- 错误提示 -->
      <div v-show="errorText"
        class="text-center text-[12px] py-2.5 px-3 rounded-lg bg-red-500/15 border border-red-500/30 text-red-400">{{ errorText }}</div>

      <!-- 登录按钮 -->
      <button type="submit" :disabled="submitting"
        class="w-full h-[46px] bg-gradient-to-r from-indigo-600 to-purple-600 border-none rounded-xl text-white text-[14px] font-bold tracking-[2px] cursor-pointer shadow-lg shadow-indigo-600/35 transition-all duration-200 hover:-translate-y-0.5 hover:shadow-xl hover:shadow-indigo-600/50 active:translate-y-0 disabled:opacity-60 disabled:cursor-not-allowed mt-1">
        {{ submitting ? '正在验证...' : '解 锁 进 入' }}
      </button>
    </form>

    <!-- Footer -->
    <div class="mt-6 pt-4 border-t border-white/10 text-center text-[11px] text-slate-400 leading-relaxed">
      凭证通过环境变量 <code class="text-indigo-400 font-mono">WEBUI_USERNAME</code> / <code class="text-indigo-400 font-mono">WEBUI_PASSWORD</code> 配置<br>
      用户名与密码均加密传输
    </div>
  </div>
  </div>
</template>
