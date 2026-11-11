const CN_TIMEZONES = new Set([
  'Asia/Shanghai',
  'Asia/Chongqing',
  'Asia/Harbin',
  'Asia/Urumqi'
]);

async function fetchWithTimeout(url, timeoutMs = 5000) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    return await fetch(url, { signal: ctrl.signal, cache: 'no-store' });
  } finally {
    clearTimeout(timer);
  }
}

function parseTrace(text) {
  const map = {};
  for (const line of String(text || '').split(/\r?\n/)) {
    const idx = line.indexOf('=');
    if (idx > 0) map[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
  }
  return map;
}

function maskIp(ip) {
  if (!ip) return '未知';
  if (ip.includes('.')) {
    const p = ip.split('.');
    if (p.length === 4) return `${p[0]}.${p[1]}.*.*`;
  }
  if (ip.includes(':')) {
    const p = ip.split(':').filter(Boolean);
    return `${p.slice(0, 2).join(':')}:*`;
  }
  return ip;
}

function randomHex(length) {
  const bytes = new Uint8Array(Math.ceil(length / 2));
  crypto.getRandomValues(bytes);
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('').slice(0, length);
}

// 198.18.0.0/15 是 Clash/Shadowrocket 等 Fake-IP 与 TUN 网卡常用网段；CGNAT、链路本地、组播等都不是公网出口
export function classifyIpv4(ip) {
  if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(String(ip))) return 'invalid';
  const octets = String(ip).split('.').map(Number);
  if (octets.some((n) => n > 255)) return 'invalid';
  const [a, b] = octets;
  if (a === 198 && (b === 18 || b === 19)) return 'fake';
  if (a === 0 || a === 10 || a === 127 || a >= 224) return 'private';
  if (a === 169 && b === 254) return 'private';
  if (a === 172 && b >= 16 && b <= 31) return 'private';
  if (a === 192 && b === 168) return 'private';
  if (a === 100 && b >= 64 && b <= 127) return 'private';
  return 'public';
}

// 只把 2000::/3 全球单播算公网；链路本地、ULA、组播、文档段都不算
export function isPublicIpv6(ip) {
  const v = String(ip || '').toLowerCase().split('%')[0];
  if (!v.includes(':') || /^2001:0?db8:/.test(v)) return false;
  const first = parseInt(v.split(':')[0], 16);
  if (Number.isNaN(first)) return false;
  return (first & 0xe000) === 0x2000;
}

// ICE 候选：第 5 段是地址，srflx/relay 还可能带 raddr（本机基础地址）
export function extractCandidateIps(candidates) {
  const v4 = new Set();
  const v6 = new Set();
  let mdns = false;
  for (const line of candidates) {
    const parts = String(line || '').trim().split(/\s+/);
    const addresses = [parts[4]];
    const raddr = parts.indexOf('raddr');
    if (raddr > 0) addresses.push(parts[raddr + 1]);
    for (const addr of addresses) {
      if (!addr) continue;
      if (addr.endsWith('.local')) mdns = true;
      else if (addr.includes(':')) v6.add(addr.toLowerCase());
      else if (classifyIpv4(addr) !== 'invalid') v4.add(addr);
    }
  }
  return { v4: [...v4], v6: [...v6], mdns };
}

export async function evaluateWebRtc(found, { exitIp = '', exitIpv6 = '' } = {}, countryOf = async () => '') {
  const publicV4 = found.v4.filter((ip) => classifyIpv4(ip) === 'public');
  const publicV6 = found.v6.filter(isPublicIpv6);
  if (!publicV4.length && !publicV6.length) {
    return {
      state: 'ok',
      detail: found.mdns ? '未暴露公网候选（mDNS 混淆已生效）' : '未暴露公网候选（仅内网或 Fake-IP 地址）'
    };
  }
  const v6Leaks = publicV6.filter((ip) => ip !== String(exitIpv6).toLowerCase());
  if (v6Leaks.length) {
    return { state: 'fail', detail: `暴露 IPv6 候选: ${v6Leaks.map(maskIp).join(', ')}` };
  }
  const v4Others = publicV4.filter((ip) => ip !== exitIp);
  if (!v4Others.length) {
    return { state: 'ok', detail: `候选地址与代理出口一致 (${maskIp(publicV4[0] || publicV6[0])})` };
  }
  const countries = await Promise.all(v4Others.slice(0, 3).map((ip) => countryOf(ip).catch(() => '')));
  if (countries.includes('CN')) {
    return { state: 'fail', detail: `暴露国内 UDP 出口: ${v4Others.map(maskIp).join(', ')}` };
  }
  if (countries.length && countries.every(Boolean)) {
    return {
      state: 'warn',
      detail: `UDP 出口与 HTTP 出口不同（${[...new Set(countries)].join('/')} · ${v4Others.map(maskIp).join(', ')}）`
    };
  }
  return { state: 'fail', detail: `发现归属未知的非代理出口候选: ${v4Others.map(maskIp).join(', ')}` };
}

export function evaluateDnsResolvers(resolvers) {
  if (!resolvers.length) return { state: 'warn', detail: '检测服务未记录到 DNS 解析器' };
  const domestic = resolvers.filter((r) => r.country === 'CN');
  if (domestic.length) {
    return {
      state: 'fail',
      detail: `DNS 经国内解析器: ${domestic.map((r) => `${maskIp(r.ip)}${r.isp ? ` (${r.isp})` : ''}`).join(', ')}`
    };
  }
  if (resolvers.some((r) => !r.country)) {
    return { state: 'warn', detail: `部分解析器归属未知: ${resolvers.map((r) => maskIp(r.ip)).join(', ')}` };
  }
  const countries = [...new Set(resolvers.map((r) => r.country))];
  return { state: 'ok', detail: `解析器位于 ${countries.join('/')}（${resolvers.length} 个，无国内解析器）` };
}

async function lookupCountry(ip) {
  const r = await fetchWithTimeout(`https://ipinfo.io/${encodeURIComponent(ip)}/json`, 3500);
  if (!r.ok) return '';
  return String((await r.json()).country || '').toUpperCase();
}

async function checkEgress() {
  try {
    const r = await fetchWithTimeout('https://ipinfo.io/json', 4500);
    if (r.ok) {
      const j = await r.json();
      const cc = String(j.country || '').toUpperCase();
      const ip = String(j.ip || '');
      if (cc && ip) {
        if (cc === 'CN') {
          return { state: 'fail', detail: `出口位于国内 (${cc} · ${maskIp(ip)})，请先开启代理`, ip, country: cc, org: j.org || '' };
        }
        return { state: 'ok', detail: `${cc} · ${maskIp(ip)}${j.org ? ` (${j.org})` : ''}`, ip, country: cc, org: j.org || '' };
      }
    }
  } catch {}

  try {
    const r = await fetchWithTimeout('https://www.cloudflare.com/cdn-cgi/trace', 4500);
    if (r.ok) {
      const t = parseTrace(await r.text());
      const cc = String(t.loc || '').toUpperCase();
      const ip = String(t.ip || '');
      if (cc && ip) {
        if (cc === 'CN') {
          return { state: 'fail', detail: `出口位于国内 (${cc} · ${maskIp(ip)})，请先开启代理`, ip, country: cc };
        }
        return { state: 'ok', detail: `${cc} · ${maskIp(ip)} (Cloudflare Trace)`, ip, country: cc };
      }
    }
  } catch {}

  return { state: 'fail', detail: '无法探测出口 IP（检查网络连接或代理规则）', ip: '', country: '' };
}

function checkBrowserRegion(egressCountry) {
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone || 'unknown';
  const offsetHours = -new Date().getTimezoneOffset() / 60;
  const offsetStr = `UTC${offsetHours >= 0 ? '+' + offsetHours : offsetHours}`;
  const lang = navigator.language || 'unknown';

  if (egressCountry && egressCountry !== 'CN' && CN_TIMEZONES.has(tz)) {
    return {
      state: 'warn',
      detail: `浏览器时区为 ${tz} (${offsetStr})，与出口 ${egressCountry} 存在东八区时差信号`
    };
  }
  return {
    state: 'ok',
    detail: `${tz} (${offsetStr}) · 语言 ${lang}`
  };
}

async function gatherWebRtcCandidates() {
  const RTCPeer = window.RTCPeerConnection || window.webkitRTCPeerConnection;
  if (!RTCPeer) return null;
  const candidates = [];

  await new Promise((resolve) => {
    let settled = false;
    const pc = new RTCPeer({
      iceServers: [
        { urls: 'stun:stun.cloudflare.com:3478' },
        { urls: 'stun:stun.l.google.com:19302' }
      ]
    });
    const finish = () => {
      if (settled) return;
      settled = true;
      try { pc.close(); } catch {}
      resolve();
    };
    const timer = setTimeout(finish, 3200);
    pc.onicecandidate = (ev) => {
      if (!ev || !ev.candidate) {
        clearTimeout(timer);
        finish();
        return;
      }
      candidates.push(ev.candidate.candidate || '');
    };
    try {
      pc.createDataChannel('eg');
      pc.createOffer().then((o) => pc.setLocalDescription(o)).catch(finish);
    } catch {
      clearTimeout(timer);
      finish();
    }
  });

  return extractCandidateIps(candidates);
}

async function checkIpv6(egressCountry) {
  try {
    const r = await fetchWithTimeout('https://api6.ipify.org?format=json', 3200);
    if (!r.ok) return { state: 'ok', detail: '未检测到 IPv6 直连通道（无双栈旁路风险）', ip: '' };
    const j = await r.json();
    const v6 = String(j.ip || '');
    if (!v6.includes(':')) return { state: 'ok', detail: '未启用公网 IPv6', ip: '' };

    try {
      const infoRes = await fetchWithTimeout(`https://ipinfo.io/${encodeURIComponent(v6)}/json`, 3500);
      if (infoRes.ok) {
        const info = await infoRes.json();
        const v6cc = String(info.country || '').toUpperCase();
        if (v6cc === 'CN' || (egressCountry && v6cc && v6cc !== egressCountry)) {
          return { state: 'fail', detail: `IPv6 旁路泄漏! 归属 ${v6cc} (${maskIp(v6)})`, ip: v6 };
        }
        return { state: 'ok', detail: `IPv6 已走同区代理 (${v6cc} · ${maskIp(v6)})`, ip: v6 };
      }
    } catch {}
    return { state: 'warn', detail: `检测到公网 IPv6 (${maskIp(v6)})，请确认已走代理`, ip: v6 };
  } catch {
    return { state: 'ok', detail: 'IPv6 未通或已关闭（无双栈旁路泄漏）', ip: '' };
  }
}

// 访问随机子域名，让当前 DNS 解析器去问 ipleak.net 的权威服务器；它记录下来的解析器就是真实解析路径
async function checkDnsLeak() {
  try {
    const r = await fetchWithTimeout(`https://${randomHex(40)}.ipleak.net/dnsdetection/`, 8000);
    if (!r.ok) throw new Error(String(r.status));
    const ips = Object.keys((await r.json()).ip || {}).slice(0, 4);
    const resolvers = await Promise.all(ips.map(async (ip) => {
      try {
        const info = await (await fetchWithTimeout(`https://ipleak.net/json/${encodeURIComponent(ip)}`, 5000)).json();
        return { ip, country: String(info.country_code || '').toUpperCase(), isp: String(info.isp_name || '') };
      } catch {
        return { ip, country: '', isp: '' };
      }
    }));
    return evaluateDnsResolvers(resolvers);
  } catch {
    return { state: 'warn', detail: 'DNS 泄漏检测服务不可达（ipleak.net）' };
  }
}

async function checkFingerprint() {
  let gpu = '未知 GPU';
  try {
    const cv = document.createElement('canvas');
    const gl = cv.getContext('webgl') || cv.getContext('experimental-webgl');
    if (gl) {
      const ext = gl.getExtension('WEBGL_debug_renderer_info');
      gpu = ext ? String(gl.getParameter(ext.UNMASKED_RENDERER_WEBGL)) : String(gl.getParameter(gl.RENDERER));
    }
  } catch {}

  let canvasShort = 'n/a';
  try {
    const c = document.createElement('canvas');
    c.width = 180;
    c.height = 36;
    const ctx = c.getContext('2d');
    ctx.font = '14px monospace';
    ctx.fillStyle = '#39ff14';
    ctx.fillText('EnvGuard-FP-Check', 6, 22);
    const raw = new TextEncoder().encode(c.toDataURL());
    const buf = await crypto.subtle.digest('SHA-256', raw);
    canvasShort = [...new Uint8Array(buf)].slice(0, 4).map((b) => b.toString(16).padStart(2, '0')).join('');
  } catch {}

  const swRender = /SwiftShader|llvmpipe|Software/i.test(gpu);
  const shortGpu = gpu.length > 34 ? gpu.slice(0, 34) + '…' : gpu;
  return {
    state: swRender ? 'warn' : 'ok',
    detail: `${shortGpu} · Canvas#${canvasShort}`
  };
}

export async function runMobileSelfCheck(onStep) {
  const egress = await checkEgress();
  onStep('egress', egress);

  const region = checkBrowserRegion(egress.country);
  onStep('region', region);

  const [ipv6, dns, fp, candidates] = await Promise.all([
    checkIpv6(egress.country).then((r) => { onStep('ipv6', r); return r; }),
    checkDnsLeak().then((r) => { onStep('dns', r); return r; }),
    checkFingerprint().then((r) => { onStep('fp', r); return r; }),
    gatherWebRtcCandidates()
  ]);
  const webrtc = candidates
    ? await evaluateWebRtc(candidates, { exitIp: egress.ip, exitIpv6: ipv6.ip }, lookupCountry)
    : { state: 'ok', detail: '当前浏览器已禁用 WebRTC API' };
  onStep('webrtc', webrtc);

  const all = [egress, region, webrtc, ipv6, dns, fp];
  const fails = all.filter((x) => x.state === 'fail').length;
  const warns = all.filter((x) => x.state === 'warn').length;
  return { fails, warns, total: all.length };
}
