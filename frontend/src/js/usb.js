/**
 * USB store：网络模式选择与当前配置摘要。
 *
 * 契约要点：`set_usb_net_mode` 成功后，后端会**主动重新读取**一次配置并
 * 附在 `usb_net_res.data.config` 里返回，因此摘要是由后端真实结果驱动的，
 * 前端不需要（也不应该）自己乐观更新。
 * 失败变体用的是 `error` 字段，其余消息都用 `msg`。
 */
import { ACTIONS, TYPES, payload } from './contract.js';

/** 模式值 4（NCM/SDX55）后端支持，但 RM520N 上不适用，与旧界面一致不下发展示 */
export const USB_MODES = [
  { value: 0, name: 'RMNET', desc: 'Qualcomm QMI' },
  { value: 1, name: 'ECM', desc: 'Linux/Mac 免驱' },
  { value: 2, name: 'MBIM', desc: 'Win10/11 原生' },
  { value: 3, name: 'RNDIS', desc: 'Windows 免驱' },
  { value: 5, name: 'NCM', desc: '高速网卡 (RM520N)' },
];

const MODE_NAMES = {
  0: 'RMNET (QMI)',
  1: 'ECM (Linux/Mac 免驱)',
  2: 'MBIM (Win10/11 原生)',
  3: 'RNDIS (Windows 免驱)',
  4: 'NCM (SDX55)',
  5: 'NCM (SDX62 高速网卡)',
};

const TIMEOUT_TEXT = '✗ 操作超时（后端未返回）';

export function createUsbStore({ request }) {
  return {
    modes: USB_MODES,
    mode: 0,
    applying: false,
    statusText: '等待操作反馈...',
    current: { name: '--', desc: '未知', num: '--', updated: '--' },

    selectMode(value) {
      this.mode = value;
    },

    apply() {
      this.applying = true;
      const res = request(ACTIONS.SET_USB_NET_MODE, payload.setUsbNetMode(this.mode), {
        expect: TYPES.USB_NET_RES,
        timeoutMs: 60000,
      });
      if (!res.ok) {
        this.applying = false;
        this.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.statusText = '⏳ 正在下发 USB 模式指令...';
    },

    refresh() {
      if (!request(ACTIONS.GET_USB_CONFIG, payload.getUsbConfig(), {
        expect: TYPES.USB_CONFIG_INFO,
      }).ok) {
        this.statusText = '✗ 连接不可用，指令未下发';
      }
    },

    onConfigInfo(data) {
      if (!data?.success) {
        this.current = {
          name: 'ERROR',
          desc: data?.error || data?.msg || '查询失败',
          num: '--',
          updated: this.current.updated,
        };
        return;
      }
      const cfg = data.config;
      if (!cfg) return;

      this.mode = cfg.usbnet_mode;
      this.current = {
        name: cfg.usbnet_supported
          ? MODE_NAMES[cfg.usbnet_mode] || `未知 (${cfg.usbnet_mode})`
          : 'N/A',
        desc: cfg.usbnet_name || '--',
        num: cfg.usbnet_supported ? String(cfg.usbnet_mode) : '--',
        updated: new Date().toLocaleString('zh-CN', { hour12: false }),
      };
    },

    onNetRes(data) {
      this.applying = false;
      this.statusText = data?.success
        ? `✓ ${data?.msg || 'OK'}${data?.note ? `（${data.note}）` : ''}`
        : `✗ ${data?.msg || 'Failed'}`;

      // 后端已回传最新配置，直接以后端结果为准刷新摘要
      if (data?.config) {
        this.onConfigInfo({ success: true, config: data.config });
      }
    },

    onTimeout(expect) {
      if (expect === TYPES.USB_NET_RES) {
        this.applying = false;
        this.statusText = TIMEOUT_TEXT;
        return true;
      }
      if (expect === TYPES.USB_CONFIG_INFO) {
        this.statusText = TIMEOUT_TEXT;
        return true;
      }
      return false;
    },
  };
}
