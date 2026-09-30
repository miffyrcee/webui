/**
 * 登录页入口：挂载 Vue 组件。
 *
 * 认证流程与主题逻辑都在 Login.vue 内（challenge-response，
 * 密码不离开浏览器）。这里只负责 bootstrap。
 */
import { createApp } from 'vue';
import Login from './Login.vue';

createApp(Login).mount('#app');
