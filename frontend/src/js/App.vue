<script setup>
import { ref, reactive, computed, onMounted, nextTick } from 'vue';
import {
  ACTIONS,
  TYPES,
  UPDATE_TYPES,
  SCAN_TIMEOUT_MS,
  payload,
  parseAtResponse,
  parseCmgl,
  extractPercent,
  extractSlashPercent,
} from './contract.js';
import { createWs } from './ws.js';
import { btnVariants, cardVariants, badgeVariants, inputVariants } from './cva.js';

// ---- 常量 ----
const NR5G_BANDS = ['1', '3', '5', '7', '8', '20', '28', '38', '41', '71', '77', '78', '79'];
const LTE_BANDS = ['1', '3', '5', '7', '8', '20', '28', '34', '38', '39', '40', '41'];
const USB_MODES = [
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

// ---- 全局响应式状态 ----
const ui = reactive({
  currentTab: 'dashboard',
  theme: 'dark',
  mobileMenuOpen: false,
  globalLoading: false,
  loadingText: '操作执行中，请耐心等待...',
  firmwareVersion: '--',
  copyToast: { show: false, text: '', ok: true },
});

const wsState = reactive({
  connected: false,
  reconnectAttempt: 0,
  pending: [],
});

const telemetry = reactive({
  temperature: '--',
  sim_status: '--',
  signal_percentage: '--',
  internet_connection: '--',
  cpu_usage: '--',
  memory_usage: '--',
  active_sim: '--',
  network_provider: '--',
  mccmnc: '--',
  apn: '--',
  network_mode: '--',
  bandwidth: '--',
  bands: '--',
  earfcn: '--',
  pci: '--',
  ipv4: '--',
  ipv6: '--',
  uptime: '--',
  assessment: '--',
  traffic_stats: '--',
  cell_id: '--',
  enb_id: '--',
  tac: '--',
  ss_rsrq: '--',
  ss_rsrp: '--',
  sinr: '--',
  updated: '--',
  cpuPercent: 0,
  memoryPercent: 0,
  rsrqPercent: 0,
  rsrpPercent: 0,
  sinrPercent: 0,
  refreshRate: '5',
  deviceSpecs: {
    manufacturer: '--', model: '--', firmwareVersion: '--', imei: '--',
    serial: '--', simStatus: '--', hwVersion: '--', moduleType: '--',
    imsi: '--', iccid: '--', phone: '--', netStatus: '--',
    signal: '--', temperature: '--', bands: '--',
    maxRate: '--', volte: '--', gnss: '--',
  },
});

const net = reactive({
  apnPreset: 'custom',
  apnForm: { apn: '', user: '', pass: '', auth: '0' },
  networkMode: 'auto',
  netStatusText: '',
  simSlotStatusText: '',
  eth: { driver: 'r8125', pcieRc: '1', statusText: '' },
  ippt: { mode: 'dmz', statusText: '' },
  imeiInput: '',
  imeiStatusText: '',
  bandRat: 'nr5g',
  selectedNr5gBands: [],
  selectedLteBands: [],
  bandLockStatusText: '就绪',
  bandResetPending: false,
  cellLock: { tech: 'lte', pci: '', earfcn: '', band: '', statusText: '就绪' },
  scan: { scanning: false, btnText: '触发扫频', statusText: '等待搜网。最长约 240 秒。', networks: [] },
  diag: { outputText: '点击按钮查询并触发高级诊断交互。', isError: false },
  mbn: { list: [], selected: '', statusText: '' },
});

const sms = reactive({
  messages: [],
  selectedMsg: null,
  recipient: '',
  message: '',
  statusText: '',
});

const usb = reactive({
  mode: 0,
  applying: false,
  statusText: '等待操作反馈...',
  current: { name: '--', desc: '未知', num: '--', updated: '--' },
});

const logs = reactive({
  input: '',
  history: [
    '┌─ AT Console Ready ───────────────────────────────┐',
    '│ 键入标准 3GPP AT 指令后按 Enter 发送。           │',
    '└─────────────────────────────────────────────────┘',
  ],
  backend: [],
});

const consoleOut = ref(null);
const logOut = ref(null);

// ---- 计算属性 ----
const bandLockPreview = computed(() => {
  const bands = net.bandRat === 'nr5g' ? net.selectedNr5gBands : net.selectedLteBands;
  const prefix = net.bandRat === 'nr5g' ? 'n' : 'B';
  return bands.length > 0 ? bands.map((b) => prefix + b).join(':') : 'all (出厂全频段)';
});

// ---- 工具函数 ----

/**
 * 写入剪贴板，返回是否**真的**写成功。
 *
 * 优先异步 Clipboard API —— 它只在安全上下文可用，本服务常以明文 HTTP
 * 暴露，此时 navigator.clipboard 直接是 undefined。旧实现
 * `navigator.clipboard?.writeText().catch(()=>{})` 把这两种失败都吞掉，
 * 然后无条件弹出「已复制」，是典型的假成功提示。故补一条隐藏 textarea +
 * execCommand('copy') 的回退路径，两条路都失败则由调用方如实报错。
 */
async function copyText(text) {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // 权限被拒 / 非聚焦文档等，继续走下面的回退路径
    }
  }
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.top = '-9999px';
    document.body.appendChild(ta);
    ta.select();
    ta.setSelectionRange(0, text.length);
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

async function showCopy(text) {
  if (!text || text === '--') return;
  const ok = await copyText(text);
  ui.copyToast.ok = ok;
  ui.copyToast.text = ok
    ? text.length > 25
      ? `${text.slice(0, 25)}...`
      : text
    : '复制失败，请手动选中复制';
  ui.copyToast.show = true;
  setTimeout(() => (ui.copyToast.show = false), 2000);
}

function toggleTheme() {
  ui.theme = ui.theme === 'dark' ? 'light' : 'dark';
  document.documentElement.setAttribute('data-theme', ui.theme);
  localStorage.setItem('theme', ui.theme);
}

function switchTab(tab) {
  ui.currentTab = tab;
  ui.mobileMenuOpen = false;
  if (tab === 'logs') refreshBackendLogs();
}

function handleLogout() {
  wsClient.close();
  fetch('/api/logout', { method: 'POST' }).finally(() => {
    window.location.href = '/login';
  });
}

// ---- WS 通信接线 ----
let wsClient;

function request(action, payloadValue, options) {
  return wsClient.request(action, payloadValue, options);
}

function send(action, payloadValue) {
  return wsClient.send(action, payloadValue);
}

function mergeTelemetry(data) {
  if (!data || typeof data !== 'object') return;
  for (const [key, value] of Object.entries(data)) {
    if (value === undefined) continue;
    if (key === 'firmware_version' && value) ui.firmwareVersion = value;
    if (key in telemetry) telemetry[key] = value === null ? '--' : value;
  }
  telemetry.cpuPercent = extractPercent(telemetry.cpu_usage);
  telemetry.memoryPercent = extractPercent(telemetry.memory_usage);
  telemetry.rsrqPercent = extractSlashPercent(telemetry.ss_rsrq);
  telemetry.rsrpPercent = extractSlashPercent(telemetry.ss_rsrp);
  telemetry.sinrPercent = extractSlashPercent(telemetry.sinr);
}

// ---- 业务操作 ----
function onApnPresetChange() {
  net.apnForm.apn = net.apnPreset === 'custom' ? '' : net.apnPreset;
}

function applyApn() {
  if (!net.apnForm.apn) {
    net.netStatusText = '✗ APN 不能为空';
    return;
  }
  net.netStatusText = '⏳ 下发 APN 配置...';
  request(ACTIONS.SET_APN, payload.setApn(net.apnForm), { expect: TYPES.NETWORK_STATUS });
}

function applyNetworkMode() {
  net.netStatusText = '⏳ 切换选网模式...';
  request(ACTIONS.SET_NETWORK_MODE, payload.setNetworkMode(net.networkMode), { expect: TYPES.NETWORK_STATUS });
}

function setModePref(mode) {
  net.netStatusText = '⏳ 调整 5G 组网类型...';
  request(ACTIONS.SET_MODE_PREF, payload.setModePref(mode), { expect: TYPES.NETWORK_STATUS });
}

function connectNet(connect) {
  net.netStatusText = connect ? '⏳ 拨号中...' : '⏳ 断开拨号...';
  const act = connect ? ACTIONS.NET_CONNECT : ACTIONS.NET_DISCONNECT;
  const pl = connect ? payload.netConnect() : payload.netDisconnect();
  request(act, pl, { expect: TYPES.NETWORK_STATUS });
}

function setSimSlot(slot) {
  net.simSlotStatusText = `⏳ 正在切换至 SIM ${slot}...`;
  request(ACTIONS.SET_SIM_SLOT, payload.setSimSlot(slot), { expect: TYPES.SIM_SLOT_RES, intent: slot });
}

function toggleBand(b) {
  const list = net.bandRat === 'nr5g' ? net.selectedNr5gBands : net.selectedLteBands;
  const idx = list.indexOf(b);
  if (idx >= 0) list.splice(idx, 1);
  else list.push(b);
}

function isBandSelected(b) {
  const list = net.bandRat === 'nr5g' ? net.selectedNr5gBands : net.selectedLteBands;
  return list.includes(b);
}

function applyBandLock() {
  const bands = net.bandRat === 'nr5g' ? net.selectedNr5gBands : net.selectedLteBands;
  if (bands.length === 0) {
    net.bandLockStatusText = '✗ 请选择至少一个频段';
    return;
  }
  net.bandLockStatusText = '⏳ 频段锁定下发中...';
  request(ACTIONS.SET_BAND_LOCK, payload.setBandLock({ nr5g: net.bandRat === 'nr5g', bands: bands.join(':') }), {
    expect: TYPES.BAND_LOCK_RES,
    intent: { kind: 'apply', rat: net.bandRat },
    timeoutMs: 60000,
  });
}

function resetBandLock() {
  net.bandResetPending = true;
  net.bandLockStatusText = '⏳ 恢复出厂频段中...';
  request(ACTIONS.SET_BAND_LOCK, payload.setBandLock({ nr5g: net.bandRat === 'nr5g', bands: 'all' }), {
    expect: TYPES.BAND_LOCK_RES,
    intent: { kind: 'reset', rat: net.bandRat },
    timeoutMs: 60000,
  });
}

function applyCellLock() {
  if (!net.cellLock.pci || !net.cellLock.earfcn) {
    net.cellLock.statusText = '✗ 请填写 PCI 和 EARFCN';
    return;
  }
  net.cellLock.statusText = '⏳ 强绑定小区中...';
  request(ACTIONS.SET_CELL_LOCK, payload.setCellLock({ ...net.cellLock, enable: true }), {
    expect: TYPES.CELL_LOCK_RES,
  });
}

function unlockCell() {
  net.cellLock.statusText = '⏳ 解除小区绑定...';
  request(ACTIONS.SET_CELL_LOCK, payload.setCellLock({ tech: net.cellLock.tech, enable: false }), {
    expect: TYPES.CELL_LOCK_RES,
  });
}

function startNetworkScan() {
  if (net.scan.scanning) return;
  net.scan.scanning = true;
  net.scan.btnText = '正在扫描中...';
  net.scan.statusText = '射频深度扫描中（最长约 240 秒），请勿切网或断电。';
  ui.globalLoading = true;
  ui.loadingText = '正在执行基站深度扫频，请耐心等待...';
  request(ACTIONS.NETWORK_SCAN, payload.networkScan(), {
    expect: TYPES.SCAN_RESULT,
    timeoutMs: SCAN_TIMEOUT_MS,
  });
}

function runDiag(sub) {
  net.diag.isError = false;
  net.diag.outputText = '⏳ 底层查询中...';
  request(ACTIONS.GET_DIAGNOSTICS, payload.getDiagnostics(sub), { expect: TYPES.DIAGNOSTICS_RES });
}

function refreshMbnList() {
  net.mbn.statusText = '⏳ 正在拉取 MBN 列表...';
  request(ACTIONS.GET_MBN_LIST, payload.getMbnList(), { expect: TYPES.MBN_LIST_RES });
}

function applyMbn() {
  if (!net.mbn.selected) return;
  net.mbn.statusText = '⏳ 正在应用 MBN...';
  request(ACTIONS.SET_MBN, payload.setMbn(net.mbn.selected), { expect: TYPES.MBN_SET_RES });
}

function setMbnAutoSel(enable) {
  net.mbn.statusText = '⏳ 更新 MBN AutoSel...';
  request(ACTIONS.MBN_AUTOSEL, payload.mbnAutoSel(enable), { expect: TYPES.MBN_SET_RES });
}

function deactivateMbn() {
  if (!confirm('确认停用当前 MBN？')) return;
  net.mbn.statusText = '⏳ 停用当前 MBN...';
  request(ACTIONS.MBN_DEACTIVATE, payload.mbnDeactivate(), { expect: TYPES.MBN_SET_RES });
}

function applyEthConfig() {
  net.eth.statusText = '⏳ 下发网口配置...';
  request(ACTIONS.SET_ETH_CONFIG, payload.setEthConfig({ driver: net.eth.driver, pcie_rc: net.eth.pcieRc === '1' }), {
    expect: TYPES.ETH_RES,
  });
}

function applyIpptConfig() {
  net.ippt.statusText = '⏳ 下发直通配置...';
  request(ACTIONS.SET_IPPT_CONFIG, payload.setIpptConfig({ mode: net.ippt.mode }), { expect: TYPES.IPPT_RES });
}

function readImei() {
  net.imeiStatusText = '⏳ 读取当前 IMEI...';
  request(ACTIONS.READ_IMEI, payload.readImei(), { expect: TYPES.IMEI_RES, intent: 'read' });
}

function writeImei() {
  const imei = net.imeiInput.trim();
  if (!/^\d{15}$/.test(imei)) {
    net.imeiStatusText = '✗ 请输入 15 位数字 IMEI';
    return;
  }
  if (!confirm(`确认将 IMEI 写入为: ${imei} ?`)) return;
  net.imeiStatusText = '⏳ 写入 IMEI...';
  request(ACTIONS.WRITE_IMEI, payload.writeImei(imei), { expect: TYPES.IMEI_RES, intent: 'write' });
}

function rebootModule() {
  if (!confirm('确认重启模组？')) return;
  request(ACTIONS.REBOOT, payload.reboot(), { expect: TYPES.SETTINGS_LOG, timeoutMs: 60000 });
}

function factoryResetModule() {
  if (!confirm('警告：此操作将彻底重置模组配置为出厂状态！确认重置？')) return;
  request(ACTIONS.FACTORY_RESET, payload.factoryReset(), { expect: TYPES.SETTINGS_LOG, timeoutMs: 60000 });
}

function setFlightMode(on) {
  request(ACTIONS.FLIGHT_MODE, payload.flightMode(on), { expect: TYPES.SETTINGS_LOG });
}

function refreshSpecs() {
  send(ACTIONS.GET_DEVICE_INFO, payload.getDeviceInfo());
}

function updateRefreshRate() {
  telemetry.refreshRate = String(payload.setInterval(telemetry.refreshRate));
  send(ACTIONS.SET_INTERVAL, payload.setInterval(telemetry.refreshRate));
}

// ---- 短信 ----
function refreshSms() {
  sms.statusText = '⏳ 正在拉取短信...';
  request(ACTIONS.GET_SMS_LIST, payload.getSmsList(), { expect: TYPES.SMS_LIST });
}

function sendSmsMsg() {
  if (!sms.recipient || !sms.message) {
    sms.statusText = '✗ 请同时输入收信人和正文';
    return;
  }
  sms.statusText = '⏳ 正在发送短信...';
  request(ACTIONS.SEND_SMS, payload.sendSms(sms), { expect: TYPES.SMS_SENT, timeoutMs: 60000 });
}

// ---- USB ----
function refreshUsbConfig() {
  usb.statusText = '⏳ 查询 USB 配置...';
  request(ACTIONS.GET_USB_CONFIG, payload.getUsbConfig(), { expect: TYPES.USB_CONFIG_INFO });
}

function applyUsbMode() {
  usb.applying = true;
  usb.statusText = '⏳ 下发 USB 模式...';
  request(ACTIONS.SET_USB_NET_MODE, payload.setUsbNetMode(usb.mode), { expect: TYPES.USB_NET_RES, timeoutMs: 60000 });
}

// ---- 控制台与日志 ----
function sendManualAt(cmd) {
  const at = (cmd || logs.input).trim();
  if (!at) return;
  logs.history.push(`▶ ${at}`);
  send(ACTIONS.MANUAL_AT, payload.manualAt(at));
  logs.input = '';
  nextTick(() => {
    if (consoleOut.value) consoleOut.value.scrollTop = consoleOut.value.scrollHeight;
  });
}

function refreshBackendLogs() {
  request(ACTIONS.GET_BACKEND_LOG, payload.getBackendLog(), { expect: TYPES.BACKEND_LOG });
}

// ---- 生命周期挂载与路由绑定 ----
onMounted(() => {
  const saved = localStorage.getItem('theme');
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  ui.theme = saved || (prefersDark ? 'dark' : 'light');
  document.documentElement.setAttribute('data-theme', ui.theme);

  const HANDLERS = {
    [TYPES.STATIC_INFO]: (data) => {
      if (data?.firmware_version) ui.firmwareVersion = data.firmware_version;
      if (data?.active_sim) telemetry.active_sim = data.active_sim;
      if (data?.network_provider) telemetry.network_provider = data.network_provider;
      if (data?.apn) {
        telemetry.apn = data.apn;
        if (!net.apnForm.apn) net.apnForm.apn = data.apn;
      }
    },
    [TYPES.DEVICE_INFO]: (data) => {
      if (!data) return;
      Object.assign(telemetry.deviceSpecs, {
        manufacturer: data.manufacturer || '--',
        model: data.model || '--',
        firmwareVersion: data.firmware_version || '--',
        imei: data.imei || '--',
        serial: data.serial || '--',
        hwVersion: data.hw_version || '--',
        moduleType: data.module_type || '--',
        simStatus: data.sim_status || '--',
        imsi: data.imsi || '--',
        iccid: data.iccid || '--',
        phone: data.phone || '--',
        netStatus: data.net_status || '--',
        signal: data.signal || '--',
        temperature: data.temperature || '--',
        bands: data.bands || '--',
        maxRate: data.max_rate || '--',
        volte: data.volte || '--',
        gnss: data.gnss || '--',
      });
    },
    [TYPES.NETWORK_STATUS]: (data) => {
      net.netStatusText = data?.success ? `✓ ${data.msg || data.status || '成功'}` : `✗ ${data?.msg || '失败'}`;
    },
    [TYPES.SCAN_RESULT]: (data) => {
      net.scan.scanning = false;
      net.scan.btnText = '触发扫频';
      net.scan.networks = Array.isArray(data?.networks) ? data.networks : [];
      net.scan.statusText = data?.status || '扫描完成';
      ui.globalLoading = false;
    },
    [TYPES.SMS_LIST]: (data) => {
      sms.statusText = '';
      if (typeof data === 'string') sms.messages = parseCmgl(data);
      else if (Array.isArray(data)) sms.messages = data;
      else if (Array.isArray(data?.messages)) sms.messages = data.messages;
    },
    [TYPES.SMS_SENT]: (data) => {
      if (data?.status?.includes('successfully')) {
        sms.statusText = `✓ 短信发送成功至 ${data.recipient || ''}`;
        sms.message = '';
      } else {
        sms.statusText = `✗ ${data?.status || '发送失败'}`;
      }
    },
    [TYPES.BACKEND_LOG]: (data) => {
      logs.backend = Array.isArray(data) ? data : [];
      nextTick(() => {
        if (logOut.value) logOut.value.scrollTop = logOut.value.scrollHeight;
      });
    },
    [TYPES.SETTINGS_LOG]: (data) => {
      const msg = typeof data === 'string' ? data : data?.msg;
      if (msg) logs.backend.push(`[${new Date().toLocaleTimeString('zh-CN', { hour12: false })}] ${msg}`);
    },
    [TYPES.BAND_LOCK_RES]: (data, intent) => {
      net.bandLockStatusText = data?.success ? `✓ ${data.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;
      if (data?.success && intent?.kind === 'reset') {
        if (intent.rat === 'nr5g') net.selectedNr5gBands = [];
        else net.selectedLteBands = [];
      }
      net.bandResetPending = false;
    },
    [TYPES.CELL_LOCK_RES]: (data) => {
      net.cellLock.statusText = data?.success ? `✓ ${data.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;
    },
    [TYPES.DIAGNOSTICS_RES]: (data) => {
      if (data?.success) {
        const raw = data.data;
        net.diag.outputText = typeof raw === 'object' ? JSON.stringify(raw, null, 2) : String(raw || '');
        net.diag.isError = false;
      } else {
        net.diag.outputText = `ERROR: ${data?.msg || '未知错误'}`;
        net.diag.isError = true;
      }
    },
    [TYPES.USB_CONFIG_INFO]: (data) => {
      if (data?.success && data.config) {
        usb.mode = data.config.usbnet_mode;
        usb.current = {
          name: data.config.usbnet_supported ? MODE_NAMES[data.config.usbnet_mode] || '未知' : 'N/A',
          desc: data.config.usbnet_name || '--',
          num: String(data.config.usbnet_mode),
          updated: new Date().toLocaleTimeString('zh-CN', { hour12: false }),
        };
      }
    },
    [TYPES.USB_NET_RES]: (data) => {
      usb.applying = false;
      // note 由后端固定下发（“修改后需重启模组方可生效”），是用户判断后续动作的关键，
      // 不能丢；只在成功时拼接，失败时的 msg 已是错误原因。
      usb.statusText = data?.success
        ? `✓ ${data.msg || 'OK'}${data?.note ? `（${data.note}）` : ''}`
        : `✗ ${data?.msg || 'Failed'}`;
      if (data?.config) HANDLERS[TYPES.USB_CONFIG_INFO]({ success: true, config: data.config });
    },
    [TYPES.MBN_LIST_RES]: (data) => {
      if (data?.success && Array.isArray(data.list)) {
        net.mbn.list = data.list;
        const active = data.list.find((i) => i.state === 1);
        if (active) net.mbn.selected = String(active.name);
        net.mbn.statusText = `✓ 已载入 ${data.list.length} 个配置文件`;
      }
    },
    [TYPES.MBN_SET_RES]: (data) => {
      net.mbn.statusText = data?.success ? `✓ ${data.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;
    },
    [TYPES.IMEI_RES]: (data, intent) => {
      if (data?.success) {
        if (intent === 'write') net.imeiStatusText = '✓ 写入成功，需热重启生效';
        else {
          net.imeiInput = data.imei || '';
          net.imeiStatusText = `✓ 读取成功: ${data.imei || '--'}`;
        }
      } else {
        net.imeiStatusText = '✗ 操作失败';
      }
    },
    [TYPES.SIM_SLOT_RES]: (data) => {
      net.simSlotStatusText = data?.success ? `✓ ${data.note || '切换成功'}` : `✗ ${data?.msg || '切换失败'}`;
      if (data?.success) telemetry.active_sim = `SIM ${data.slot}`;
    },
    [TYPES.ETH_RES]: (data) => {
      net.eth.statusText = data?.success ? `✓ ${data.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;
    },
    [TYPES.IPPT_RES]: (data) => {
      net.ippt.statusText = data?.success ? `✓ ${data.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;
    },
    [TYPES.AT_RES]: (data) => {
      const text = parseAtResponse(data);
      if (text.includes('+CMGL:')) HANDLERS[TYPES.SMS_LIST](text);
      else {
        logs.history.push(text);
        nextTick(() => {
          if (consoleOut.value) consoleOut.value.scrollTop = consoleOut.value.scrollHeight;
        });
      }
    },
  };

  wsClient = createWs({
    onMessage(type, data, frame) {
      if (type === null) {
        if (frame?.update_type === UPDATE_TYPES.FULL || frame?.update_type === UPDATE_TYPES.DELTA) {
          mergeTelemetry(frame.data ?? frame);
        }
        return;
      }
      const record = wsClient.resolve(type);
      const handler = HANDLERS[type];
      if (handler) handler(data, record?.intent ?? null);
    },
    onState(connected, attempt) {
      wsState.connected = connected;
      wsState.reconnectAttempt = attempt;
      if (connected) {
        request(ACTIONS.GET_STATIC_INFO, payload.getStaticInfo(), { expect: TYPES.STATIC_INFO });
        refreshSpecs();
        refreshUsbConfig();
      }
    },
    onPendingChange(list) {
      wsState.pending = list;
    },
    onTimeout({ expect }) {
      ui.globalLoading = false;
      net.scan.scanning = false;
      net.scan.btnText = '触发扫频';
      logs.backend.push(`✗ [超时] ${expect} 未收到硬件回执`);
    },
  });

  wsClient.connect();

  document.addEventListener('visibilitychange', () => {
    send(ACTIONS.SET_VIEW_STATE, payload.setViewState(document.visibilityState === 'visible' ? 'active' : 'idle'));
  });
});
</script>

<template>
  <div class="flex h-screen overflow-hidden">
    <!-- 全局加载条 -->
    <div v-if="ui.globalLoading" class="toast toast-top toast-center z-50">
      <div class="alert alert-info py-2 px-4 shadow-lg text-xs font-semibold flex items-center gap-2">
        <span class="loading loading-spinner loading-xs"></span>
        <span>{{ ui.loadingText }}</span>
      </div>
    </div>

    <!-- 扫频全屏阻断 -->
    <div v-if="net.scan.scanning" class="modal modal-open z-40 bg-black/40 backdrop-blur-[2px]">
      <div class="modal-box text-center max-w-sm">
        <h3 class="font-bold text-lg">模组射频扫描中</h3>
        <p class="py-4 text-xs opacity-70">AT+COPS=? 最长耗时约 240 秒，期间硬件通道独占，请勿操作。</p>
        <span class="loading loading-dots loading-md text-primary"></span>
      </div>
    </div>

    <!-- 断网重连遮罩 -->
    <div v-if="!wsState.connected" class="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm flex items-center justify-center">
      <div class="card bg-base-100 p-6 shadow-2xl text-center space-y-3 max-w-xs">
        <span class="loading loading-spinner loading-lg text-primary mx-auto"></span>
        <p class="text-sm font-semibold">通信中断，正在尝试重连...</p>
      </div>
    </div>

    <!-- 左侧菜单 Sidebar -->
    <aside
      class="fixed md:static inset-y-0 left-0 w-64 bg-base-100 border-r border-base-300 flex flex-col justify-between z-30 transition-transform duration-200"
      :class="ui.mobileMenuOpen ? 'translate-x-0' : '-translate-x-full md:translate-x-0'"
    >
      <div>
        <div class="p-5 border-b border-base-300 flex items-center justify-between">
          <div class="flex items-center gap-2">
            <span class="text-2xl font-black tracking-wider text-primary">Argon</span>
            <span :class="badgeVariants({ intent: 'primary' })">RM520N</span>
          </div>
          <div
            class="w-2.5 h-2.5 rounded-full"
            :class="wsState.connected ? 'bg-success animate-pulse' : 'bg-error'"
          ></div>
        </div>

        <ul class="menu p-3 gap-1 text-sm font-medium">
          <li>
            <a :class="{ active: ui.currentTab === 'dashboard' }" @click="switchTab('dashboard')">
              仪表盘 Dashboard
            </a>
          </li>
          <li>
            <a :class="{ active: ui.currentTab === 'network' }" @click="switchTab('network')">
              蜂窝与射频 Network &amp; RF
            </a>
          </li>
          <li>
            <a :class="{ active: ui.currentTab === 'sms' }" @click="switchTab('sms')">
              短信中心 SMS
            </a>
          </li>
          <li>
            <a :class="{ active: ui.currentTab === 'usb' }" @click="switchTab('usb')">
              USB 配置
            </a>
          </li>
          <li>
            <a :class="{ active: ui.currentTab === 'logs' }" @click="switchTab('logs')">
              系统日志 Logs
            </a>
          </li>
        </ul>
      </div>

      <div class="p-4 border-t border-base-300 text-[10px] text-center opacity-60">
        固件版本: <span class="font-bold">{{ ui.firmwareVersion }}</span>
      </div>
    </aside>

    <div
      v-if="ui.mobileMenuOpen"
      @click="ui.mobileMenuOpen = false"
      class="fixed inset-0 bg-black/40 z-20 md:hidden"
    ></div>

    <!-- 右侧主体内容容器 -->
    <div class="flex-1 flex flex-col h-screen overflow-hidden">
      <!-- 顶栏 Header -->
      <header class="navbar bg-base-100 border-b border-base-300 px-4 sm:px-8 shrink-0 z-10 justify-between">
        <div class="flex items-center gap-2">
          <button
            type="button"
            class="btn btn-ghost btn-sm md:hidden"
            @click="ui.mobileMenuOpen = !ui.mobileMenuOpen"
          >
            ☰
          </button>
          <span class="text-xs font-bold tracking-widest uppercase opacity-70">
            {{ ui.currentTab.toUpperCase() }} CONTROL
          </span>
        </div>

        <div class="flex items-center gap-3">
          <span :class="badgeVariants({ intent: 'info' })">Auto Refresh On</span>
          <button type="button" class="btn btn-ghost btn-circle btn-sm" @click="toggleTheme" title="切换主题">
            <span v-if="ui.theme === 'dark'">🌙</span>
            <span v-else>☀️</span>
          </button>
          <button type="button" class="btn btn-ghost btn-circle btn-sm" @click="handleLogout" title="退出登录">
            🚪
          </button>
        </div>
      </header>

      <!-- 可滚动视口 -->
      <main class="flex-1 overflow-y-auto p-4 sm:p-6 space-y-6">

        <!-- ===================== 1. Dashboard ===================== -->
        <div v-show="ui.currentTab === 'dashboard'" class="space-y-6">
          <div class="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-3">
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">工作温度</span>
              <span class="text-xl font-black text-warning mt-2">{{ telemetry.temperature }}</span>
            </div>
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">SIM 状态</span>
              <span class="text-xl font-black text-primary mt-2">{{ telemetry.sim_status }}</span>
            </div>
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">信号质量</span>
              <span class="text-xl font-black text-info mt-2">{{ telemetry.signal_percentage }}</span>
            </div>
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">网络链路</span>
              <span class="text-xl font-black text-success mt-2">{{ telemetry.internet_connection }}</span>
            </div>
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">CPU 负载</span>
              <span class="text-xl font-black text-error mt-2">{{ telemetry.cpu_usage }}</span>
              <progress class="progress progress-error w-full mt-2" :value="telemetry.cpuPercent" max="100"></progress>
            </div>
            <div :class="cardVariants()" class="p-4 flex flex-col justify-between">
              <span class="text-[10px] font-bold uppercase opacity-60">内存占用</span>
              <span class="text-xl font-black text-secondary mt-2">{{ telemetry.memory_usage }}</span>
              <progress class="progress progress-secondary w-full mt-2" :value="telemetry.memoryPercent" max="100"></progress>
            </div>
          </div>

          <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <!-- 网络基础参数 -->
            <div :class="cardVariants()">
              <div class="p-4 border-b border-base-300 font-bold text-xs uppercase text-primary">网络架构参数</div>
              <div class="p-4 grid grid-cols-1 sm:grid-cols-2 gap-3 text-xs">
                <div class="bg-base-200 p-2.5 rounded-lg flex flex-col justify-between">
                  <div class="flex justify-between items-center">
                    <span class="opacity-60 text-[10px]">卡槽控制</span>
                    <span class="text-primary text-[10px]">{{ net.simSlotStatusText }}</span>
                  </div>
                  <div class="join w-full mt-2">
                    <button
                      type="button"
                      class="btn btn-xs join-item flex-1"
                      :class="telemetry.active_sim.includes('1') ? 'btn-primary' : 'btn-ghost'"
                      @click="setSimSlot(1)"
                    >
                      SIM 1
                    </button>
                    <button
                      type="button"
                      class="btn btn-xs join-item flex-1"
                      :class="telemetry.active_sim.includes('2') ? 'btn-primary' : 'btn-ghost'"
                      @click="setSimSlot(2)"
                    >
                      SIM 2
                    </button>
                  </div>
                </div>

                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.network_provider)">
                  <span class="opacity-60 text-[10px] block">运营商</span>
                  <span class="font-bold text-sm truncate block mt-1">{{ telemetry.network_provider }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.mccmnc)">
                  <span class="opacity-60 text-[10px] block">MCCMNC</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.mccmnc }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.apn)">
                  <span class="opacity-60 text-[10px] block">APN</span>
                  <span class="font-bold text-sm block mt-1 uppercase">{{ telemetry.apn }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.network_mode)">
                  <span class="opacity-60 text-[10px] block">网络模式</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.network_mode }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.bandwidth)">
                  <span class="opacity-60 text-[10px] block">总频宽</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.bandwidth }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg sm:col-span-2" @click="showCopy(telemetry.bands)">
                  <span class="opacity-60 text-[10px] block">当前频段</span>
                  <span class="font-bold text-sm block mt-1 truncate">{{ telemetry.bands }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.ipv4)">
                  <span class="opacity-60 text-[10px] block">IPv4</span>
                  <span class="font-bold text-sm text-primary block mt-1">{{ telemetry.ipv4 }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg" @click="showCopy(telemetry.uptime)">
                  <span class="opacity-60 text-[10px] block">系统运行时间</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.uptime }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg sm:col-span-2" @click="showCopy(telemetry.ipv6)">
                  <span class="opacity-60 text-[10px] block">IPv6</span>
                  <span class="font-bold text-xs text-primary block mt-1 truncate">{{ telemetry.ipv6 }}</span>
                </div>
              </div>

              <div class="p-3 border-t border-base-300 flex justify-between items-center text-xs">
                <span class="opacity-60">轮询频率</span>
                <div class="flex gap-2">
                  <select v-model="telemetry.refreshRate" class="select select-bordered select-xs">
                    <option value="3">3 秒</option>
                    <option value="5">5 秒</option>
                    <option value="10">10 秒</option>
                  </select>
                  <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="updateRefreshRate">
                    更新
                  </button>
                </div>
              </div>
            </div>

            <!-- 空口指标 -->
            <div :class="cardVariants()">
              <div class="p-4 border-b border-base-300 font-bold text-xs uppercase text-primary">物理层空口指标</div>
              <div class="p-4 grid grid-cols-2 gap-3 text-xs">
                <div class="bg-base-200 p-2.5 rounded-lg">
                  <span class="opacity-60 text-[10px] block">空口评估</span>
                  <span class="font-bold text-sm text-primary block mt-1">{{ telemetry.assessment }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg">
                  <span class="opacity-60 text-[10px] block">累计流量</span>
                  <span class="font-bold text-sm block mt-1 truncate">{{ telemetry.traffic_stats }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg">
                  <span class="opacity-60 text-[10px] block">CELL ID</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.cell_id }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg">
                  <span class="opacity-60 text-[10px] block">eNB ID</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.enb_id }}</span>
                </div>
                <div class="bg-base-200 p-2.5 rounded-lg col-span-2">
                  <span class="opacity-60 text-[10px] block">TAC 追踪区编码</span>
                  <span class="font-bold text-sm block mt-1">{{ telemetry.tac }}</span>
                </div>
              </div>

              <div class="px-4 pb-4 space-y-3 text-xs border-t border-base-300 pt-3">
                <div>
                  <div class="flex justify-between font-bold">
                    <span>RSRQ (信噪比质量)</span>
                    <span>{{ telemetry.ss_rsrq }}</span>
                  </div>
                  <progress class="progress progress-primary w-full mt-1" :value="telemetry.rsrqPercent" max="100"></progress>
                </div>
                <div>
                  <div class="flex justify-between font-bold">
                    <span>RSRP (参考信号接收功率)</span>
                    <span>{{ telemetry.ss_rsrp }}</span>
                  </div>
                  <progress class="progress progress-info w-full mt-1" :value="telemetry.rsrpPercent" max="100"></progress>
                </div>
                <div>
                  <div class="flex justify-between font-bold">
                    <span>SINR (信号干扰比)</span>
                    <span>{{ telemetry.sinr }}</span>
                  </div>
                  <progress class="progress progress-success w-full mt-1" :value="telemetry.sinrPercent" max="100"></progress>
                </div>
              </div>
            </div>
          </div>

          <!-- 硬件资产折叠抽屉 -->
          <div class="collapse collapse-arrow bg-base-100 border border-base-300 rounded-2xl shadow-sm">
            <input type="checkbox" />
            <div class="collapse-title text-xs font-bold uppercase text-primary">
              硬件底层资产规格 &amp; 射频能力 (点击展开)
            </div>
            <div class="collapse-content text-xs space-y-3">
              <div class="grid grid-cols-2 sm:grid-cols-4 gap-3 pt-2">
                <div v-for="(val, key) in telemetry.deviceSpecs" :key="key" class="bg-base-200 p-2.5 rounded-lg">
                  <span class="opacity-60 text-[10px] block uppercase">{{ key }}</span>
                  <span class="font-bold truncate block mt-0.5" @click="showCopy(val)">{{ val }}</span>
                </div>
              </div>
              <div class="flex justify-end pt-2">
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="refreshSpecs">
                  拉取硬件规格
                </button>
              </div>
            </div>
          </div>
        </div>

        <!-- ===================== 2. Network & RF ===================== -->
        <div v-show="ui.currentTab === 'network'" class="space-y-6">
          <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <!-- APN 与拨号 -->
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">APN 拨号接入配置</div>
              <select v-model="net.apnPreset" @change="onApnPresetChange" class="select select-bordered select-sm w-full">
                <option value="custom">自定义输入 (Custom)...</option>
                <option value="3gnet">中国联通 - 3gnet</option>
                <option value="wonet">中国联通 - wonet (5G)</option>
                <option value="ctnet">中国电信 - ctnet</option>
                <option value="ctlte">中国电信 - ctlte</option>
                <option value="cmnet">中国移动 - cmnet</option>
                <option value="cmiot">中国移动 - cmiot</option>
                <option value="cbnet">中国广电 - cbnet</option>
                <option value="ims">IMS 专网 - ims</option>
              </select>
              <input v-model="net.apnForm.apn" placeholder="APN 接入点 (如 3gnet)" :class="inputVariants()" />
              <input v-model="net.apnForm.user" placeholder="用户名（可选）" :class="inputVariants()" />
              <input v-model="net.apnForm.pass" type="password" placeholder="密码（可选）" :class="inputVariants()" />
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm', block: true })" @click="applyApn">
                保存并应用 APN
              </button>

              <div class="divider my-1 text-[10px] opacity-40">拨号控制</div>
              <div class="flex gap-2">
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm' })" class="flex-1" @click="connectNet(true)">
                  建立拨号连接
                </button>
                <button type="button" :class="btnVariants({ intent: 'error', size: 'sm' })" class="flex-1" @click="connectNet(false)">
                  断开拨号连接
                </button>
              </div>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.netStatusText }}</div>
            </div>

            <!-- 射频选网与 5G 组网类型 -->
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">选网模式 &amp; 5G 偏好</div>
              <select v-model="net.networkMode" class="select select-bordered select-sm w-full">
                <option value="auto">全自动搜网模式 (AUTO)</option>
                <option value="nr5g">纯 5G 锁定 (NR5G Only)</option>
                <option value="lte">纯 4G 锁定 (LTE Only)</option>
                <option value="nr5g_lte">5G NR + LTE 协同模式</option>
                <option value="wcdma">3G WCDMA 留底</option>
              </select>
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm', block: true })" @click="applyNetworkMode">
                更新选网模式
              </button>

              <div class="divider my-1 text-[10px] opacity-40">5G 架构控制</div>
              <div class="grid grid-cols-3 gap-2">
                <button type="button" :class="btnVariants({ intent: 'warning', size: 'xs' })" @click="setModePref('disable_sa')">
                  禁用 SA (NSA)
                </button>
                <button type="button" :class="btnVariants({ intent: 'warning', size: 'xs' })" @click="setModePref('disable_nsa')">
                  禁用 NSA (SA)
                </button>
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="setModePref('enable_all_5g')">
                  开启双模
                </button>
              </div>
            </div>
          </div>

          <!-- M.2 转网口 / IP 直通 / IMEI -->
          <div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">M.2 转以太网口</div>
              <select v-model="net.eth.driver" class="select select-bordered select-xs w-full">
                <option value="r8125">Realtek RTL8125 (2.5G 推荐)</option>
                <option value="r8168">Realtek RTL8168/8111 (千兆)</option>
                <option value="aqc107">Aquantia AQC107 (万兆)</option>
              </select>
              <select v-model="net.eth.pcieRc" class="select select-bordered select-xs w-full">
                <option value="1">RC 主机模式</option>
                <option value="0">EP 从机直通模式</option>
              </select>
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm', block: true })" @click="applyEthConfig">
                应用网口配置
              </button>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.eth.statusText }}</div>
            </div>

            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">IP Passthrough (直通)</div>
              <select v-model="net.ippt.mode" class="select select-bordered select-xs w-full">
                <option value="dmz">DMZ 准直通模式</option>
                <option value="nat">标准 NAT 路由模式</option>
              </select>
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm', block: true })" @click="applyIpptConfig">
                应用直通配置
              </button>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.ippt.statusText }}</div>
            </div>

            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">IMEI 读取与修复</div>
              <input v-model="net.imeiInput" maxlength="15" placeholder="15 位 IMEI" :class="inputVariants({ size: 'sm' })" />
              <div class="flex gap-2">
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" class="flex-1" @click="readImei">读取当前</button>
                <button type="button" :class="btnVariants({ intent: 'warning', size: 'xs' })" class="flex-1" @click="writeImei">写入修复</button>
              </div>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.imeiStatusText }}</div>
            </div>
          </div>

          <!-- 频段锁定与物理小区绑定 -->
          <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">频段强制锁定 (Band Lock)</div>
              <div class="flex gap-4 text-xs font-semibold">
                <label class="flex items-center gap-1.5 cursor-pointer">
                  <input type="radio" value="nr5g" v-model="net.bandRat" class="radio radio-primary radio-xs" />
                  <span>5G (NR5G)</span>
                </label>
                <label class="flex items-center gap-1.5 cursor-pointer">
                  <input type="radio" value="lte" v-model="net.bandRat" class="radio radio-primary radio-xs" />
                  <span>4G (LTE)</span>
                </label>
              </div>

              <div class="flex flex-wrap gap-1.5 pt-2">
                <template v-if="net.bandRat === 'nr5g'">
                  <button
                    v-for="b in NR5G_BANDS"
                    :key="b"
                    type="button"
                    class="btn btn-xs"
                    :class="isBandSelected(b) ? 'btn-primary' : 'btn-outline'"
                    @click="toggleBand(b)"
                  >
                    n{{ b }}
                  </button>
                </template>
                <template v-else>
                  <button
                    v-for="b in LTE_BANDS"
                    :key="b"
                    type="button"
                    class="btn btn-xs"
                    :class="isBandSelected(b) ? 'btn-primary' : 'btn-outline'"
                    @click="toggleBand(b)"
                  >
                    B{{ b }}
                  </button>
                </template>
              </div>

              <div class="bg-base-200 p-2 rounded text-xs flex justify-between items-center font-mono">
                <span class="opacity-60">锁定编译</span>
                <span class="font-bold text-primary">{{ bandLockPreview }}</span>
              </div>

              <div class="flex gap-2">
                <button type="button" :disabled="net.bandResetPending" :class="btnVariants({ intent: 'primary', size: 'sm' })" class="flex-1" @click="applyBandLock">
                  应用频段锁定
                </button>
                <button type="button" :disabled="net.bandResetPending" :class="btnVariants({ intent: 'error', size: 'sm' })" class="flex-1" @click="resetBandLock">
                  恢复默认全频段
                </button>
              </div>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.bandLockStatusText }}</div>
            </div>

            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">物理小区锁定 (Cell Lock)</div>
              <div class="flex gap-4 text-xs font-semibold">
                <label class="flex items-center gap-1.5 cursor-pointer">
                  <input type="radio" value="lte" v-model="net.cellLock.tech" class="radio radio-primary radio-xs" />
                  <span>4G (LTE)</span>
                </label>
                <label class="flex items-center gap-1.5 cursor-pointer">
                  <input type="radio" value="5g" v-model="net.cellLock.tech" class="radio radio-primary radio-xs" />
                  <span>5G (NR)</span>
                </label>
              </div>
              <div class="grid grid-cols-2 gap-2">
                <input v-model="net.cellLock.pci" type="number" placeholder="PCI" :class="inputVariants({ size: 'sm' })" />
                <input v-model="net.cellLock.earfcn" type="number" placeholder="EARFCN" :class="inputVariants({ size: 'sm' })" />
              </div>
              <input v-model="net.cellLock.band" type="number" placeholder="Band (5G必填)" :class="inputVariants({ size: 'sm' })" />
              <div class="flex gap-2">
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm' })" class="flex-1" @click="applyCellLock">绑定小区</button>
                <button type="button" :class="btnVariants({ intent: 'error', size: 'sm' })" class="flex-1" @click="unlockCell">解除绑定</button>
              </div>
              <div class="text-[11px] font-mono min-h-[16px]">{{ net.cellLock.statusText }}</div>
            </div>
          </div>

          <!-- 扫频基站扫描 -->
          <div :class="cardVariants()">
            <div class="p-4 border-b border-base-300 flex justify-between items-center">
              <span class="font-bold text-xs uppercase text-primary">空中基站深度扫频扫描</span>
              <button type="button" :disabled="net.scan.scanning" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="startNetworkScan">
                {{ net.scan.btnText }}
              </button>
            </div>
            <div class="px-4 py-2 text-xs bg-base-200 border-b border-base-300 opacity-70">{{ net.scan.statusText }}</div>
            <div class="overflow-x-auto">
              <table class="table table-zebra table-sm w-full">
                <thead>
                  <tr class="text-xs">
                    <th>#</th>
                    <th>运营商</th>
                    <th>MCC/MNC</th>
                    <th>技术</th>
                    <th>状态</th>
                    <th>频段</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="(item, idx) in net.scan.networks" :key="idx">
                    <td>{{ idx + 1 }}</td>
                    <td class="font-bold">{{ item.operator || '--' }}</td>
                    <td>{{ item.mccmnc || '--' }}</td>
                    <td><span :class="badgeVariants({ intent: item.technology === 'NR5G' ? 'primary' : 'info' })">{{ item.technology }}</span></td>
                    <td>{{ item.status }}</td>
                    <td>{{ item.band || '--' }}</td>
                  </tr>
                  <tr v-if="net.scan.networks.length === 0">
                    <td colspan="6" class="text-center py-6 opacity-40">暂无扫频历史。</td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>

          <!-- 诊断与 MBN -->
          <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">底层诊断与 MBN</div>
              <div class="flex flex-wrap gap-1.5">
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="runDiag('neighbour')">邻区侦测</button>
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="runDiag('qlts')">时钟同步</button>
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="refreshMbnList">刷新 MBN</button>
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="setMbnAutoSel(true)">启用 AutoSel</button>
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="setMbnAutoSel(false)">禁用 AutoSel</button>
                <button type="button" :class="btnVariants({ intent: 'warning', size: 'xs' })" @click="deactivateMbn">停用 MBN</button>
              </div>
              <div class="flex gap-2">
                <select v-model="net.mbn.selected" class="select select-bordered select-xs flex-1">
                  <option value="">选择 MBN...</option>
                  <option v-for="m in net.mbn.list" :key="m.name" :value="m.name">
                    {{ m.name }}{{ m.state === 1 ? ' (当前)' : '' }}
                  </option>
                </select>
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="applyMbn">应用 MBN</button>
              </div>
              <pre class="terminal-box max-h-48 overflow-y-auto" :class="{ 'terminal-box-error': net.diag.isError }">{{ net.diag.outputText }}</pre>
            </div>

            <!-- 复位控制 -->
            <div :class="cardVariants()" class="p-4 space-y-3">
              <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">硬件复位与飞行模式</div>
              <div class="flex gap-2">
                <button type="button" :class="btnVariants({ intent: 'warning', size: 'sm' })" class="flex-1" @click="rebootModule">
                  热重启模组 (Reboot)
                </button>
                <button type="button" :class="btnVariants({ intent: 'error', size: 'sm' })" class="flex-1" @click="factoryResetModule">
                  恢复出厂设置 (Reset)
                </button>
              </div>
              <div class="flex gap-2">
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'sm' })" class="flex-1" @click="setFlightMode(true)">
                  开启飞行模式
                </button>
                <button type="button" :class="btnVariants({ intent: 'neutral', size: 'sm' })" class="flex-1" @click="setFlightMode(false)">
                  关闭飞行模式
                </button>
              </div>
            </div>
          </div>

          <!-- AT 交互终端 -->
          <div :class="cardVariants()" class="p-4 space-y-3">
            <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">AT 命令交互控制台</div>
            <div ref="consoleOut" class="terminal-box h-48 overflow-y-auto space-y-0.5">
              <div v-for="(line, idx) in logs.history" :key="idx">{{ line }}</div>
            </div>
            <div class="flex gap-2">
              <input
                v-model="logs.input"
                @keydown.enter="sendManualAt()"
                placeholder="AT+CGMI"
                :class="inputVariants({ size: 'sm' })"
              />
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm' })" @click="sendManualAt()">
                发送指令
              </button>
            </div>
            <div class="flex flex-wrap gap-1.5 pt-1 text-xs">
              <button type="button" class="btn btn-ghost btn-xs border border-base-300" @click="sendManualAt('AT+CSQ')">信号 CSQ</button>
              <button type="button" class="btn btn-ghost btn-xs border border-base-300" @click="sendManualAt('AT+COPS?')">运营商 COPS?</button>
              <button type="button" class="btn btn-ghost btn-xs border border-base-300" @click="sendManualAt('AT+CGPADDR')">IP CGPADDR</button>
              <button type="button" class="btn btn-ghost btn-xs border border-base-300" @click="sendManualAt('AT+QENG=&quot;servingcell&quot;')">服务小区 QENG</button>
              <button type="button" class="btn btn-ghost btn-xs border border-base-300" @click="sendManualAt('AT+CFUN=1,1')">重启 CFUN=1,1</button>
            </div>
          </div>
        </div>

        <!-- ===================== 3. SMS ===================== -->
        <div v-show="ui.currentTab === 'sms'" class="space-y-6">
          <div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
            <!-- 收件箱 -->
            <div :class="cardVariants()" class="p-4 space-y-3 lg:col-span-1">
              <div class="flex justify-between items-center border-b border-base-300 pb-2">
                <span class="font-bold text-xs uppercase text-primary">短信收件箱</span>
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="refreshSms">读取列表</button>
              </div>
              <div class="divide-y divide-base-300 max-h-96 overflow-y-auto">
                <div
                  v-for="(msg, idx) in sms.messages"
                  :key="idx"
                  class="p-2 hover:bg-base-200 cursor-pointer rounded transition-colors text-xs"
                  @click="sms.selectedMsg = msg"
                >
                  <div class="flex justify-between font-bold">
                    <span>{{ msg.sender }}</span>
                    <span class="text-[10px] opacity-60">{{ msg.timestamp }}</span>
                  </div>
                  <div class="truncate opacity-70 mt-1">{{ msg.text }}</div>
                </div>
                <div v-if="sms.messages.length === 0" class="p-4 text-center text-xs opacity-50">卡内暂无短信。</div>
              </div>
            </div>

            <!-- 阅读器与发送 -->
            <div class="space-y-6 lg:col-span-2">
              <div v-if="sms.selectedMsg" :class="cardVariants()" class="p-4 space-y-3">
                <div class="flex justify-between items-center border-b border-base-300 pb-2">
                  <span class="font-bold text-xs uppercase text-primary">短信详情</span>
                  <button type="button" class="btn btn-ghost btn-xs" @click="sms.selectedMsg = null">关闭</button>
                </div>
                <div class="text-xs flex justify-between opacity-70">
                  <span>发信人: <strong>{{ sms.selectedMsg.sender }}</strong></span>
                  <span>{{ sms.selectedMsg.timestamp }}</span>
                </div>
                <div class="bg-base-200 p-3 rounded-lg text-sm whitespace-pre-wrap">{{ sms.selectedMsg.text }}</div>
                <div class="flex justify-end gap-2">
                  <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="sms.recipient = sms.selectedMsg.sender">
                    快捷回复
                  </button>
                  <button type="button" :class="btnVariants({ intent: 'neutral', size: 'xs' })" @click="showCopy(sms.selectedMsg.text)">
                    复制正文
                  </button>
                </div>
              </div>

              <!-- 发送引擎 -->
              <div :class="cardVariants()" class="p-4 space-y-3">
                <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">发信引擎</div>
                <input v-model="sms.recipient" placeholder="收件人号码 (+86...)" :class="inputVariants({ size: 'sm' })" />
                <textarea
                  v-model="sms.message"
                  rows="3"
                  maxlength="160"
                  placeholder="短信正文..."
                  class="textarea textarea-bordered w-full text-xs"
                ></textarea>
                <div class="flex justify-between items-center text-[10px] opacity-60">
                  <span>字数: {{ sms.message.length }} / 160</span>
                </div>
                <button type="button" :class="btnVariants({ intent: 'primary', size: 'sm', block: true })" @click="sendSmsMsg">
                  立刻发送
                </button>
                <div class="text-[11px] font-mono min-h-[16px]">{{ sms.statusText }}</div>
              </div>
            </div>
          </div>
        </div>

        <!-- ===================== 4. USB ===================== -->
        <div v-show="ui.currentTab === 'usb'" class="space-y-6">
          <div :class="cardVariants()" class="p-4 space-y-3">
            <div class="font-bold text-xs uppercase text-primary border-b border-base-300 pb-2">USB 网络模式控制</div>
            <div class="grid grid-cols-2 sm:grid-cols-3 gap-2">
              <button
                v-for="m in USB_MODES"
                :key="m.value"
                type="button"
                class="btn btn-sm flex flex-col items-center justify-center h-auto py-2"
                :class="usb.mode === m.value ? 'btn-primary' : 'btn-outline'"
                @click="usb.mode = m.value"
              >
                <span class="font-bold text-xs">{{ m.name }}</span>
                <span class="text-[9px] opacity-70">{{ m.desc }}</span>
              </button>
            </div>
            <button
              type="button"
              :disabled="usb.applying"
              :class="btnVariants({ intent: 'primary', size: 'sm', block: true })"
              @click="applyUsbMode"
            >
              应用 USB 模式并提示重启
            </button>
            <div class="text-[11px] font-mono min-h-[16px]">{{ usb.statusText }}</div>
          </div>

          <div :class="cardVariants()" class="p-4 space-y-3">
            <div class="flex justify-between items-center border-b border-base-300 pb-2">
              <span class="font-bold text-xs uppercase text-primary">当前 USB 配置</span>
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="refreshUsbConfig">
                刷新查询
              </button>
            </div>
            <div class="grid grid-cols-3 gap-3 text-center text-xs">
              <div class="bg-base-200 p-3 rounded-xl">
                <span class="opacity-60 text-[10px] block">当前协议</span>
                <span class="font-bold text-primary text-sm mt-1 block">{{ usb.current.name }}</span>
              </div>
              <div class="bg-base-200 p-3 rounded-xl">
                <span class="opacity-60 text-[10px] block">数值代号</span>
                <span class="font-bold text-sm mt-1 block font-mono">{{ usb.current.num }}</span>
              </div>
              <div class="bg-base-200 p-3 rounded-xl">
                <span class="opacity-60 text-[10px] block">更新时间</span>
                <span class="font-bold text-sm mt-1 block font-mono">{{ usb.current.updated }}</span>
              </div>
            </div>
          </div>
        </div>

        <!-- ===================== 5. Logs ===================== -->
        <div v-show="ui.currentTab === 'logs'" class="space-y-6">
          <div :class="cardVariants()" class="p-4 space-y-3">
            <div class="flex justify-between items-center border-b border-base-300 pb-2">
              <span class="font-bold text-xs uppercase text-primary">系统内存环形日志</span>
              <button type="button" :class="btnVariants({ intent: 'primary', size: 'xs' })" @click="refreshBackendLogs">
                手动刷新
              </button>
            </div>
            <div ref="logOut" class="terminal-box h-96 overflow-y-auto space-y-1">
              <div
                v-for="(line, idx) in logs.backend"
                :key="idx"
                :class="{
                  'text-error': line.includes('[ERROR]'),
                  'text-warning': line.includes('[WARN]'),
                  'text-info': line.includes('[INFO]'),
                }"
              >
                {{ line }}
              </div>
              <div v-if="logs.backend.length === 0" class="opacity-40">暂无日志或正在加载中...</div>
            </div>
          </div>
        </div>

      </main>
    </div>

    <!-- 复制提示气泡 -->
    <div v-if="ui.copyToast.show" class="toast toast-bottom toast-center z-50">
      <div :class="ui.copyToast.ok ? 'alert-success' : 'alert-error'"
        class="alert py-2 px-4 shadow text-xs font-semibold">
        <span v-if="ui.copyToast.ok">已复制: {{ ui.copyToast.text }}</span>
        <span v-else>{{ ui.copyToast.text }}</span>
      </div>
    </div>
  </div>
</template>
