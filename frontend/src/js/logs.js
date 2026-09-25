/**
 * 日志 store：AT 命令控制台历史 + 后端环形日志快照。
 *
 * `manual_at` 的回执是 `at_res`（裸文本），无法判定"成功"，
 * 因此控制台只做原样回显，不做任何成功断言。
 * 后端日志是拉取式快照（`get_backend_log` / `settings_log` 推送）。
 */
import { ACTIONS, TYPES, payload } from './contract.js';

const MAX_CONSOLE_LINES = 200;
const MAX_BACKEND_LINES = 200;

export function createLogsStore({ request, send }) {
  return {
    input: '',
    history: [
      '┌─ AT Console Ready ───────────────────────────────┐',
      '│ 键入标准 3GPP AT 指令后按 Enter 发送。           │',
      '└─────────────────────────────────────────────────┘',
    ],
    backend: [],

    sendAt() {
      const cmd = this.input.trim();
      if (!cmd) return;
      this.pushLine(`▶ ${cmd}`);
      send(ACTIONS.MANUAL_AT, payload.manualAt(cmd));
      this.input = '';
    },

    quickAt(cmd) {
      this.input = cmd;
      this.sendAt();
    },

    pushLine(text) {
      this.history.push(text);
      if (this.history.length > MAX_CONSOLE_LINES) {
        this.history.splice(0, this.history.length - MAX_CONSOLE_LINES);
      }
    },

    refreshBackend() {
      if (!request(ACTIONS.GET_BACKEND_LOG, payload.getBackendLog(), {
        expect: TYPES.BACKEND_LOG,
      }).ok) {
        this.appendBackend('✗ 连接不可用，指令未下发');
      }
    },

    onBackendLog(list) {
      this.backend = Array.isArray(list) ? list : [];
    },

    /** `settings_log` 是后端驱动的日志推送，直接追加 */
    appendBackend(msg) {
      if (!msg) return;
      const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
      this.backend.push(`[${time}] ${msg}`);
      if (this.backend.length > MAX_BACKEND_LINES) {
        this.backend.splice(0, this.backend.length - MAX_BACKEND_LINES);
      }
    },

    onTimeout(expect) {
      if (expect !== TYPES.BACKEND_LOG) return false;
      this.appendBackend('✗ 日志拉取超时（后端未返回）');
      return true;
    },
  };
}
