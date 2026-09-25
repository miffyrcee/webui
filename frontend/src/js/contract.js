/**
 * WS 契约层：action / 消息类型常量、出站载荷构造、裸文本解析。
 *
 * 之所以集中在这里：后端对载荷值类型极为敏感 —— 类型不符时不会报错，
 * 而是静默降级为默认值（例如 flight_mode 传布尔 true 会被当成 OFF），
 * 且未知 action 与未知 diagnostics 子命令后端完全不回消息。
 * 所有出站载荷必须经本模块构造，避免各处手写裸对象时踩类型陷阱。
 */

/**
 * 空中扫频（AT+COPS=?）的前端等待上限。
 *
 * 后端 `scan_available_networks` 的超时为 240s（见 src/backend/real.rs）。
 * 前端若先于后端超时，会在模组仍被独占时解除加载态，用户此时点击其他控件，
 * 指令只会堆在硬件 Actor 队列里继续超时。故此处必须留出余量。
 */
export const SCAN_TIMEOUT_MS = 250000;

/** 客户端 → 服务端 action 名（与 src/main.rs 的 dispatch 一一对应） */
export const ACTIONS = {
  MANUAL_AT: 'manual_at',
  READ_IMEI: 'read_imei',
  WRITE_IMEI: 'write_imei',
  SET_INTERVAL: 'set_interval',
  SET_VIEW_STATE: 'set_view_state',
  GET_STATIC_INFO: 'get_static_info',
  GET_BACKEND_LOG: 'get_backend_log',
  SET_APN: 'set_apn',
  SET_NETWORK_MODE: 'set_network_mode',
  NET_CONNECT: 'net_connect',
  NET_DISCONNECT: 'net_disconnect',
  NETWORK_SCAN: 'network_scan',
  SEND_SMS: 'send_sms',
  GET_SMS_LIST: 'get_sms_list',
  GET_DEVICE_INFO: 'get_device_info',
  REBOOT: 'reboot',
  FACTORY_RESET: 'factory_reset',
  FLIGHT_MODE: 'flight_mode',
  SET_BAND_LOCK: 'set_band_lock',
  SET_CELL_LOCK: 'set_cell_lock',
  GET_DIAGNOSTICS: 'get_diagnostics',
  SET_MODE_PREF: 'set_mode_pref',
  SET_USB_NET_MODE: 'set_usb_net_mode',
  GET_USB_CONFIG: 'get_usb_config',
  SET_SIM_SLOT: 'set_sim_slot',
  GET_MBN_LIST: 'get_mbn_list',
  SET_MBN: 'set_mbn',
  MBN_AUTOSEL: 'mbn_autosel',
  MBN_DEACTIVATE: 'mbn_deactivate',
  SET_ETH_CONFIG: 'set_eth_config',
  SET_IPPT_CONFIG: 'set_ippt_config',
};

/** 服务端 → 客户端 消息类型（顶层 `type` 字段） */
export const TYPES = {
  STATIC_INFO: 'static_info',
  AT_RES: 'at_res',
  NETWORK_STATUS: 'network_status',
  SCAN_RESULT: 'scan_result',
  SMS_LIST: 'sms_list',
  SMS_SENT: 'sms_sent',
  BACKEND_LOG: 'backend_log',
  DEVICE_INFO: 'device_info',
  BAND_LOCK_RES: 'band_lock_res',
  CELL_LOCK_RES: 'cell_lock_res',
  DIAGNOSTICS_RES: 'diagnostics_res',
  SETTINGS_LOG: 'settings_log',
  USB_CONFIG_INFO: 'usb_config_info',
  USB_NET_RES: 'usb_net_res',
  SIM_SLOT_RES: 'sim_slot_res',
  MBN_LIST_RES: 'mbn_list_res',
  MBN_SET_RES: 'mbn_set_res',
  IMEI_RES: 'imei_res',
  ETH_RES: 'eth_res',
  IPPT_RES: 'ippt_res',
};

/**
 * 遥测帧的判别字段不是 `type` 而是 `update_type`。
 * 后端每个 tick 发的都是**全量合并快照**（并非增量 diff），
 * 因此 delta 与 full 一样按整份数据 merge 进 store。
 */
export const UPDATE_TYPES = { FULL: 'full', DELTA: 'delta' };

/** diagnostics 子命令白名单（后端对白名单外的值静默丢弃） */
export const DIAG = {
  NEIGHBOUR: 'neighbour',
  QLTS: 'qlts',
  MBN_LIST: 'mbn_list',
  AUTOSEL_QUERY: 'autosel_query',
};

/** 后端可接受的最大诊断子命令集合，用于防止拼错导致静默无响应 */
const DIAG_VALUES = new Set(Object.values(DIAG));

// ---------------------------------------------------------------------------
// 出站载荷构造
// ---------------------------------------------------------------------------

export const payload = {
  /** payload 为裸字符串 AT 指令 */
  manualAt: (cmd) => String(cmd),
  /** payload 为裸字符串 IMEI */
  writeImei: (imei) => String(imei),
  readImei: () => undefined,
  /** payload 为数字（后端亦接受数字字符串），且被 clamp 到 >= 3 */
  setInterval: (secs) => Math.max(3, Number(secs) || 3),
  /** 后端见 "active" 则 +1 view，其余值一律 -1 */
  setViewState: (state) => (state === 'active' ? 'active' : 'idle'),
  /** auth 必须是**字符串**："0"~"3"；传 JSON 数字会退化为 0 */
  setApn: ({ apn, user = '', pass = '', auth = '0' }) => ({
    apn: String(apn),
    user: String(user),
    pass: String(pass),
    auth: String(auth),
  }),
  /** payload 为裸字符串模式名：auto / nr5g / lte / nr5g_lte / wcdma */
  setNetworkMode: (mode) => String(mode),
  /** SA/NSA 组网偏好，与 set_network_mode 同后端处理 */
  setModePref: (mode) => String(mode),
  netConnect: () => undefined,
  netDisconnect: () => undefined,
  networkScan: () => undefined,
  sendSms: ({ recipient, message }) => ({
    recipient: String(recipient),
    message: String(message),
  }),
  getSmsList: () => undefined,
  getDeviceInfo: () => undefined,
  getStaticInfo: () => undefined,
  getBackendLog: () => undefined,
  reboot: () => undefined,
  factoryReset: () => undefined,
  /** 必须是**字符串** "1"/"0"；布尔或数字会被当成 OFF */
  flightMode: (on) => (on ? '1' : '0'),
  /** nr5g 必须是**布尔**；bands 为 ':' 分隔的字符串或 'all' */
  setBandLock: ({ nr5g, bands }) => ({
    nr5g: Boolean(nr5g),
    bands: String(bands),
  }),
  /** pci/earfcn/band 必须是**数字**；字符串会退化为 0 */
  setCellLock: ({ tech = 'lte', pci, earfcn, band, enable = true }) => {
    const p = { tech: String(tech), enable: Boolean(enable) };
    if (enable) {
      p.pci = Math.trunc(Number(pci));
      p.earfcn = Math.trunc(Number(earfcn));
      if (band !== undefined && band !== null && String(band) !== '') {
        p.band = Math.trunc(Number(band));
      }
    }
    return p;
  },
  /** payload 为裸字符串子命令；白名单外的值后端不回任何消息 */
  getDiagnostics: (sub) => {
    if (!DIAG_VALUES.has(sub)) {
      throw new Error(`未知的诊断子命令: ${sub}`);
    }
    return sub;
  },
  /** payload 为数字（后端亦接受数字字符串） */
  setUsbNetMode: (mode) => Math.trunc(Number(mode)),
  getUsbConfig: () => undefined,
  /** slot 必须是**数字**；字符串会退化为 0（即非法卡槽） */
  setSimSlot: (slot) => ({ slot: Math.trunc(Number(slot)) }),
  getMbnList: () => undefined,
  setMbn: (name) => ({ name: String(name) }),
  /** 必须是**字符串** "1"/"0" */
  mbnAutoSel: (on) => (on ? '1' : '0'),
  mbnDeactivate: () => undefined,
  /** pcie_rc 必须是**布尔** */
  setEthConfig: ({ driver, pcie_rc }) => ({
    driver: String(driver),
    pcie_rc: Boolean(pcie_rc),
  }),
  setIpptConfig: ({ mode }) => ({ mode: String(mode) }),
};

// ---------------------------------------------------------------------------
// 入站裸文本解析
// ---------------------------------------------------------------------------

/** `at_res` 的 data 是裸字符串且可能含 CR/LF，规范化为 LF 便于终端渲染 */
export function parseAtResponse(text) {
  if (typeof text !== 'string') return String(text ?? '');
  return text.replace(/\r\n?/g, '\n').replace(/\n+$/, '');
}

/**
 * 按引号感知切分逗号分隔字段（`<alpha>` 里可能含逗号）。
 * 同时剥掉字段两侧的引号与空白。
 */
function splitQuoted(line) {
  const out = [];
  let cur = '';
  let inQuote = false;
  for (const ch of line) {
    if (ch === '"') {
      inQuote = !inQuote;
    } else if (ch === ',' && !inQuote) {
      out.push(cur);
      cur = '';
    } else {
      cur += ch;
    }
  }
  out.push(cur);
  return out.map((s) => s.trim().replace(/^"|"$/g, '').trim());
}

/**
 * 部分模组把短信正文以 UCS2 十六进制返回（每 4 个 hex 一个字符）。
 * 只在整个字符串都是偶数长度 hex 时才解码，否则原样返回（避免误伤普通文本）。
 */
export function decodeUcs2Hex(text) {
  if (typeof text !== 'string' || !text) return text;
  if (!/^[0-9A-Fa-f]+$/.test(text) || text.length % 4 !== 0) return text;
  let out = '';
  for (let i = 0; i < text.length; i += 4) {
    out += String.fromCharCode(Number.parseInt(text.substring(i, i + 4), 16));
  }
  return out;
}

/**
 * 解析 `+CMGL` 列表原始文本（后端 `sms_list` 的 data 就是这种裸文本）。
 *
 * 格式：
 *   +CMGL: <index>,<stat>,<oa>,<alpha>,<scts>
 *   <正文，可多行>
 *   +CMGL: ...
 *   OK
 */
export function parseCmgl(text) {
  if (typeof text !== 'string' || text === '') return [];
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  const messages = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i].trim();
    if (!line.startsWith('+CMGL:')) {
      i++;
      continue;
    }

    const fields = splitQuoted(line.slice('+CMGL:'.length));
    const index = Number.parseInt(fields[0], 10);
    const sender = fields[2] || 'Unknown';
    const timestamp = fields[4] || '';

    i++;
    const body = [];
    while (i < lines.length) {
      const bodyLine = lines[i].trim();
      if (bodyLine.startsWith('+CMGL:') || bodyLine === 'OK' || bodyLine === '') break;
      body.push(bodyLine);
      i++;
    }

    messages.push({
      index: Number.isNaN(index) ? messages.length : index,
      sender,
      timestamp,
      text: decodeUcs2Hex(body.join('\n')),
    });
  }

  return messages;
}

/** 从 "12.5%" / "45%" 这类字符串取百分比数值，上限 100 */
export function extractPercent(str) {
  if (!str) return 0;
  const m = String(str).match(/(\d+(?:\.\d+)?)\s*%/);
  return m ? Math.min(parseFloat(m[1]), 100) : 0;
}

/** 从 "-85 dBm / 76%" 这类 "值 / 百分比" 字符串取百分比数值，上限 100 */
export function extractSlashPercent(str) {
  if (!str) return 0;
  const parts = String(str).split('/');
  if (parts.length < 2) return 0;
  return Math.min(parseFloat(parts[1].replace('%', '').trim()) || 0, 100);
}
