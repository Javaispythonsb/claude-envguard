import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  POD_WIDTH,
  POD_COMPACT_HEIGHT,
  CHECK_SPECS,
  maskDisplayIp,
  conciseCheckValue,
  buildPlatterTiles,
  buildAnomalyCallouts,
  summarizeMatrixCounts,
  normalizeSafeUrl,
  computePrimaryIslandAction,
  formatUpdateCheckReport,
  formatBuildTime,
  plainReleaseNote,
  describeUpdateProgress
} from '../src/hig-model.js';

test('Compact 灵动岛几何常量与 14 项四域全覆盖无遗漏', () => {
  assert.equal(POD_WIDTH, 480);
  assert.equal(POD_COMPACT_HEIGHT, 64);
  assert.equal(CHECK_SPECS.length, 14);
  assert.equal(CHECK_SPECS.length % 2, 0);
  const validDomains = new Set(['tunnel', 'egress', 'leak', 'locale']);
  for (const s of CHECK_SPECS) {
    assert.ok(validDomains.has(s.domain), `${s.key} 必须归属四大控制域`);
  }
});

test('maskDisplayIp 对真实公网 IPv4 打码，保留 198.18.0.0/15 Fake-IP', () => {
  assert.equal(maskDisplayIp('出口 104.28.192.45 (US) TUN 198.18.0.1'), '出口 104.28.*.* (US) TUN 198.18.0.1');
  assert.equal(maskDisplayIp('TUN 198.19.255.12 正常'), 'TUN 198.19.255.12 正常');
});

test('conciseCheckValue 精准解析大小写不敏感的 ASN 属性与客户端语言偏差', () => {
  assert.equal(conciseCheckValue({ key: 'ASN属性' }, 'warn', 'Hosting(AS13335 Cloudflare)', 'US'), '机房');
  assert.equal(conciseCheckValue({ key: 'ASN属性' }, 'warn', 'Content(AS13335 Cloudflare, Inc.)', 'US'), 'CDN');
  assert.equal(conciseCheckValue({ key: 'ASN属性' }, 'ok', 'ISP(AS7922 Comcast)', 'US'), '住宅 ISP');
  assert.equal(
    conciseCheckValue({ key: '客户端语言' }, 'warn', 'Claude=zh-CN,期望 en 前缀', 'US'),
    'zh-CN≠en'
  );
  assert.equal(conciseCheckValue({ key: '时区' }, 'skip', '已由用户禁用', 'US'), '已免检');
});

test('buildPlatterTiles 消除上绿下黄矛盾：ASN 与客户端语言告警同步反映在对应域磁贴', () => {
  const map = new Map([
    ['代理内核', { state: 'ok', detail: 'Clash Verge 运行中' }],
    ['TUN网卡', { state: 'ok', detail: 'Meta (198.18.0.1)' }],
    ['规则置顶', { state: 'ok', detail: '前 16 条已生效' }],
    ['出口IP', { state: 'ok', detail: 'US (104.28.15.9)' }],
    ['IPv6', { state: 'ok', detail: '已禁用' }],
    ['IP一致', { state: 'ok', detail: '与上次一致' }],
    ['ASN属性', { state: 'warn', detail: 'Hosting(AS13335 Cloudflare)' }],
    ['DNS', { state: 'ok', detail: '解析器位于 US' }],
    ['WebRTC', { state: 'ok', detail: '无泄漏候选' }],
    ['保镖', { state: 'ok', detail: '运行中' }],
    ['生态可达性', { state: 'ok', detail: '可达' }],
    ['客户端语言', { state: 'warn', detail: 'Claude=zh-CN,期望 en 前缀' }],
    ['时区', { state: 'skip', detail: '已由用户禁用' }],
    ['地区', { state: 'skip', detail: '已由用户禁用' }]
  ]);

  const tiles = buildPlatterTiles({ country_target: 'US', proxy_mode: 'clash-verge' }, map);
  assert.equal(tiles.length, 4);
  const byId = Object.fromEntries(tiles.map((t) => [t.id, t]));
  assert.equal(byId.tunnel.tone, 'ok');
  assert.equal(byId.egress.tone, 'warn');
  assert.match(byId.egress.sub, /机房节点/);
  assert.equal(byId.leak.tone, 'ok');
  assert.equal(byId.locale.tone, 'warn');
  assert.match(byId.locale.sub, /zh-CN≠en/);

  for (const t of tiles) {
    assert.ok(!t.title.includes('—') && !t.sub.includes('—'));
  }

  const callouts = buildAnomalyCallouts({ country_target: 'US' }, map);
  assert.equal(callouts.length, 2);
  assert.equal(callouts[0].actionType, 'mute');

  const summary = summarizeMatrixCounts(map);
  assert.equal(summary.ok, 10);
  assert.equal(summary.warn, 2);
  assert.equal(summary.skip, 2);
  assert.equal(summary.summaryText, '10 正常 · 2 提示 · 2 免检');
});

test('normalizeSafeUrl 支持自动补全裸域名 https:// 并拦截非法协议与引号注入', () => {
  assert.deepEqual(normalizeSafeUrl('claude.ai/login'), {
    ok: true,
    url: 'https://claude.ai/login',
    reason: ''
  });
  assert.deepEqual(normalizeSafeUrl('https://anthropic.com/status'), {
    ok: true,
    url: 'https://anthropic.com/status',
    reason: ''
  });
  assert.equal(normalizeSafeUrl('javascript:alert(1)').ok, false);
  assert.equal(normalizeSafeUrl('https://claude.ai/" --flag').ok, false);
  assert.equal(normalizeSafeUrl('').ok, false);
});

test('computePrimaryIslandAction 遵循 Apple HIG 主操作角色切换', () => {
  const ready = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: true, needsFix: false });
  assert.equal(ready.mode, 'launch');
  assert.equal(ready.tone, 'green');
  assert.equal(ready.disabled, false);

  const fixable = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: false, needsFix: true });
  assert.equal(fixable.mode, 'fix');
  assert.equal(fixable.tone, 'amber');
  assert.equal(fixable.disabled, false);

  const locked = computePrimaryIslandAction({ busy: false, busyLabel: '', complete: true, ready: false, needsFix: false });
  assert.equal(locked.mode, 'blocked');
  assert.equal(locked.tone, 'red');
  assert.equal(locked.disabled, true);
});

const HIDDEN_DEV_DETAILS = /1111111|2222222|3333333|github\.com|feat|fix:|polish|SHA/;

test('formatUpdateCheckReport 只展示版本与中文说明，不暴露提交哈希、提交标题或仓库地址', () => {
  const newer = formatUpdateCheckReport({
    status: 'update_available',
    repo: 'Javaispythonsb/claude-envguard',
    current_version: '2.0.0',
    current_commit: '1111111',
    latest_version: '2.1.0',
    latest_commit: '2222222',
    same_version: false,
    notes: '悬浮球拖动更顺滑，热更新显示进度'
  });
  assert.equal(newer.canHotUpdate, true);
  assert.equal(newer.title, '发现新版本 v2.1.0');
  assert.match(newer.body, /当前版本：v2\.0\.0/);
  assert.match(newer.body, /最新版本：v2\.1\.0/);
  assert.match(newer.body, /更新内容：悬浮球拖动更顺滑，热更新显示进度/);
  assert.doesNotMatch(newer.body, HIDDEN_DEV_DETAILS);

  // 同版本号的滚动构建：用构建时间区分，英文提交标题换成固定中文说明
  const maintenance = formatUpdateCheckReport({
    status: 'update_available',
    current_version: '2.0.0',
    current_commit: '1111111',
    current_build_unix: 1794466125,
    latest_version: '2.0.0',
    latest_commit: '2222222',
    latest_build_unix: 1794474325,
    same_version: true,
    notes: 'feat(desktop): add luminous breathing orb'
  });
  assert.equal(maintenance.canHotUpdate, true);
  assert.match(maintenance.body, /维护更新/);
  assert.match(maintenance.body, /常规维护与稳定性改进/);
  assert.ok(maintenance.body.includes(formatBuildTime(1794474325)));
  assert.doesNotMatch(maintenance.body, HIDDEN_DEV_DETAILS);

  const pending = formatUpdateCheckReport({
    status: 'up_to_date',
    current_version: '2.0.0',
    current_commit: '2222222',
    ci_building: true,
    ci_head_commit: '3333333',
    ci_head_title: 'fix: polish'
  });
  assert.equal(pending.canHotUpdate, false);
  assert.match(pending.body, /正在发布/);
  assert.doesNotMatch(pending.body, HIDDEN_DEV_DETAILS);

  const building = formatUpdateCheckReport({ status: 'building', current_version: '2.0.0', latest_commit: '3333333' });
  assert.equal(building.canHotUpdate, false);
  assert.match(building.title, /发布中/);
  assert.doesNotMatch(building.body, HIDDEN_DEV_DETAILS);

  for (const r of [newer, maintenance, pending, building]) {
    assert.ok(!r.body.includes('—') && !r.title.includes('—'));
  }
});

test('plainReleaseNote 与 describeUpdateProgress', () => {
  assert.equal(plainReleaseNote('feat(desktop): add orb'), '常规维护与稳定性改进');
  assert.equal(plainReleaseNote(''), '常规维护与稳定性改进');
  assert.equal(plainReleaseNote('  修复  拖动 '), '修复 拖动');
  assert.equal(formatBuildTime(0), '');

  assert.deepEqual(describeUpdateProgress('download', 42, 10), { label: '下载更新包', percent: 42 });
  assert.equal(describeUpdateProgress('verify', 30, 60).percent, 60, '进度条不能倒退');
  assert.equal(describeUpdateProgress('restart', 180, 0).percent, 100);
  assert.equal(describeUpdateProgress('unknown-stage', 5).label, '正在更新');
});
