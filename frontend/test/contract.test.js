/**
 * 契约层纯函数测试（Node 内置 test runner，零新依赖）。
 *
 * 覆盖重点是历史上最容易出问题的地方：
 *   - `sms_list` / `at_res` 的裸文本解析
 *   - 出站载荷的值类型（后端对类型不符是静默降级，不会报错）
 */
import test from 'node:test';
import assert from 'node:assert/strict';

import {
  payload,
  parseAtResponse,
  parseCmgl,
  extractPercent,
  extractSlashPercent,
  DIAG,
} from '../src/js/contract.js';

test('parseAtResponse 规范化 CRLF 并去掉尾部空行', () => {
  assert.equal(parseAtResponse('OK\r\n\r\n'), 'OK');
  assert.equal(parseAtResponse('+CSQ: 23,99\r\nOK'), '+CSQ: 23,99\nOK');
  assert.equal(parseAtResponse(''), '');
});

test('parseCmgl 解析单条短信', () => {
  const raw =
    '+CMGL: 1,1,"+8613800138000",,"24/09/25,10:11:12+32"\r\nhello world\r\nOK';
  const msgs = parseCmgl(raw);
  assert.equal(msgs.length, 1);
  assert.equal(msgs[0].index, 1);
  assert.equal(msgs[0].sender, '+8613800138000');
  assert.equal(msgs[0].timestamp, '24/09/25,10:11:12+32');
  assert.equal(msgs[0].text, 'hello world');
});

test('parseCmgl 解析多条短信', () => {
  const raw = [
    '+CMGL: 1,1,"10086",,"24/09/25,10:11:12+32"',
    'first',
    '+CMGL: 2,1,"10010",,"24/09/25,10:12:00+32"',
    'second',
    'OK',
  ].join('\r\n');
  const msgs = parseCmgl(raw);
  assert.equal(msgs.length, 2);
  assert.deepEqual(
    msgs.map((m) => [m.index, m.sender, m.text]),
    [
      [1, '10086', 'first'],
      [2, '10010', 'second'],
    ],
  );
});

test('parseCmgl 保留多行正文', () => {
  const raw = '+CMGL: 3,1,"10086",,"24/09/25,10:11:12+32"\r\nline1\r\nline2\r\nOK';
  const msgs = parseCmgl(raw);
  assert.equal(msgs[0].text, 'line1\nline2');
});

test('parseCmgl 处理 <alpha> 中含逗号的引号字段', () => {
  const raw = '+CMGL: 1,1,"+86138","China, Mobile","24/09/25,10:11:12+32"\r\nbody\r\nOK';
  const msgs = parseCmgl(raw);
  assert.equal(msgs[0].sender, '+86138');
  assert.equal(msgs[0].timestamp, '24/09/25,10:11:12+32');
  assert.equal(msgs[0].text, 'body');
});

test('parseCmgl 对空输入与无短信内容返回空数组', () => {
  assert.deepEqual(parseCmgl(''), []);
  assert.deepEqual(parseCmgl(undefined), []);
  assert.deepEqual(parseCmgl('OK'), []);
});

test('载荷类型：flight_mode 与 mbn_autosel 必须是字符串 "1"/"0"', () => {
  assert.equal(payload.flightMode(true), '1');
  assert.equal(payload.flightMode(false), '0');
  assert.equal(payload.mbnAutoSel(true), '1');
  assert.equal(payload.mbnAutoSel(false), '0');
  assert.equal(typeof payload.flightMode(true), 'string');
});

test('载荷类型：set_apn 的 auth 必须是字符串', () => {
  const p = payload.setApn({ apn: '3gnet', user: 'u', pass: 'p', auth: 2 });
  assert.deepEqual(p, { apn: '3gnet', user: 'u', pass: 'p', auth: '2' });
  assert.equal(typeof p.auth, 'string');
});

test('载荷类型：set_sim_slot 的 slot 与 set_cell_lock 的数字字段', () => {
  assert.deepEqual(payload.setSimSlot('2'), { slot: 2 });

  const lock = payload.setCellLock({ tech: 'lte', pci: '123', earfcn: '1650', enable: true });
  assert.deepEqual(lock, { tech: 'lte', enable: true, pci: 123, earfcn: 1650 });

  const lockWithBand = payload.setCellLock({
    tech: '5g',
    pci: '1',
    earfcn: '2',
    band: '41',
    enable: true,
  });
  assert.equal(lockWithBand.band, 41);
});

test('载荷类型：set_cell_lock 空 band 不下发，解锁时只带 tech+enable', () => {
  const lock = payload.setCellLock({ tech: 'lte', pci: 1, earfcn: 2, band: '', enable: true });
  assert.equal('band' in lock, false);

  const unlock = payload.setCellLock({ tech: 'lte', enable: false });
  assert.deepEqual(unlock, { tech: 'lte', enable: false });
});

test('载荷类型：set_band_lock 的 nr5g 是布尔、bands 是字符串', () => {
  assert.deepEqual(payload.setBandLock({ nr5g: 1, bands: 78 }), { nr5g: true, bands: '78' });
  assert.deepEqual(payload.setBandLock({ nr5g: false, bands: 'all' }), {
    nr5g: false,
    bands: 'all',
  });
});

test('载荷类型：set_eth_config 的 pcie_rc 是布尔', () => {
  assert.deepEqual(payload.setEthConfig({ driver: 'r8125', pcie_rc: '1' }), {
    driver: 'r8125',
    pcie_rc: true,
  });
});

test('set_interval 被 clamp 到 >= 3 的数字', () => {
  assert.equal(payload.setInterval('5'), 5);
  assert.equal(payload.setInterval(1), 3);
  assert.equal(payload.setInterval('abc'), 3);
});

test('set_view_state 只发送 active，其余一律 idle', () => {
  assert.equal(payload.setViewState('active'), 'active');
  assert.equal(payload.setViewState('idle'), 'idle');
  assert.equal(payload.setViewState(true), 'idle');
});

test('get_diagnostics 白名单外的子命令抛错（避免后端静默无响应）', () => {
  assert.equal(payload.getDiagnostics(DIAG.NEIGHBOUR), 'neighbour');
  assert.equal(payload.getDiagnostics(DIAG.AUTOSEL_QUERY), 'autosel_query');
  assert.throws(() => payload.getDiagnostics('nope'), /未知的诊断子命令/);
});

test('百分比提取', () => {
  assert.equal(extractPercent('23%'), 23);
  assert.equal(extractPercent('加载 45.5%'), 45.5);
  assert.equal(extractPercent('--%'), 0);
  assert.equal(extractPercent(null), 0);

  assert.equal(extractSlashPercent('-85 dBm / 76%'), 76);
  assert.equal(extractSlashPercent('12 / 130%'), 100);
  assert.equal(extractSlashPercent('-- / --%'), 0);
});
