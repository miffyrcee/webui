/**
 * WebSocket 传输层：连接、指数退避重连、入站分发、请求生命周期（pending / 超时）。
 *
 * 本模块不 import 任何 store，只通过回调把事件交出去，保证依赖方向是
 * "store → ws" 单向。所有出站消息都经 request() 登记 pending，原因见下：
 *
 *   1. 后端对未知 action / 未知诊断子命令**完全不回消息**；
 *   2. 后端没有 per-request 超时，串口 actor 阻塞时请求会一直挂着。
 * 若不在客户端兜底，UI 状态会永久停留在"⏳ 进行中"，即所谓状态悬挂。
 */

const RECONNECT_BASE_MS = 3000;
const RECONNECT_MAX_MS = 60000;
const DEFAULT_TIMEOUT_MS = 30000;

/**
 * @param {object} hooks
 * @param {(type: string|null, data: any) => void} hooks.onMessage 入站消息（遥测帧的 type 为 null，data 为整帧）
 * @param {(connected: boolean, attempt: number) => void} hooks.onState 连接状态变化
 * @param {(list: Array<object>) => void} hooks.onPendingChange 进行中请求列表变化
 * @param {(info: {action: string, expect: string}) => void} [hooks.onTimeout] 请求超时
 * @param {(reason: string, aborted: Array<{action:string, expect:string, intent:any}>) => void} hooks.onPendingAborted
 *        断连导致 pending 被批量中止；aborted 为被中止的记录，供各 store 解锁自己的进行中状态
 */
export function createWs({
  onMessage,
  onState,
  onPendingChange,
  onTimeout,
  onPendingAborted,
  url,
} = {}) {
  let socket = null;
  let attempt = 0;
  let reconnectTimer = null;
  let manualClose = false;
  /** @type {Map<string, {action:string, expect:string, intent:any, startedAt:number, timer:any, timeoutMs:number}>} */
  const pending = new Map();

  const wsUrl =
    url ||
    `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/ws`;

  function pendingList() {
    return [...pending.values()].map((r) => ({
      action: r.action,
      expect: r.expect,
      intent: r.intent,
      startedAt: r.startedAt,
    }));
  }

  function notifyPending() {
    onPendingChange?.(pendingList());
  }

  function connect() {
    manualClose = false;
    clearTimeout(reconnectTimer);

    socket = new WebSocket(wsUrl);

    socket.onopen = () => {
      attempt = 0;
      onState?.(true, attempt);
    };

    socket.onmessage = (event) => {
      let msg;
      try {
        msg = JSON.parse(event.data);
      } catch (err) {
        // 非 JSON 帧不应断开连接，只记录
        console.error('无法解析的 WebSocket 帧:', event.data, err);
        return;
      }
      // 遥测帧用 update_type 判别，其余用 type
      const type = typeof msg.type === 'string' ? msg.type : null;
      onMessage?.(type, msg.data !== undefined ? msg.data : msg, msg);
    };

    socket.onclose = () => {
      abortAllPending('连接已断开');
      if (manualClose) {
        onState?.(false, 0);
        return;
      }

      // 先把本次断连计入重连次数，UI 才能在等待期间显示"正在重连"而不是"未连接"
      attempt++;
      onState?.(false, attempt);

      const delay =
        Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** (attempt - 1)) +
        Math.floor(Math.random() * 1000);
      reconnectTimer = setTimeout(connect, delay);
    };

    socket.onerror = () => {
      // onerror 后浏览器必定触发 onclose，重连逻辑统一放在 onclose
      try {
        socket?.close();
      } catch {
        /* ignore */
      }
    };
  }

  function isOpen() {
    return socket !== null && socket.readyState === WebSocket.OPEN;
  }

  function send(action, payload) {
    if (!isOpen()) return false;
    const msg = { action };
    if (payload !== undefined) msg.payload = payload;
    socket.send(JSON.stringify(msg));
    return true;
  }

  /**
   * 发起一次需要结构化回执的请求。
   * @returns {{ ok: boolean, reason?: 'offline'|'busy' }}
   */
  function request(action, payload, options = {}) {
    const { expect = null, timeoutMs = DEFAULT_TIMEOUT_MS, intent = null } = options;

    if (!isOpen()) return { ok: false, reason: 'offline' };

    if (expect) {
      if (pending.has(expect)) return { ok: false, reason: 'busy' };
      const record = {
        action,
        expect,
        intent,
        startedAt: Date.now(),
        timeoutMs,
        timer: null,
      };
      record.timer = setTimeout(() => {
        pending.delete(expect);
        notifyPending();
        onTimeout?.({ action, expect, intent });
      }, timeoutMs);
      pending.set(expect, record);
      notifyPending();
    }

    send(action, payload);
    return { ok: true };
  }

  /**
   * 收到某类回执时解除对应 pending。
   * @returns {object|null} pending 记录（含 intent），供 handler 判断发起意图
   */
  function resolve(expect) {
    const record = pending.get(expect);
    if (!record) return null;
    clearTimeout(record.timer);
    pending.delete(expect);
    notifyPending();
    return record;
  }

  function abortAllPending(reason) {
    if (pending.size === 0) return;
    const aborted = pendingList();
    for (const record of pending.values()) clearTimeout(record.timer);
    pending.clear();
    notifyPending();
    onPendingAborted?.(reason, aborted);
  }

  function close() {
    manualClose = true;
    clearTimeout(reconnectTimer);
    abortAllPending('连接已关闭');
    try {
      socket?.close();
    } catch {
      /* ignore */
    }
    socket = null;
  }

  return { connect, close, send, request, resolve, isOpen, pendingList };
}
