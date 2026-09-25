/**
 * 遥测 store：承载后端 `update_type: full|delta` 帧。
 *
 * 注意：后端的 "delta" 并不是增量 diff —— 每个 tick 发来的都是**全量合并快照**，
 * 因此这里对 full / delta 采用同样的 merge 语义。
 * 字段名保持后端原始的 snake_case，避免映射层引入新的错位。
 */
import { ACTIONS, payload, extractPercent, extractSlashPercent } from './contract.js';

const PLACEHOLDER = '--';

/** `YYYY/MM/DD HH:mm:ss` 本地时间戳 */
function localStamp(date = new Date()) {
  const pad = (n) => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

const DEVICE_SPEC_LABELS = [
  ['模组制造商', 'manufacturer'],
  ['产品型号', 'model'],
  ['当前固件分支', 'firmwareVersion'],
  ['IMEI 识别码', 'imei'],
  ['出厂硬件序列号', 'serial'],
  ['卡套主存状态', 'simStatus'],
  ['硬件 PCB 版本', 'hwVersion'],
  ['底层架构类别', 'moduleType'],
  ['IMSI 标识卡号', 'imsi'],
  ['ICCID 芯片编码', 'iccid'],
  ['绑定手机号码', 'phone'],
  ['网络注册状态', 'netStatus'],
  ['当前射频信号', 'signal'],
  ['当前温度', 'temperature'],
  ['VoLTE 通话支持', 'volte'],
  ['GNSS 定位支持', 'gnss'],
  ['极限理论传输速率', 'maxRate'],
];

export function createTelemetryStore({ send }) {
  return {
    // ---- 遥测字段（与 GlobalTelemetry 一一对应）----
    firmware_version: PLACEHOLDER,
    temperature: PLACEHOLDER,
    cpu_usage: PLACEHOLDER,
    memory_usage: PLACEHOLDER,
    sim_status: PLACEHOLDER,
    signal_percentage: PLACEHOLDER,
    internet_connection: PLACEHOLDER,
    active_sim: PLACEHOLDER,
    network_provider: PLACEHOLDER,
    mccmnc: PLACEHOLDER,
    apn: PLACEHOLDER,
    network_mode: PLACEHOLDER,
    bands: PLACEHOLDER,
    bandwidth: PLACEHOLDER,
    earfcn: PLACEHOLDER,
    pci: PLACEHOLDER,
    ipv4: PLACEHOLDER,
    ipv6: PLACEHOLDER,
    uptime: PLACEHOLDER,
    assessment: PLACEHOLDER,
    traffic_stats: PLACEHOLDER,
    cell_id: PLACEHOLDER,
    enb_id: PLACEHOLDER,
    tac: PLACEHOLDER,
    ss_rsrq: PLACEHOLDER,
    ss_rsrp: PLACEHOLDER,
    sinr: PLACEHOLDER,
    updated: PLACEHOLDER,

    // ---- 派生百分比（驱动进度条）----
    cpuPercent: 0,
    memoryPercent: 0,
    rsrqPercent: 0,
    rsrpPercent: 0,
    sinrPercent: 0,

    // ---- 硬件规格抽屉 ----
    specsOpen: false,
    deviceSpecs: {
      manufacturer: PLACEHOLDER,
      model: PLACEHOLDER,
      firmwareVersion: PLACEHOLDER,
      imei: PLACEHOLDER,
      serial: PLACEHOLDER,
      simStatus: PLACEHOLDER,
      hwVersion: PLACEHOLDER,
      moduleType: PLACEHOLDER,
      imsi: PLACEHOLDER,
      iccid: PLACEHOLDER,
      phone: PLACEHOLDER,
      netStatus: PLACEHOLDER,
      signal: PLACEHOLDER,
      temperature: PLACEHOLDER,
      bands: PLACEHOLDER,
      maxRate: PLACEHOLDER,
      volte: PLACEHOLDER,
      gnss: PLACEHOLDER,
    },

    refreshRate: '5',

    /** 由 app.js 注入：把固件版本同步给 ui store */
    onFirmware: null,
    /** 由 app.js 注入：SIM 卡槽切换成功后的联动 */
    onActiveSimChange: null,

    /** 规格抽屉的稳定行数组（不要写成模板里的内联数组，那会每帧重建） */
    get specsRows() {
      return DEVICE_SPEC_LABELS.map(([label, key]) => [label, this.deviceSpecs[key]]);
    },

    /** 后端的 `updated` 是 UTC 字符串，展示前转成本地时间；缺失则退回渲染时刻 */
    get updatedText() {
      const raw = this.updated;
      if (!raw || raw === PLACEHOLDER) return localStamp();
      const iso = String(raw).trim().replace(/\//g, '-').replace(' ', 'T') + 'Z';
      const date = new Date(iso);
      return Number.isNaN(date.getTime()) ? String(raw) : localStamp(date);
    },

    get rsrpColorClass() {
      if (this.rsrpPercent < 30) return 'bg-red-600';
      return this.rsrpPercent < 70 ? 'bg-yellow-500' : 'bg-indigo-600';
    },

    get sinrColorClass() {
      if (this.sinrPercent < 30) return 'bg-red-600';
      return this.sinrPercent < 70 ? 'bg-yellow-500' : 'bg-indigo-600';
    },

    /** 合并一份遥测快照（full 与 delta 语义相同） */
    merge(data) {
      if (!data || typeof data !== 'object') return;

      for (const [key, value] of Object.entries(data)) {
        if (value === undefined || value === null) continue;
        if (key === 'firmware_version' && this.onFirmware) {
          this.onFirmware(value);
        }
        if (key in this) {
          this[key] = value;
        }
      }

      this.cpuPercent = extractPercent(this.cpu_usage);
      this.memoryPercent = extractPercent(this.memory_usage);
      this.rsrqPercent = extractSlashPercent(this.ss_rsrq);
      this.rsrpPercent = extractSlashPercent(this.ss_rsrp);
      this.sinrPercent = extractSlashPercent(this.sinr);
    },

    /** `static_info` 只带少量字段，单独合并 */
    applyStaticInfo(data) {
      if (!data) return;
      if (data.firmware_version) this.onFirmware?.(data.firmware_version);
      if (data.active_sim) this.active_sim = data.active_sim;
      if (data.network_provider) this.network_provider = data.network_provider;
      if (data.apn) this.apn = data.apn;
    },

    applyDeviceInfo(data) {
      if (!data) return;
      const map = {
        manufacturer: 'manufacturer',
        model: 'model',
        firmware_version: 'firmwareVersion',
        imei: 'imei',
        serial: 'serial',
        hw_version: 'hwVersion',
        module_type: 'moduleType',
        sim_status: 'simStatus',
        imsi: 'imsi',
        iccid: 'iccid',
        phone: 'phone',
        net_status: 'netStatus',
        signal: 'signal',
        temperature: 'temperature',
        bands: 'bands',
        max_rate: 'maxRate',
        volte: 'volte',
        gnss: 'gnss',
      };
      for (const [incoming, key] of Object.entries(map)) {
        const value = data[incoming];
        if (value !== undefined && value !== null && value !== '') {
          this.deviceSpecs[key] = value;
        }
      }
    },

    /** 卡槽切换成功后同步展示（由 app.js 在收到 sim_slot_res 成功时调用） */
    markSimSlot(slot) {
      this.active_sim = `SIM ${slot}`;
      this.deviceSpecs.simStatus = `SIM ${slot}`;
      this.onActiveSimChange?.(slot);
    },

    updateRefreshRate() {
      this.refreshRate = String(payload.setInterval(this.refreshRate));
      send(ACTIONS.SET_INTERVAL, payload.setInterval(this.refreshRate));
    },

    /** 页签可见性变化时通知后端，空闲态后端会放宽轮询间隔 */
    setViewState(isActive) {
      send(ACTIONS.SET_VIEW_STATE, payload.setViewState(isActive ? 'active' : 'idle'));
    },

    isRefreshRate(value) {
      return String(this.refreshRate) === String(value);
    },
  };
}
