/**
 * 登录页：challenge-response 认证。
 *
 * 流程固定为 /api/get_nonce → sha256(nonce + username + sha256(password)) → /api/login，
 * 密码本身从不离开浏览器，服务端只见到一次性 nonce 的响应值。
 */
import Alpine from 'alpinejs';
import { sha256 } from './sha256.js';

export function initLogin() {
  const saved = localStorage.getItem('theme');
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  document.documentElement.classList.toggle('dark', saved === 'dark' || (!saved && prefersDark));

  Alpine.data('loginForm', () => ({
    username: 'admin',
    password: '',
    errorText: '',
    submitting: false,

    get canSubmit() {
      return !this.submitting && this.username.trim().length > 0 && this.password.length > 0;
    },

    toggleTheme() {
      const dark = !document.documentElement.classList.contains('dark');
      document.documentElement.classList.toggle('dark', dark);
      localStorage.setItem('theme', dark ? 'dark' : 'light');
    },

    async submit() {
      if (!this.canSubmit) {
        this.errorText = '请填写用户名和密码';
        return;
      }
      this.errorText = '';
      this.submitting = true;
      try {
        const nonceResp = await fetch('/api/get_nonce');
        if (!nonceResp.ok) throw new Error('无法获取服务端随机数');
        const { nonce } = await nonceResp.json();

        const passHash = await sha256(this.password);
        const response = await sha256(nonce + this.username.trim() + passHash);

        const loginResp = await fetch('/api/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username: this.username.trim(), nonce, response }),
        });
        const result = await loginResp.json();

        if (loginResp.ok && result.success) {
          window.location.href = '/';
          return;
        }
        this.errorText = '用户名或密码错误';
      } catch (err) {
        this.errorText = `登录响应异常: ${err.message}`;
      } finally {
        this.submitting = false;
      }
    },
  }));

  Alpine.start();
}

// 入口模块：初始化主题并注册登录表单组件
initLogin();
