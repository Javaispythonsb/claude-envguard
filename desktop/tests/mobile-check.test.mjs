import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  classifyIpv4,
  isPublicIpv6,
  extractCandidateIps,
  evaluateWebRtc,
  evaluateDnsResolvers
} from '../src/mobile-check.js';

const noLookup = async () => {
  throw new Error('不应查询归属');
};

test('classifyIpv4 区分公网、内网与 Fake-IP 网段', () => {
  for (const ip of ['10.1.2.3', '172.16.0.1', '172.31.255.1', '192.168.1.1', '127.0.0.1', '169.254.1.1', '100.64.0.1', '100.127.255.254', '0.0.0.0', '224.0.0.1']) {
    assert.equal(classifyIpv4(ip), 'private', ip);
  }
  for (const ip of ['198.18.0.1', '198.19.255.254']) assert.equal(classifyIpv4(ip), 'fake', ip);
  for (const ip of ['8.8.8.8', '172.32.0.1', '100.128.0.1', '198.20.0.1', '203.0.113.7']) assert.equal(classifyIpv4(ip), 'public', ip);
  for (const ip of ['1.2.3', '1.2.3.256', 'abc', '']) assert.equal(classifyIpv4(ip), 'invalid', ip);
});

test('isPublicIpv6 只接受全球单播地址', () => {
  assert.equal(isPublicIpv6('2408:8207:1234::1'), true);
  assert.equal(isPublicIpv6('2a01:4f8::2'), true);
  for (const ip of ['fe80::1', 'fe80::1%12', 'fd12:3456::1', 'fc00::1', '::1', '::', 'ff02::1', '2001:db8::1', '::ffff:1.2.3.4']) {
    assert.equal(isPublicIpv6(ip), false, ip);
  }
});

test('extractCandidateIps 读取地址与 raddr，并识别 mDNS', () => {
  const found = extractCandidateIps([
    'candidate:1 1 udp 2122260223 7d1c2b9a-1b2c-4d5e-8f90-123456789abc.local 54321 typ host generation 0',
    'candidate:2 1 udp 1686052607 203.0.113.7 54321 typ srflx raddr 198.18.0.1 rport 54321 generation 0',
    'candidate:3 1 udp 2122197247 2408:8207:1234::1 54322 typ host generation 0'
  ]);
  assert.equal(found.mdns, true);
  assert.deepEqual([...found.v4].sort(), ['198.18.0.1', '203.0.113.7']);
  assert.deepEqual(found.v6, ['2408:8207:1234::1']);
});

test('TUN 网卡的 Fake-IP 地址与代理出口同时出现时不算泄漏', async () => {
  const result = await evaluateWebRtc({ v4: ['198.18.0.1', '203.0.113.7'], v6: [], mdns: false }, { exitIp: '203.0.113.7' }, noLookup);
  assert.equal(result.state, 'ok');
});

test('只有内网、CGNAT、Fake-IP 与链路本地候选时通过', async () => {
  const result = await evaluateWebRtc({ v4: ['192.168.1.5', '100.70.1.2', '198.19.0.3'], v6: ['fe80::1'], mdns: false }, { exitIp: '203.0.113.7' }, noLookup);
  assert.equal(result.state, 'ok');
});

test('暴露国内 UDP 出口判失败', async () => {
  const result = await evaluateWebRtc({ v4: ['203.0.113.7', '58.32.1.2'], v6: [], mdns: true }, { exitIp: '203.0.113.7' }, async () => 'CN');
  assert.equal(result.state, 'fail');
  assert.match(result.detail, /国内 UDP 出口: 58\.32\.\*\.\*/);
});

test('海外 UDP 出口与 HTTP 出口不同只提示', async () => {
  const result = await evaluateWebRtc({ v4: ['198.51.100.9'], v6: [], mdns: true }, { exitIp: '203.0.113.7' }, async () => 'US');
  assert.equal(result.state, 'warn');
});

test('归属查不到的非代理候选仍判失败', async () => {
  const result = await evaluateWebRtc({ v4: ['198.51.100.9'], v6: [], mdns: false }, { exitIp: '203.0.113.7' }, async () => '');
  assert.equal(result.state, 'fail');
});

test('公网 IPv6 候选不是代理的 IPv6 出口时判失败', async () => {
  const leak = await evaluateWebRtc({ v4: [], v6: ['2408:8207:1234::1'], mdns: false }, { exitIp: '203.0.113.7', exitIpv6: '' }, noLookup);
  assert.equal(leak.state, 'fail');
  const proxied = await evaluateWebRtc({ v4: [], v6: ['2a01:4f8::2'], mdns: false }, { exitIp: '203.0.113.7', exitIpv6: '2A01:4F8::2' }, noLookup);
  assert.equal(proxied.state, 'ok');
});

test('DNS 解析器：国内判失败、归属未知提示、全在海外通过', () => {
  assert.equal(evaluateDnsResolvers([{ ip: '8.8.8.8', country: 'US', isp: 'Google' }, { ip: '223.5.5.5', country: 'CN', isp: 'Alibaba' }]).state, 'fail');
  assert.equal(evaluateDnsResolvers([{ ip: '8.8.8.8', country: 'US', isp: 'Google' }, { ip: '1.1.1.1', country: '', isp: '' }]).state, 'warn');
  assert.equal(evaluateDnsResolvers([]).state, 'warn');
  const ok = evaluateDnsResolvers([{ ip: '8.8.8.8', country: 'US', isp: 'Google' }, { ip: '8.8.4.4', country: 'US', isp: 'Google' }]);
  assert.equal(ok.state, 'ok');
  assert.match(ok.detail, /US/);
});
