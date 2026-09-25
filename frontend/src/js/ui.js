/**
 * UI store：主题、全局加载态、导航、剪贴板提示等纯本地状态。
 * 不含任何出站 WS 动作（页签切换需要拉日志时通过 onTabChange 钩子交给 app.js）。
 */

export const TAB_TITLES = {
  dashboard: 'STATUS / OVERVIEW',
  network: 'CELLULAR & RF CONFIGURATION',
  sms: 'SMS / MESSAGE CENTER',
  usb: 'USB CONFIGURATION',
  logs: 'SYSTEM MEMORY LOGS',
};

const THEME_KEY = 'theme';
const TOAST_MS = 2000;

export function createUiStore() {
  return {
    currentTab: 'dashboard',
    darkMode: false,
    mobileMenuOpen: false,
    globalLoading: false,
    loadingText: '操作执行中，请耐心等待...',
    copyToast: { show: false, text: '' },
    firmwareVersion: '--',

    /** 由 app.js 注入：页签切换后的副作用（如 logs 页签拉取日志） */
    onTabChange: null,
    _toastTimer: null,

    get pageTitle() {
      return TAB_TITLES[this.currentTab] || TAB_TITLES.dashboard;
    },

    initTheme() {
      const saved = localStorage.getItem(THEME_KEY);
      const systemDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
      this.darkMode = saved === 'dark' || (!saved && systemDark);
      this.applyTheme();
    },

    toggleTheme() {
      this.darkMode = !this.darkMode;
      localStorage.setItem(THEME_KEY, this.darkMode ? 'dark' : 'light');
      this.applyTheme();
    },

    applyTheme() {
      document.documentElement.classList.toggle('dark', this.darkMode);
    },

    switchTab(tab) {
      this.currentTab = tab;
      this.mobileMenuOpen = false;
      this.onTabChange?.(tab);
    },

    showLoading(text = '操作执行中，请耐心等待...') {
      this.loadingText = text;
      this.globalLoading = true;
    },

    hideLoading() {
      this.globalLoading = false;
    },

    /** 复制到剪贴板（含非安全上下文回退），并弹提示气泡 */
    copyText(text) {
      if (!text || text === '--') return;

      if (navigator.clipboard?.writeText) {
        navigator.clipboard.writeText(text).catch(() => this._legacyCopy(text));
      } else {
        this._legacyCopy(text);
      }

      clearTimeout(this._toastTimer);
      this.copyToast.text = text.length > 20 ? `${text.slice(0, 20)}...` : text;
      this.copyToast.show = true;
      this._toastTimer = setTimeout(() => {
        this.copyToast.show = false;
      }, TOAST_MS);
    },

    _legacyCopy(text) {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.setAttribute('readonly', '');
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand('copy');
      } catch (err) {
        console.error('复制失败:', err);
      }
      document.body.removeChild(ta);
    },
  };
}
