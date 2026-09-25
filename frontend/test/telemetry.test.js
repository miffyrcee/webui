/**
 * 遥测 store 的合并与展示语义测试（Node 内置 test runner，零新依赖）。
 *
 * 覆盖两个历史缺陷：
 *   - 后端 `updated` 已带本地时区，前端再补 'Z' 会二次本地化，凭空多出时区偏移
 *   - 后端用 JSON null 表示「本轮未采到」，前端若跳过 null 会永久残留掉线前的值
 */
import test from 'node:test';
import assert from 'node:assert/strict';

import { createTelemetryStore } from '../src/js/telemetry.js';

const makeStore = () => createTelemetryStore({ send() {} });

test('merge 在收到 null 时把字段回落为占位符', () => {
  const store = makeStore();
  store.merge({ ipv4: '10.172.99.214', signal_percentage: '78%' });
  assert.equal(store.ipv4, '10.172.99.214');

  // 模组脱网：后端把采不到的字段发成 null
  store.merge({ ipv4: null, signal_percentage: '78%' });
  assert.equal(store.ipv4, '--');
  assert.equal(store.signal_percentage, '78%');
});

test('merge 忽略 undefined，不清空当前值', () => {
  const store = makeStore();
  store.merge({ ipv4: '10.172.99.214' });
  store.merge({ bands: 'NR5G BAND 41' });
  assert.equal(store.ipv4, '10.172.99.214');
});

test('merge 在 firmware_version 为 null 时不触发联动回调', () => {
  const store = makeStore();
  const seen = [];
  store.onFirmware = (v) => seen.push(v);

  store.merge({ firmware_version: 'RM520NGLAAR01A02M4G' });
  store.merge({ firmware_version: null });

  assert.deepEqual(seen, ['RM520NGLAAR01A02M4G']);
  assert.equal(store.firmware_version, '--');
});

test('updatedText 直接展示后端本地时间，不再二次本地化', () => {
  const store = makeStore();
  store.merge({ updated: '2026/09/25 22:41:00' });
  // 若这里被当作 UTC 再本地化，东八区下会变成次日 06:41:00
  assert.equal(store.updatedText, '2026/09/25 22:41:00');
});

test('updatedText 在缺失时退回渲染时刻的本地时间戳', () => {
  const store = makeStore();
  assert.match(store.updatedText, /^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}:\d{2}$/);

  store.merge({ updated: '--' });
  assert.match(store.updatedText, /^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}:\d{2}$/);
});
