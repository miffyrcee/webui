/**
 * 短信 store：收件箱、详情、发送。
 *
 * 契约要点：后端 `sms_list` 的 data 是**裸字符串**（原始 +CMGL 文本），
 * 需要本地解析；`sms_sent` 只有人类可读的 status 字符串，没有 success 字段，
 * 因此成功与否只能按 "successfully" 判定 —— 失败时保留正文以便重试。
 */
import { ACTIONS, TYPES, payload, parseCmgl } from './contract.js';

const TIMEOUT_TEXT = '✗ 操作超时（后端未返回）';

export function createSmsStore({ request }) {
  return {
    messages: [],
    selectedMsg: null,
    recipient: '',
    message: '',
    statusText: '',

    get charCount() {
      return this.message.length;
    },

    get hasDetail() {
      return this.selectedMsg !== null;
    },

    refresh() {
      if (!request(ACTIONS.GET_SMS_LIST, payload.getSmsList(), {
        expect: TYPES.SMS_LIST,
      }).ok) {
        this.statusText = '✗ 连接不可用，指令未下发';
      }
    },

    showDetail(msg) {
      this.selectedMsg = msg;
    },

    clearDetail() {
      this.selectedMsg = null;
    },

    replyToSelected() {
      if (this.selectedMsg?.sender) this.recipient = this.selectedMsg.sender;
    },

    send() {
      if (!this.recipient || !this.message) {
        this.statusText = '✗ 请同时填写目标收信人及正文内容';
        return;
      }
      if (!request(ACTIONS.SEND_SMS, payload.sendSms({
        recipient: this.recipient,
        message: this.message,
      }), { expect: TYPES.SMS_SENT, timeoutMs: 60000 }).ok) {
        this.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.statusText = '⏳ 正在发送短信...';
    },

    /** data 可能是裸 +CMGL 文本（真实后端）或消息数组（演示后端） */
    onList(data) {
      if (typeof data === 'string') {
        this.messages = parseCmgl(data);
      } else if (Array.isArray(data)) {
        this.messages = data;
      } else if (Array.isArray(data?.messages)) {
        this.messages = data.messages;
      } else {
        this.messages = [];
      }
    },

    onSent(data) {
      const status = data?.status || '';
      if (status.includes('successfully')) {
        this.statusText = `✓ 短信已发送至 ${data?.recipient || ''}`;
        this.message = '';
      } else {
        // 失败保留正文，便于用户修正后重试
        this.statusText = `✗ ${status || '短信发送失败'}，请检查信号与余额`;
      }
    },

    onTimeout(expect) {
      if (expect !== TYPES.SMS_LIST && expect !== TYPES.SMS_SENT) return false;
      this.statusText = TIMEOUT_TEXT;
      return true;
    },
  };
}
