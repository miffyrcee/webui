/**
 * 控制台入口：把根组件挂到 index.html 的 #app 上。
 *
 * 所有全局状态与 WS 接线都在 App.vue 内部完成，这里只负责挂载，
 * 不额外引入路由或状态库 —— 控制台是单页单视图，多出的抽象只会增加心智负担。
 */
import { createApp } from 'vue';
import App from './App.vue';

createApp(App).mount('#app');
