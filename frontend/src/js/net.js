/**
 * 蜂窝与射频 store：APN / 选网 / SA-NSA / 网口 / 直通 / IMEI / 频段锁 / 小区锁 /
 * 扫频 / 诊断 / MBN / 卡槽 / 复位。
 *
 * 反假成功约定（与后端真实回执严格对应）：
 *   - 所有状态栏文案只在收到后端结构化回执（或超时）后才落定；
 *   - 失败与超时一律保留用户输入与选中态，不做乐观清空；
 *   - 只有后端确认成功后才执行"清空/高亮"这类破坏性 UI 更新。
 */
import { ACTIONS, TYPES, DIAG, payload } from './contract.js';

export const NR5G_BANDS = ['1', '3', '5', '7', '8', '20', '28', '38', '41', '71', '77', '78', '79'];
export const LTE_BANDS = ['1', '3', '5', '7', '8', '20', '28', '34', '38', '39', '40', '41'];

export const APN_PRESETS = [
  'ctnet',
  'ctlte',
  '3gnet',
  'wonet',
  'cmnet',
  'cmiot',
  'cbnet',
  'ims',
];

const TIMEOUT_TEXT = '✗ 操作超时（后端未返回）';

function verdict(data, fallback = '操作失败') {
  const ok = data?.success === true;
  const detail = data?.msg || data?.status || fallback;
  // 后端只在确实需要重启/额外操作时才带 note，不要自行补默认提示
  const note = data?.note ? `（${data.note}）` : '';
  return ok ? `✓ ${detail}${note}` : `✗ ${detail}`;
}

export function createNetStore({ request, send, isPending, ui }) {
  return {
    // ---- APN ----
    apnPreset: 'custom',
    apnForm: { apn: '', user: '', pass: '', auth: '0' },

    // ---- 选网 / 拨号 ----
    networkMode: 'auto',
    netStatusText: '',

    // ---- 网口 / 直通 ----
    eth: { driver: 'r8125', pcieRc: '1', statusText: '' },
    ippt: { mode: 'dmz', statusText: '' },

    // ---- IMEI ----
    imeiInput: '',
    imeiStatusText: '',

    // ---- 频段锁 ----
    bandRat: 'nr5g',
    selectedNr5gBands: [],
    selectedLteBands: [],
    bandLockStatusText: '锁定反馈：就绪',
    bandResetPending: false,
    nr5gBands: NR5G_BANDS,
    lteBands: LTE_BANDS,

    // ---- 小区锁 ----
    cellLock: { tech: 'lte', pci: '', earfcn: '', band: '', statusText: '小区反馈：就绪' },

    // ---- 扫频 ----
    scan: {
      scanning: false,
      btnText: '触发扫频 (15s+)',
      statusText: '等待搜网。请确保射频已经开启，搜网操作大约会中断空口连接 15-30 秒。',
      networks: [],
    },

    // ---- 诊断 / MBN ----
    diag: { outputText: '点击按钮查询并触发高级诊断交互。', isError: false },
    mbn: { list: [], selected: '', statusText: '' },

    // ---- SIM 卡槽 ----
    simSlotStatusText: '',

    get bandLockPreview() {
      const bands = this.bandRat === 'nr5g' ? this.selectedNr5gBands : this.selectedLteBands;
      const prefix = this.bandRat === 'nr5g' ? 'n' : 'B';
      return bands.length > 0
        ? bands.map((b) => prefix + b).join(':')
        : 'all (默认出厂全频段)';
    },

    get activeBands() {
      return this.bandRat === 'nr5g' ? this.selectedNr5gBands : this.selectedLteBands;
    },

    busy(action) {
      return isPending(action);
    },

    // -----------------------------------------------------------------------
    // 出站动作
    // -----------------------------------------------------------------------

    onApnPresetChange() {
      this.apnForm.apn = this.apnPreset === 'custom' ? '' : this.apnPreset;
    },

    /**
     * `static_info` 带回当前生效 APN 时回填表单，省去用户手动重选。
     * 只在用户尚未输入时回填，避免覆盖正在编辑的内容。
     */
    applyStaticApn(apn) {
      if (!apn || apn === 'N/A' || apn === '--' || this.apnForm.apn) return;
      this.apnForm.apn = apn;
      this.apnPreset = APN_PRESETS.includes(String(apn).toLowerCase())
        ? String(apn).toLowerCase()
        : 'custom';
    },

    applyApn() {
      if (!this.apnForm.apn) {
        this.netStatusText = '✗ APN 接入点不能为空';
        return;
      }
      if (!request(ACTIONS.SET_APN, payload.setApn(this.apnForm), {
        expect: TYPES.NETWORK_STATUS,
        intent: 'apn',
      }).ok) {
        this.netStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.netStatusText = '⏳ 正在下发 APN 配置...';
    },

    applyNetworkMode() {
      if (!request(ACTIONS.SET_NETWORK_MODE, payload.setNetworkMode(this.networkMode), {
        expect: TYPES.NETWORK_STATUS,
        intent: 'network_mode',
      }).ok) {
        this.netStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.netStatusText = '⏳ 正在更新选网偏好...';
    },

    setModePref(mode) {
      if (!request(ACTIONS.SET_MODE_PREF, payload.setModePref(mode), {
        expect: TYPES.NETWORK_STATUS,
        intent: 'mode_pref',
      }).ok) {
        this.netStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.netStatusText = '⏳ 正在重分配 5G 组网类型...';
    },

    connectNetwork() {
      if (!request(ACTIONS.NET_CONNECT, payload.netConnect(), {
        expect: TYPES.NETWORK_STATUS,
        intent: 'dial',
      }).ok) {
        this.netStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.netStatusText = '⏳ 正在建立拨号连接...';
    },

    disconnectNetwork() {
      if (!request(ACTIONS.NET_DISCONNECT, payload.netDisconnect(), {
        expect: TYPES.NETWORK_STATUS,
        intent: 'dial',
      }).ok) {
        this.netStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.netStatusText = '⏳ 正在断开拨号连接...';
    },

    setSimSlot(slot) {
      if (!request(ACTIONS.SET_SIM_SLOT, payload.setSimSlot(slot), {
        expect: TYPES.SIM_SLOT_RES,
        intent: slot,
      }).ok) {
        this.simSlotStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.simSlotStatusText = `⏳ 正在切换到 SIM ${slot}...`;
    },

    applyEthConfig() {
      if (!request(ACTIONS.SET_ETH_CONFIG, payload.setEthConfig({
        driver: this.eth.driver,
        pcie_rc: this.eth.pcieRc === '1',
      }), { expect: TYPES.ETH_RES }).ok) {
        this.eth.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.eth.statusText = '⏳ 正在下发网口配置...';
    },

    applyIpptConfig() {
      if (!request(ACTIONS.SET_IPPT_CONFIG, payload.setIpptConfig({ mode: this.ippt.mode }), {
        expect: TYPES.IPPT_RES,
      }).ok) {
        this.ippt.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.ippt.statusText = '⏳ 正在下发直通配置...';
    },

    readImei() {
      if (!request(ACTIONS.READ_IMEI, payload.readImei(), {
        expect: TYPES.IMEI_RES,
        intent: 'read',
      }).ok) {
        this.imeiStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.imeiStatusText = '⏳ 正在读取 IMEI...';
    },

    writeImei() {
      const imei = this.imeiInput.trim();
      if (!/^\d{15}$/.test(imei)) {
        this.imeiStatusText = '✗ 请输入 15 位纯数字的标准 IMEI';
        return;
      }
      if (!window.confirm(`确认将模组 IMEI 修改为: ${imei} ?`)) return;

      if (!request(ACTIONS.WRITE_IMEI, payload.writeImei(imei), {
        expect: TYPES.IMEI_RES,
        intent: 'write',
      }).ok) {
        this.imeiStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.imeiStatusText = '⏳ 正在写入 IMEI...';
    },

    toggleBand(band) {
      const list = this.activeBands;
      const idx = list.indexOf(band);
      if (idx >= 0) list.splice(idx, 1);
      else list.push(band);
    },

    isBandSelected(band) {
      return this.activeBands.includes(band);
    },

    applyBandLock() {
      const rat = this.bandRat;
      const bands = this.activeBands;
      if (bands.length === 0) {
        this.bandLockStatusText = '✗ 请先选择至少一个频段（如需全频段请用"恢复所有出厂频段"）';
        return;
      }
      if (!request(ACTIONS.SET_BAND_LOCK, payload.setBandLock({
        nr5g: rat === 'nr5g',
        bands: bands.join(':'),
      }), { expect: TYPES.BAND_LOCK_RES, intent: { kind: 'apply', rat }, timeoutMs: 60000 }).ok) {
        this.bandLockStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.bandLockStatusText = '⏳ 正在锁定重载中...';
    },

    resetBandLock() {
      const rat = this.bandRat;
      // 注意：此处不清空本地选中态。必须等后端确认成功后再清，
      // 否则下发失败时 UI 会与模组实际锁频状态脱节。
      this.bandResetPending = true;
      if (!request(ACTIONS.SET_BAND_LOCK, payload.setBandLock({ nr5g: rat === 'nr5g', bands: 'all' }), {
        expect: TYPES.BAND_LOCK_RES,
        intent: { kind: 'reset', rat },
        timeoutMs: 60000,
      }).ok) {
        this.bandResetPending = false;
        this.bandLockStatusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.bandLockStatusText = '⏳ 正在重置全频段...';
    },

    applyCellLock() {
      if (!this.cellLock.pci || !this.cellLock.earfcn) {
        this.cellLock.statusText = '✗ 请填写 PCI 和绝对频点号 EARFCN';
        return;
      }
      const req = payload.setCellLock({
        tech: this.cellLock.tech,
        pci: this.cellLock.pci,
        earfcn: this.cellLock.earfcn,
        band: this.cellLock.band,
        enable: true,
      });
      if (!request(ACTIONS.SET_CELL_LOCK, req, {
        expect: TYPES.CELL_LOCK_RES,
        intent: 'lock',
      }).ok) {
        this.cellLock.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.cellLock.statusText = '⏳ 正在强绑定物理小区...';
    },

    unlockCell() {
      if (!request(
        ACTIONS.SET_CELL_LOCK,
        payload.setCellLock({ tech: this.cellLock.tech, enable: false }),
        { expect: TYPES.CELL_LOCK_RES, intent: 'unlock' },
      ).ok) {
        this.cellLock.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.cellLock.statusText = '⏳ 解除绑定复位中...';
    },

    startNetworkScan() {
      if (this.scan.scanning) return;
      this.scan.scanning = true;
      this.scan.btnText = '正在后台扫描中...';
      this.scan.statusText = '正在扫描全网运营商公开基站频点（预计 15~30 秒，期间网络会短暂中断）...';
      ui.showLoading('正在深度扫频搜网中，请耐心等待...');

      if (!request(ACTIONS.NETWORK_SCAN, payload.networkScan(), {
        expect: TYPES.SCAN_RESULT,
        timeoutMs: 90000,
      }).ok) {
        this.scan.scanning = false;
        this.scan.btnText = '触发扫频 (15s+)';
        this.scan.statusText = '✗ 连接不可用，指令未下发';
        ui.hideLoading();
      }
    },

    runDiag(sub) {
      this.diag.isError = false;
      let payloadValue;
      try {
        payloadValue = payload.getDiagnostics(sub);
      } catch (err) {
        this.diag.isError = true;
        this.diag.outputText = `ERROR: ${err.message}`;
        return;
      }

      if (!request(ACTIONS.GET_DIAGNOSTICS, payloadValue, {
        expect: TYPES.DIAGNOSTICS_RES,
        intent: sub,
      }).ok) {
        this.diag.isError = true;
        this.diag.outputText = 'ERROR: 连接不可用，指令未下发';
        return;
      }
      this.diag.outputText = '⏳ 底层总线深度查询中...';
    },

    refreshMbnList() {
      if (!request(ACTIONS.GET_MBN_LIST, payload.getMbnList(), {
        expect: TYPES.MBN_LIST_RES,
      }).ok) {
        this.mbn.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.mbn.statusText = '⏳ 正在查询 MBN 列表...';
    },

    applyMbn() {
      if (!this.mbn.selected) {
        this.mbn.statusText = '✗ 请先选择一个 MBN';
        return;
      }
      if (!request(ACTIONS.SET_MBN, payload.setMbn(this.mbn.selected), {
        expect: TYPES.MBN_SET_RES,
        intent: 'apply',
      }).ok) {
        this.mbn.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.mbn.statusText = '⏳ 正在应用 MBN...';
    },

    setMbnAutoSel(enabled) {
      if (!request(ACTIONS.MBN_AUTOSEL, payload.mbnAutoSel(enabled), {
        expect: TYPES.MBN_SET_RES,
        intent: 'autosel',
      }).ok) {
        this.mbn.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.mbn.statusText = enabled ? '⏳ 启用 MBN 自动选择...' : '⏳ 禁用 MBN 自动选择...';
    },

    deactivateMbn() {
      if (!window.confirm('确认停用当前 MBN Profile？')) return;
      if (!request(ACTIONS.MBN_DEACTIVATE, payload.mbnDeactivate(), {
        expect: TYPES.MBN_SET_RES,
        intent: 'deactivate',
      }).ok) {
        this.mbn.statusText = '✗ 连接不可用，指令未下发';
        return;
      }
      this.mbn.statusText = '⏳ 正在停用当前 MBN...';
    },

    refreshDeviceSpecs() {
      send(ACTIONS.GET_DEVICE_INFO, payload.getDeviceInfo());
    },

    rebootModule() {
      if (!window.confirm('确认重启模组？（重启耗时约 20 秒，连接将会暂时断开）')) return;
      // 后端对 reboot 只回 settings_log（无结构化回执），因此这里不写任何"已生效"断言
      request(ACTIONS.REBOOT, payload.reboot(), {
        expect: TYPES.SETTINGS_LOG,
        intent: 'reboot',
        timeoutMs: 60000,
      });
    },

    factoryResetModule() {
      if (!window.confirm('警告！这会将模组配置彻底恢复为出厂配置，是否确认？')) return;
      request(ACTIONS.FACTORY_RESET, payload.factoryReset(), {
        expect: TYPES.SETTINGS_LOG,
        intent: 'factory_reset',
        timeoutMs: 60000,
      });
    },

    setFlightMode(on) {
      request(ACTIONS.FLIGHT_MODE, payload.flightMode(on), {
        expect: TYPES.SETTINGS_LOG,
        intent: on ? 'flight_on' : 'flight_off',
      });
    },

    // -----------------------------------------------------------------------
    // 入站回执处理（由 app.js 路由调用；pending 已在路由前解除）
    // -----------------------------------------------------------------------

    onNetworkStatus(data) {
      this.netStatusText = verdict(data);
    },

    onSimSlotRes(data) {
      this.simSlotStatusText = data?.success
        ? `⚠ ${data.note || `已切换到 SIM ${data.slot}`}`
        : `✗ ${data?.msg || '切换失败'}`;
    },

    onEthRes(data) {
      this.eth.statusText = verdict(data);
    },

    onIpptRes(data) {
      this.ippt.statusText = verdict(data);
    },

    /** 注意：后端 imei_res.kind 在解析成功时恒为 "read"（写入成功也是 read），
     *  所以文案必须依据发起意图 intent，而不能依据 kind。 */
    onImeiRes(data, intent) {
      if (data?.success) {
        if (intent === 'write') {
          this.imeiStatusText = '✓ IMEI 写入成功，请重启模组生效';
        } else {
          this.imeiInput = data.imei || this.imeiInput;
          this.imeiStatusText = `✓ 当前 IMEI: ${data.imei || '--'}`;
        }
      } else {
        this.imeiStatusText = `✗ IMEI ${intent === 'write' ? '写入' : '读取'}失败`;
      }
    },

    onBandLockRes(data, intent) {
      const ok = data?.success === true;
      this.bandLockStatusText = ok ? `✓ ${data?.msg || 'OK'}` : `✗ ${data?.msg || 'Failed'}`;

      // 只有后端确认重置成功，才清空本地磁贴选中态
      if (ok && intent?.kind === 'reset') {
        if (intent.rat === 'nr5g') this.selectedNr5gBands = [];
        else this.selectedLteBands = [];
      }
      this.bandResetPending = false;
    },

    onCellLockRes(data) {
      this.cellLock.statusText = verdict(data);
    },

    onScanResult(data) {
      this.scan.scanning = false;
      this.scan.btnText = '触发扫频 (15s+)';
      this.scan.networks = Array.isArray(data?.networks) ? data.networks : [];
      this.scan.statusText = data?.status || '扫描结束。';
      ui.hideLoading();
    },

    onDiagnosticsRes(data) {
      if (data?.success) {
        const raw = data.data;
        let text;
        if (raw && typeof raw === 'object') {
          text = typeof raw.raw === 'string' ? raw.raw : JSON.stringify(raw, null, 2);
        } else {
          text = raw === null || raw === undefined ? '' : String(raw);
        }
        this.diag.outputText = text || '(empty response)';
        this.diag.isError = false;
      } else {
        this.diag.outputText = `ERROR: ${data?.msg || 'Unknown error'}`;
        this.diag.isError = true;
      }
    },

    onMbnListRes(data) {
      if (data?.success && Array.isArray(data.list)) {
        this.mbn.list = data.list;
        const active = data.list.find((item) => item.state === 1);
        this.mbn.selected = active ? String(active.name) : '';
        this.mbn.statusText = `✓ 已加载 ${data.list.length} 个 MBN`;
      } else {
        this.mbn.statusText = `✗ ${data?.msg || '列表加载失败'}`;
      }
    },

    onMbnSetRes(data) {
      this.mbn.statusText = verdict(data);
    },

    /** 超时兜底：解锁按钮、结束加载态、把真实情况告诉用户 */
    onTimeout(expect) {
      switch (expect) {
        case TYPES.NETWORK_STATUS:
          this.netStatusText = TIMEOUT_TEXT;
          return true;
        case TYPES.SIM_SLOT_RES:
          this.simSlotStatusText = TIMEOUT_TEXT;
          return true;
        case TYPES.ETH_RES:
          this.eth.statusText = TIMEOUT_TEXT;
          return true;
        case TYPES.IPPT_RES:
          this.ippt.statusText = TIMEOUT_TEXT;
          return true;
        case TYPES.IMEI_RES:
          this.imeiStatusText = TIMEOUT_TEXT;
          return true;
        case TYPES.BAND_LOCK_RES:
          this.bandLockStatusText = TIMEOUT_TEXT;
          this.bandResetPending = false;
          return true;
        case TYPES.CELL_LOCK_RES:
          this.cellLock.statusText = TIMEOUT_TEXT;
          return true;
        case TYPES.SCAN_RESULT:
          this.scan.scanning = false;
          this.scan.btnText = '触发扫频 (15s+)';
          this.scan.statusText = '✗ 扫频超时（后端未返回），请重试。';
          ui.hideLoading();
          return true;
        case TYPES.DIAGNOSTICS_RES:
          this.diag.isError = true;
          this.diag.outputText = 'ERROR: 操作超时（后端未返回）';
          return true;
        case TYPES.MBN_LIST_RES:
        case TYPES.MBN_SET_RES:
          this.mbn.statusText = TIMEOUT_TEXT;
          return true;
        default:
          return false;
      }
    },
  };
}
