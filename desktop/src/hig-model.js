// Apple HIG Dynamic Island & Control Center Platter pure view-model helpers
// Strictly preserves security invariants, IP masking, and zero em-dash copy.

export const POD_WIDTH = 480;
export const POD_COMPACT_HEIGHT = 64;

export const FIXABLE_OPTIONAL = new Set(['时区', '地区', 'DNS', 'WebRTC', '生态可达性']);

export const CHECK_SPECS = [
  { key: '代理内核', label: '代理内核', hard: true },
  { key: 'TUN网卡', label: 'TUN 网卡', hard: true },
  { key: '出口IP', label: '出口地区', hard: true },
  { key: 'IPv6', label: 'IPv6 阻断', hard: true },
  { key: '保镖', label: '常驻保镖', hard: true },
  { key: '规则置顶', label: '置顶规则', hard: true },
  { key: 'DNS', label: 'DNS 归属', hard: false },
  { key: 'WebRTC', label: 'WebRTC', hard: false },
  { key: '生态可达性', label: '服务连通', hard: false },
  { key: 'IP一致', label: 'IP 稳定', hard: false },
  { key: '客户端语言', label: '客户端语言', hard: false },
  { key: 'ASN属性', label: 'ASN 属性', hard: false },
  { key: '时区', label: '系统时区', hard: false },
  { key: '地区', label: '区域格式', hard: false }
];

export const OPTIONAL_ITEMS = [
  { key: 'DNS', title: 'DNS 归属' },
  { key: 'WebRTC', title: 'WebRTC' },
  { key: '生态可达性', title: '服务连通' },
  { key: 'IP一致', title: 'IP 稳定' },
  { key: '客户端语言', title: '客户端语言' },
  { key: 'ASN属性', title: 'ASN 属性' },
  { key: '时区', title: '系统时区' },
  { key: '地区', title: '区域格式' }
];

export const CLIENT_NAMES = {
  'clash-verge': 'Clash Verge',
  flclash: 'FlClash',
  'sing-box': 'sing-box',
  clashx: 'ClashX',
  surge: 'Surge',
  wireguard: 'WireGuard',
  other: '其他 TUN',
  auto: '自动识别'
};

export function maskDisplayIp(text) {
  return String(text || '').replace(
    /\b(?!198\.1[89]\.)(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}\b/g,
    '$1.$2.*.*'
  );
}

/**
 * Derives concise right-aligned value text for the 7x2 Inset Grouped check row.
 */
export function conciseCheckValue(spec, st, rawDetail, countryTarget) {
  const detail = maskDisplayIp(rawDetail || '').trim();
  if (st === 'skip') return '已跳过';

  if (spec.key === '出口IP') {
    if (st === 'ok') return countryTarget || '合规';
    const m = detail.match(/\b([A-Z]{2})\b/);
    return m ? `异常 ${m[1]}` : '异常';
  }
  if (spec.key === 'DNS') {
    if (detail.includes('解析器位于 ')) {
      const cc = detail.split('解析器位于 ').pop().trim().split(/\s+/)[0];
      if (cc) return cc;
    }
    return st === 'ok' ? '海外' : st === 'warn' ? '待查' : '泄漏';
  }
  if (spec.key === 'IPv6') {
    return st === 'ok' ? '已关' : '暴露';
  }
  if (spec.key === 'TUN网卡') {
    return st === 'ok' ? '已接管' : '未启用';
  }
  if (spec.key === '保镖') {
    return st === 'ok' ? '守护中' : st === 'warn' ? '待升级' : '离线';
  }
  if (spec.key === '规则置顶') {
    if (st === 'ok') return '已生效';
    const reason = detail.match(/[（(]([^）)]+)[）)]/);
    return reason ? reason[1].slice(0, 6) : '未验证';
  }
  if (spec.key === 'ASN属性') {
    if (detail.includes('ISP') || detail.includes('住宅')) return 'ISP';
    if (detail.includes('机房') || detail.includes('hosting')) return '机房';
  }
  if (st === 'ok') return '正常';
  if (st === 'warn') return detail.startsWith('非关键失败已忽略') ? '已降级' : '提示';
  if (st === 'fail') return '未通过';
  return '待检';
}

function worstTone(states) {
  if (states.includes('fail')) return 'fail';
  if (states.includes('warn')) return 'warn';
  if (states.every((s) => s === 'skip')) return 'skip';
  return 'ok';
}

/**
 * Builds the 2x2 Apple Control Center Platter Tiles summarizing live telemetry.
 */
export function buildPlatterTiles(data, checkMap) {
  const get = (k) => checkMap.get(k) || { state: 'skip', detail: '' };
  const cc = data?.country_target || '--';
  const clientName = CLIENT_NAMES[data?.proxy_mode] || data?.proxy_mode || '未识别';

  const cCore = get('代理内核');
  const cTun = get('TUN网卡');
  const cRule = get('规则置顶');
  const tunnelTone = (cCore.state !== 'ok' || cTun.state !== 'ok' || cRule.state !== 'ok')
    ? (cCore.state === 'skip' && cTun.state === 'skip' ? 'skip' : 'fail')
    : 'ok';
  const tunnelSub = tunnelTone === 'ok'
    ? 'TUN 接管 · 规则置顶'
    : cTun.state !== 'ok'
      ? 'TUN 未就绪'
      : cCore.state !== 'ok'
        ? '代理内核未运行'
        : '分流规则待验证';

  const cExit = get('出口IP');
  const cV6 = get('IPv6');
  const cStable = get('IP一致');
  const egressTone = (cExit.state !== 'ok' || cV6.state !== 'ok')
    ? (cExit.state === 'skip' && cV6.state === 'skip' ? 'skip' : 'fail')
    : cStable.state === 'warn'
      ? 'warn'
      : 'ok';
  const egressSub = cExit.state !== 'ok'
    ? (cExit.state === 'skip' ? '等待出口探测' : '出口地区不符')
    : cV6.state !== 'ok'
      ? 'IPv6 尚未阻断'
      : cStable.state === 'warn'
        ? 'IP 近期发生变动'
        : 'IPv6 已关 · 链路纯净';

  const cDns = get('DNS');
  const cRtc = get('WebRTC');
  const leakTone = worstTone([cDns.state, cRtc.state]);
  const dnsVal = conciseCheckValue({ key: 'DNS' }, cDns.state, cDns.detail, cc);
  const leakSub = leakTone === 'ok'
    ? `DNS ${dnsVal} · STUN 无暴露`
    : leakTone === 'skip'
      ? '防漏项已设为跳过'
      : cDns.state === 'fail' || cDns.state === 'warn'
        ? `DNS ${dnsVal} · 需关注`
        : 'WebRTC 候选需关注';

  const cGuard = get('保镖');
  const cEco = get('生态可达性');
  const guardTone = cGuard.state !== 'ok'
    ? (cGuard.state === 'skip' ? 'skip' : 'fail')
    : cEco.state === 'fail' || cEco.state === 'warn'
      ? 'warn'
      : 'ok';
  const guardTitle = cGuard.state === 'ok' ? '保镖守护中' : cGuard.state === 'warn' ? '保镖待升级' : '保镖未运行';
  const guardSub = cGuard.state === 'ok'
    ? (cEco.state === 'ok' ? '10s 巡检 · 服务连通' : '10s 掉线急停巡检中')
    : '进程急停保护未激活';

  return [
    { id: 'tunnel', title: clientName, sub: tunnelSub, tone: tunnelTone },
    { id: 'egress', title: `出口 · ${cc}`, sub: egressSub, tone: egressTone },
    { id: 'leak', title: 'DNS 与 WebRTC', sub: leakSub, tone: leakTone },
    { id: 'guard', title: guardTitle, sub: guardSub, tone: guardTone }
  ];
}

/**
 * Computes the Compact Dynamic Island primary button role according to Apple HIG:
 * - Busy: muted disabled progress label
 * - Ready: System Green (#30D158) "启动" -> launches Claude Desktop
 * - Blocked + Fixable: System Amber (#FFD600) "修复" -> runs 1-click fix directly!
 * - Blocked + Unfixable: Muted Red "已锁定" -> disabled
 * - Incomplete initial load: Muted "审计中…" -> disabled
 */
export function computePrimaryIslandAction({ busy, busyLabel, complete, ready, needsFix }) {
  if (busy) {
    return {
      mode: 'busy',
      text: busyLabel || '处理中…',
      disabled: true,
      tone: 'muted',
      title: '正在执行操作…'
    };
  }
  if (ready) {
    return {
      mode: 'launch',
      text: '启动',
      disabled: false,
      tone: 'green',
      title: '环境已通过安全门禁，启动 Claude 桌面端'
    };
  }
  if (complete && needsFix) {
    return {
      mode: 'fix',
      text: '修复',
      disabled: false,
      tone: 'amber',
      title: '检测到可自动修复的阻断项，点击一键修复'
    };
  }
  if (complete && !ready) {
    return {
      mode: 'blocked',
      text: '已锁定',
      disabled: true,
      tone: 'red',
      title: '存在需手动处理的硬性安全项，已阻止启动'
    };
  }
  return {
    mode: 'auditing',
    text: '审计中…',
    disabled: true,
    tone: 'muted',
    title: '正在执行启动前安全审计…'
  };
}
