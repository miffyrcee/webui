/**
 * 应用装配层：唯一的"知道全局"的地方。
 *
 * 职责有三，且仅有三：
 *   1. 创建 ws 传输实例，并把它的回调（连接状态 / pending 变化 / 超时 / 断连中止）
 *      接到对应 store；
 *   2. 持有 `type → handler` 路由表，把入站消息投递给对应 store（先 resolve 取 intent）；
 *   3. 注册全部 Alpine store 并启动。
 *
 * 依赖方向严格单向：template → store → ws。store 之间不互相 import，
 * 需要跨 store 联动的（如 sim_slot_res 成功后刷新遥测卡槽）一律在这里接线。
 */
import Alpine from 'alpinejs';

import { ACTIONS, TYPES, UPDATE_TYPES, payload, parseAtResponse } from './contract.js';
import { createWs } from './ws.js';
import { createUiStore } from './ui.js';
import { createTelemetryStore } from './telemetry.js';
import { createNetStore } from './net.js';
import { createSmsStore } from './sms.js';
import { createUsbStore } from './usb.js';
import { createLogsStore } from './logs.js';

/**
 * 少数 expect 类型没有归属 store，或不需要打扰用户。
 * 未在此登记、也无人认领的超时会退化为一条后端日志，避免状态悬挂。
 */
const SILENT_TIMEOUTS = new Set([
  // 静态信息走的是后台刷新，失败静默即可，不必在日志面板刷屏
  TYPES.STATIC_INFO,
]);

export function initApp() {
  // ---- 1. 各 store（先建，再建 ws：两者通过下面的闭包互相引用）----
  //
  // 注意：Alpine.store(name, obj) 只是把 obj 塞进它自己的 reactive 容器，
  // 并不会把 obj 变成响应式代理。而本文件是唯一从外部改动这些 store 的地方
  // （ws 回调与路由表），所以必须自己先套 Alpine.reactive()，
  // 否则 `store.xxx = ...` 改的是裸对象，DOM 不会更新，且完全静默。
  const uiStore = Alpine.reactive(createUiStore());

  /** ws 的可响应切面。WebSocket 实例本身绝不放进响应式对象。 */
  const wsState = Alpine.reactive({
    connected: false,
    reconnectAttempt: 0,
    pending: [],
    get isBusy() {
      return this.pending.length > 0;
    },
    get statusText() {
      if (this.connected) return this.pending.length > 0 ? '指令下发中...' : '已连接';
      return this.reconnectAttempt > 0 ? '连接中断，正在重连...' : '未连接';
    },
  });

  const isPending = (action) => wsState.pending.some((p) => p.action === action);

  // 这两个包装在调用时才解引用 wsClient（其赋值在其后，闭包求值不成问题）
  const request = (action, payloadValue, options) => wsClient.request(action, payloadValue, options);
  const send = (action, payloadValue) => wsClient.send(action, payloadValue);

  const telemetryStore = Alpine.reactive(createTelemetryStore({ send }));
  const smsStore = Alpine.reactive(createSmsStore({ request }));
  const usbStore = Alpine.reactive(createUsbStore({ request }));
  const logsStore = Alpine.reactive(createLogsStore({ request, send }));
  const netStore = Alpine.reactive(createNetStore({ request, send, isPending, ui: uiStore }));

  // ---- 2. type → handler 路由表 ----
  /** 入站处理器签名统一为 (data, intent)，intent 来自发起请求时登记的意图 */
  const HANDLERS = {
    [TYPES.STATIC_INFO]: (data) => {
      telemetryStore.applyStaticInfo(data);
      netStore.applyStaticApn(data?.apn);
    },
    [TYPES.DEVICE_INFO]: (data) => telemetryStore.applyDeviceInfo(data),
    [TYPES.NETWORK_STATUS]: (data) => netStore.onNetworkStatus(data),
    [TYPES.SCAN_RESULT]: (data) => netStore.onScanResult(data),
    [TYPES.SMS_LIST]: (data) => smsStore.onList(data),
    [TYPES.SMS_SENT]: (data) => smsStore.onSent(data),
    [TYPES.BACKEND_LOG]: (data) => logsStore.onBackendLog(data),
    /** settings_log 是后端**主动推送**的进度/结果文本，不是结构化回执 */
    [TYPES.SETTINGS_LOG]: (data) => logsStore.appendBackend(data),
    [TYPES.BAND_LOCK_RES]: (data, intent) => netStore.onBandLockRes(data, intent),
    [TYPES.CELL_LOCK_RES]: (data) => netStore.onCellLockRes(data),
    [TYPES.DIAGNOSTICS_RES]: (data) => netStore.onDiagnosticsRes(data),
    [TYPES.USB_CONFIG_INFO]: (data) => usbStore.onConfigInfo(data),
    [TYPES.USB_NET_RES]: (data) => usbStore.onNetRes(data),
    [TYPES.MBN_LIST_RES]: (data) => netStore.onMbnListRes(data),
    [TYPES.MBN_SET_RES]: (data) => netStore.onMbnSetRes(data),
    [TYPES.IMEI_RES]: (data, intent) => netStore.onImeiRes(data, intent),
    [TYPES.ETH_RES]: (data) => netStore.onEthRes(data),
    [TYPES.IPPT_RES]: (data) => netStore.onIpptRes(data),

    [TYPES.SIM_SLOT_RES]: (data) => {
      netStore.onSimSlotRes(data);
      // 后端确认切换成功后，遥测里的卡槽展示才跟着走（不做乐观更新）
      if (data?.success) telemetryStore.markSimSlot(data.slot);
    },

    /**
     * at_res 是裸文本，没有结构化判别字段，只能按内容分流：
     * ±CMGL 列表本应走 sms_list，但手工 AT 也会拿到同样的文本。
     */
    [TYPES.AT_RES]: (data) => {
      const text = parseAtResponse(data);
      if (text.includes('+CMGL:')) smsStore.onList(text);
      else logsStore.pushLine(text);
    },
  };

  // ---- 3. ws 传输与回调 ----
  const wsClient = createWs({
    onMessage(type, data, frame) {
      if (type === null) {
        // 遥测帧没有 type，用 update_type 判别；full 与 delta 都是全量快照
        if (frame?.update_type === UPDATE_TYPES.FULL || frame?.update_type === UPDATE_TYPES.DELTA) {
          telemetryStore.merge(frame.data ?? frame);
        } else {
          console.warn('未识别的 WS 帧:', frame);
        }
        return;
      }

      // 先解除 pending 才能拿到 intent —— 文案要依据"发起意图"而非回执内容
      const record = wsClient.resolve(type);
      const handler = HANDLERS[type];
      if (!handler) {
        console.warn('无处理器的消息类型:', type, data);
        return;
      }
      handler(data, record?.intent ?? null);
    },

    onState(connected, attempt) {
      wsState.connected = connected;
      wsState.reconnectAttempt = attempt;
      if (connected) bootstrap();
    },

    onPendingChange(list) {
      wsState.pending = list;
    },

    onTimeout({ expect }) {
      unlockExpect(expect, '✗ 操作超时（后端未返回）');
    },

    onPendingAborted(reason, aborted) {
      // 断连时批量解锁，否则各面板会永久停在"⏳"
      for (const record of aborted ?? []) unlockExpect(record.expect);
      if (aborted?.length) {
        logsStore.appendBackend(`✗ ${reason}，${aborted.length} 个进行中的请求已中止`);
      }
    },
  });

  /** 把某个 expect 的超时/中止交给认领它的 store；无人认领则记一条日志 */
  function unlockExpect(expect, fallbackText) {
    const owners = [netStore, smsStore, usbStore, logsStore];
    const handled = owners.some((store) => store.onTimeout?.(expect) === true);
    if (handled || SILENT_TIMEOUTS.has(expect)) return;
    logsStore.appendBackend(fallbackText || `✗ ${expect} 未收到后端回执`);
  }

  /** 连接（含重连）后重新拉取基础数据，保证界面不会停留在过期快照上 */
  function bootstrap() {
    request(ACTIONS.GET_STATIC_INFO, payload.getStaticInfo(), {
      expect: TYPES.STATIC_INFO,
      timeoutMs: 15000,
    });
    netStore.refreshDeviceSpecs();
    usbStore.refresh();
    if (uiStore.currentTab === 'logs') logsStore.refreshBackend();
  }

  // ---- 4. 跨 store 钩子 ----
  uiStore.onTabChange = (tab) => {
    if (tab === 'logs') logsStore.refreshBackend();
  };

  document.addEventListener('visibilitychange', () => {
    telemetryStore.setViewState(document.visibilityState === 'visible');
  });

  // ---- 5. 注册并启动 ----
  Alpine.store('ws', wsState);
  Alpine.store('ui', uiStore);
  Alpine.store('telemetry', telemetryStore);
  Alpine.store('net', netStore);
  Alpine.store('sms', smsStore);
  Alpine.store('usb', usbStore);
  Alpine.store('logs', logsStore);

  uiStore.initTheme();
  // 暴露给模板里的 x-data 便捷引用，同时便于排查问题
  window.__app = { ws: wsClient, wsState, uiStore, telemetryStore, netStore, smsStore, usbStore, logsStore };

  /** 退出登录：先断开 WS（不再重连），再清服务端会话 */
  window.logout = () => {
    wsClient.close();
    fetch('/api/logout', { method: 'POST' })
      .catch(() => {})
      .finally(() => {
        window.location.href = '/login';
      });
  };

  Alpine.start();
  wsClient.connect();

  return { wsClient, wsState };
}

// 入口模块：注册 stores 并启动 Alpine
initApp();
