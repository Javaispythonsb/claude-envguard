import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  POD_WIDTH,
  POD_COMPACT_HEIGHT,
  CHECK_SPECS,
  maskDisplayIp,
  conciseCheckValue,
  buildPlatterTiles,
  computePrimaryIslandAction
} from '../src/hig-model.js';

test('Compact 灵动岛几何常量与 14 项对称网格无孤儿格', () => {
  assert.equal(POD_WIDTH, 480);
  assert.equal(POD_COMPACT_HEIGHT, 64);
  assert.equal(CHECK_SPECS.length, 14);
  assert.equal(CHECK_SPECS.length % 2, 0);
});

test('maskDisplayIp 对真实公网 IPv4 打码，保留 198.18.0.0/15 Fake-IP', () => {
  assert.equal(maskDisplayIp('出口 104.28.192.45 (US) TUN 198.18.0.1'), '出口 104.28.*.* (US) TUN 198.18.0.1');
  assert.equal(maskDisplayIp('TUN 198.19.255.12 正常'), 'TUN 198.19.255.12 正常');
});

test('computePrimaryIslandAction 遵循 Apple HIG 主操作角色切换', () => {
  // 环境安全 -> 系统绿「启动」
  const ready = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: true, needsFix: false });
  assert.equal(ready.mode, 'launch');
  assert.equal(ready.tone, 'green');
  assert.equal(ready.disabled, false);

  // 环境阻断但可自动修复 -> 系统琥珀黄「修复」
  const fixable = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: false, needsFix: true });
  assert.equal(fixable.mode, 'fix');
  assert.equal(fixable.tone, 'amber');
  assert.equal(fixable.disabled, false);

  // 环境阻断且不可自动修复 -> 红色锁定禁用
  const locked = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: false, needsFix: false });
  assert.equal(locked.mode, 'blocked');
  assert.equal(locked.tone, 'red');
  assert.equal(locked.disabled, true);

  // 忙碌态 -> 全部禁用防重入
  const busy = computePrimaryIslandAction({ busy: true, busyLabel: '修复…', complete: true, ready: true, needsFix: false });
  assert.equal(busy.mode, 'busy');
  assert.equal(busy.disabled, true);
});

test('conciseCheckValue 与 buildPlatterTiles 生成精简且无破折号的控制中心摘要', () => {
  const map = new Map([
    ['代理内核', { state: 'ok', detail: 'Clash Verge 运行中' }],
    ['TUN网卡', { state: 'ok', detail: 'Meta (198.18.0.1)' }],
    ['规则置顶', { state: 'ok', detail: '前 16 条已生效' }],
    ['出口IP', { state: 'ok', detail: 'US (104.28.15.9)' }],
    ['IPv6', { state: 'ok', detail: '已禁用' }],
    ['IP一致', { state: 'ok', detail: '与上次一致' }],
    ['DNS', { state: 'ok', detail: '解析器位于 US' }],
    ['WebRTC', { state: 'ok', detail: '无泄漏候选' }],
    ['保镖', { state: 'ok', detail: '运行中' }],
    ['生态可达性', { state: 'ok', detail: '可达' }],
    ['时区', { state: 'skip', detail: '已由用户禁用' }]
  ]);

  assert.equal(conciseCheckValue({ key: '出口IP' }, 'ok', 'US (104.28.15.9)', 'US'), 'US');
  assert.equal(conciseCheckValue({ key: 'DNS' }, 'ok', '解析器位于 US', 'US'), 'US');
  assert.equal(conciseCheckValue({ key: '时区' }, 'skip', '已由用户禁用', 'US'), '已跳过');

  const tiles = buildPlatterTiles({ country_target: 'US', proxy_mode: 'clash-verge' }, map);
  assert.equal(tiles.length, 4);
  for (const t of tiles) {
    assert.equal(t.tone, 'ok', t.id);
    assert.ok(!t.title.includes('—') && !t.sub.includes('—'));
  }
});
