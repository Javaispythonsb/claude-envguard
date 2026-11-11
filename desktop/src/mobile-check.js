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

async function checkWebRtc(exitIp) {
  const RTCPeer = window.RTCPeerConnection || window.webkitRTCPeerConnection;
  if (!RTCPeer) {
    return { state: 'ok', detail: '当前浏览器已禁用 WebRTC API' };
  }
  const ips = new Set();
  let hasMdns = false;

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
      const c = ev.candidate.candidate || '';
      if (c.includes('.local')) hasMdns = true;
      const ipv4Matches = c.match(/\b(?:\d{1,3}\.){3}\d{1,3}\b/g) || [];
      for (const ip of ipv4Matches) {
        if (ip === '0.0.0.0' || ip.startsWith('127.') || ip.startsWith('10.') || ip.startsWith('192.168.') || /^172\.(1[6-9]|2\d|3[0-1])\./.test(ip)) {
          continue;
        }
        ips.add(ip);
      }
    };
    try {
      pc.createDataChannel('eg');
      pc.createOffer().then((o) => pc.setLocalDescription(o)).catch(finish);
    } catch {
      clearTimeout(timer);
      finish();
    }
  });

  if (ips.size === 0) {
    return { state: 'ok', detail: hasMdns ? '未暴露公网候选（mDNS 混淆已生效）' : 'STUN 无额外公网候选暴露' };
  }
  const list = [...ips];
  const fakeIpOnly = list.every((ip) => ip.startsWith('198.18.'));
  if (fakeIpOnly) {
    return { state: 'ok', detail: '仅返回代理 Fake-IP (198.18.*)' };
  }
  const leaked = exitIp ? list.filter((ip) => ip !== exitIp) : list;
  if (leaked.length > 0) {
    return { state: 'fail', detail: `发现非代理出口候选: ${leaked.map(maskIp).join(', ')}` };
  }
  return { state: 'ok', detail: `候选地址与代理出口一致 (${maskIp(exitIp)})` };
}

async function checkIpv6(egressCountry) {
  try {
    const r = await fetchWithTimeout('https://api6.ipify.org?format=json', 3200);
    if (!r.ok) return { state: 'ok', detail: '未检测到 IPv6 直连通道（无双栈旁路风险）' };
    const j = await r.json();
    const v6 = String(j.ip || '');
    if (!v6.includes(':')) return { state: 'ok', detail: '未启用公网 IPv6' };

    try {
      const infoRes = await fetchWithTimeout(`https://ipinfo.io/${encodeURIComponent(v6)}/json`, 3500);
      if (infoRes.ok) {
        const info = await infoRes.json();
        const v6cc = String(info.country || '').toUpperCase();
        if (v6cc === 'CN' || (egressCountry && v6cc && v6cc !== egressCountry)) {
          return { state: 'fail', detail: `IPv6 旁路泄漏! 归属 ${v6cc} (${maskIp(v6)})` };
        }
        return { state: 'ok', detail: `IPv6 已走同区代理 (${v6cc} · ${maskIp(v6)})` };
      }
    } catch {}
    return { state: 'warn', detail: `检测到公网 IPv6 (${maskIp(v6)})，请确认已走代理` };
  } catch {
    return { state: 'ok', detail: 'IPv6 未通或已关闭（无双栈旁路泄漏）' };
  }
}

async function checkDoh() {
  const results = await Promise.allSettled([
    fetchWithTimeout('https://1.1.1.1/cdn-cgi/trace', 3500),
    fetchWithTimeout('https://dns.google/resolve?name=claude.ai&type=A', 3500)
  ]);
  const cfOk = results[0].status === 'fulfilled' && results[0].value.ok;
  const ggOk = results[1].status === 'fulfilled' && results[1].value.ok;
  if (cfOk && ggOk) return { state: 'ok', detail: 'Cloudflare 1.1.1.1 与 Google DoH 均可达' };
  if (cfOk || ggOk) return { state: 'ok', detail: `${cfOk ? 'Cloudflare' : 'Google'} DoH 加密解析可达` };
  return { state: 'warn', detail: '公共 DoH 不可达（受当前代理分流影响）' };
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

  const [webrtc, ipv6, doh, fp] = await Promise.all([
    checkWebRtc(egress.ip).then((r) => { onStep('webrtc', r); return r; }),
    checkIpv6(egress.country).then((r) => { onStep('ipv6', r); return r; }),
    checkDoh().then((r) => { onStep('doh', r); return r; }),
    checkFingerprint().then((r) => { onStep('fp', r); return r; })
  ]);

  const all = [egress, region, webrtc, ipv6, doh, fp];
  const fails = all.filter((x) => x.state === 'fail').length;
  const warns = all.filter((x) => x.state === 'warn').length;
  return { fails, warns, total: all.length };
}
