// Apple HIG Dynamic Island & Control Center Platter pure view-model helpers
// Preserves security invariants, IP masking, full 14-check domain mapping, and zero em-dash copy.

export const POD_WIDTH = 480;
export const POD_COMPACT_HEIGHT = 64;
export const ORB_STAGE_SIZE = 72;
export const ORB_VISUAL_SIZE = 56;

export const FIXABLE_OPTIONAL = new Set(['时区', '地区', 'DNS', 'WebRTC', '生态可达性']);
export const HARD_FIXABLE = new Set(['代理内核', 'TUN网卡', 'IPv6', '保镖', '规则置顶']);

export const CHECK_SPECS = [
  { key: '代理内核', label: '代理内核', hard: true, domain: 'tunnel' },
  { key: 'TUN网卡', label: 'TUN 网卡', hard: true, domain: 'tunnel' },
  { key: '出口IP', label: '出口地区', hard: true, domain: 'egress' },
  { key: 'IPv6', label: 'IPv6 阻断', hard: true, domain: 'egress' },
  { key: '保镖', label: '常驻保镖', hard: true, domain: 'leak' },
  { key: '规则置顶', label: '置顶规则', hard: true, domain: 'tunnel' },
  { key: 'DNS', label: 'DNS 归属', hard: false, domain: 'leak' },
  { key: 'WebRTC', label: 'WebRTC', hard: false, domain: 'leak' },
  { key: '生态可达性', label: '服务连通', hard: false, domain: 'leak' },
  { key: 'IP一致', label: 'IP 稳定', hard: false, domain: 'egress' },
  { key: '客户端语言', label: '客户端语言', hard: false, domain: 'locale' },
  { key: 'ASN属性', label: 'ASN 属性', hard: false, domain: 'egress' },
  { key: '时区', label: '系统时区', hard: false, domain: 'locale' },
  { key: '地区', label: '区域格式', hard: false, domain: 'locale' }
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
  if (st === 'skip') return '已免检';

  if (spec.key === '出口IP') {
    if (st === 'ok') return countryTarget || '合规';
    const m = detail.match(/\b([A-Z]{2})\b/);
    return m ? `异常 ${m[1]}` : '异常';
  }
  if (spec.key === 'DNS') {
    if (detail.includes('解析器位于 ')) {
      const cc = detail.split('解析器位于 ').pop().trim().split(/[;；\s]+/)[0];
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
    if (/isp|住宅/i.test(detail)) return '住宅 ISP';
    if (/hosting|机房/i.test(detail)) return '机房';
    if (/content|cdn/i.test(detail)) return 'CDN';
    if (/education|edu/i.test(detail)) return '教育网';
    if (/business|enterprise/i.test(detail)) return '商业网';
    if (/未确认|unknown/i.test(detail)) return '待确认';
  }
  if (spec.key === '客户端语言') {
    const mismatch = detail.match(/Claude=([a-zA-Z-]+).*?期望\s*([a-zA-Z-]+)/i);
    if (mismatch) return `${mismatch[1]}≠${mismatch[2]}`;
    const okLang = detail.match(/Claude=([a-zA-Z-]+)/i);
    if (st === 'ok' && okLang) return okLang[1];
    if (/未能读取/.test(detail)) return '未读取';
    return st === 'ok' ? '匹配' : '不匹配';
  }
  if (spec.key === '时区' || spec.key === '地区') {
    if (st === 'ok') return '一致';
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
 * Builds the 2x2 Apple Control Center Platter Tiles covering all 14 checks across 4 domains:
 * 1. tunnel (代理内核 + TUN网卡 + 规则置顶)
 * 2. egress (出口IP + IPv6 + IP一致 + ASN属性)
 * 3. leak   (DNS + WebRTC + 生态可达性 + 保镖)
 * 4. locale (客户端语言 + 时区 + 地区)
 */
export function buildPlatterTiles(data, checkMap) {
  const get = (k) => checkMap.get(k) || { state: 'skip', detail: '' };
  const cc = data?.country_target || '--';
  const clientName = CLIENT_NAMES[data?.proxy_mode] || data?.proxy_mode || '未识别';

  // 1. Tunnel & Rules (3 hard checks)
  const cCore = get('代理内核');
  const cTun = get('TUN网卡');
  const cRule = get('规则置顶');
  const tunnelAllSkip = cCore.state === 'skip' && cTun.state === 'skip' && cRule.state === 'skip';
  const tunnelTone = tunnelAllSkip
    ? 'skip'
    : (cCore.state === 'ok' && cTun.state === 'ok' && cRule.state === 'ok' ? 'ok' : 'fail');
  const tunnelSub = tunnelTone === 'ok'
    ? 'TUN 接管 · 规则置顶'
    : tunnelTone === 'skip'
      ? '等待探测隧道'
      : cTun.state !== 'ok'
        ? 'TUN 网卡未就绪'
        : cCore.state !== 'ok'
          ? '代理内核未运行'
          : '分流置顶待验证';

  // 2. Egress & Network (出口IP + IPv6 + IP一致 + ASN属性)
  const cExit = get('出口IP');
  const cV6 = get('IPv6');
  const cStable = get('IP一致');
  const cAsn = get('ASN属性');
  const egressHardBad = cExit.state !== 'ok' || cV6.state !== 'ok';
  const egressAllSkip = [cExit, cV6, cStable, cAsn].every((x) => x.state === 'skip');
  const egressTone = egressAllSkip
    ? 'skip'
    : egressHardBad
      ? 'fail'
      : worstTone([cExit.state, cV6.state, cStable.state, cAsn.state]);
  const asnShort = conciseCheckValue({ key: 'ASN属性' }, cAsn.state, cAsn.detail, cc);
  const egressSub = egressAllSkip
    ? '等待出口探测'
    : cExit.state !== 'ok'
      ? '出口地区不符'
      : cV6.state !== 'ok'
        ? 'IPv6 尚未阻断'
        : cStable.state === 'warn' || cStable.state === 'fail'
          ? '出口 IP 近期变动'
          : cAsn.state === 'warn' || cAsn.state === 'fail'
            ? `IPv6 已关 · ${asnShort}节点`
            : 'IPv6 已关 · 链路纯净';

  // 3. Leak & Bodyguard (DNS + WebRTC + 生态可达性 + 保镖)
  const cDns = get('DNS');
  const cRtc = get('WebRTC');
  const cEco = get('生态可达性');
  const cGuard = get('保镖');
  const leakAllSkip = [cDns, cRtc, cEco, cGuard].every((x) => x.state === 'skip');
  const leakTone = leakAllSkip
    ? 'skip'
    : cGuard.state !== 'ok'
      ? 'fail'
      : worstTone([cDns.state, cRtc.state, cEco.state]);
  const dnsVal = conciseCheckValue({ key: 'DNS' }, cDns.state, cDns.detail, cc);
  const leakSub = leakAllSkip
    ? '等待泄漏与守护探测'
    : cGuard.state !== 'ok'
      ? '常驻保镖未激活'
      : cDns.state === 'fail' || cDns.state === 'warn'
        ? `DNS ${dnsVal} · 需关注`
        : cRtc.state === 'fail' || cRtc.state === 'warn'
          ? 'WebRTC 候选需关注'
          : cEco.state === 'fail' || cEco.state === 'warn'
            ? 'Claude 连通性波动'
            : `DNS ${dnsVal === '已免检' ? '免检' : dnsVal} · 保镖在线`;

  // 4. Locale & Language (客户端语言 + 时区 + 地区)
  const cLang = get('客户端语言');
  const cTz = get('时区');
  const cLoc = get('地区');
  const localeStates = [cLang.state, cTz.state, cLoc.state];
  const localeTone = worstTone(localeStates);
  const langVal = conciseCheckValue({ key: '客户端语言' }, cLang.state, cLang.detail, cc);
  const skipCount = localeStates.filter((s) => s === 'skip').length;
  const localeSub = localeTone === 'skip'
    ? '3 项均设为免检'
    : cLang.state === 'warn' || cLang.state === 'fail'
      ? `客户端 ${langVal}`
      : cTz.state === 'warn' || cTz.state === 'fail'
        ? '系统时区不匹配'
        : cLoc.state === 'warn' || cLoc.state === 'fail'
          ? '区域格式不匹配'
          : skipCount > 0
            ? `语言匹配 · ${skipCount} 项免检`
            : '时区、语言与格式一致';

  return [
    { id: 'tunnel', title: clientName, sub: tunnelSub, tone: tunnelTone },
    { id: 'egress', title: `出口 · ${cc}`, sub: egressSub, tone: egressTone },
    { id: 'leak', title: '防漏与保镖', sub: leakSub, tone: leakTone },
    { id: 'locale', title: '区域与语言', sub: localeSub, tone: localeTone }
  ];
}

/**
 * Generates human-readable guidance & contextual quick action for any check item.
 */
export function describeCheckGuidance(spec, st, rawDetail, countryTarget) {
  const detail = maskDisplayIp(rawDetail || '').trim();
  if (st === 'ok') {
    return {
      hint: detail || '该安全检查项已通过验证。',
      actionType: null,
      actionLabel: ''
    };
  }
  if (st === 'skip') {
    return {
      hint: '当前已在「设置」中设为免检跳过，不参与启动阻断。',
      actionType: 'goto-settings',
      actionLabel: '去设置'
    };
  }

  if (spec.key === '出口IP') {
    return {
      hint: `${detail || '当前出口国家与目标不一致'}。若已切换代理国家节点，可点右侧重新锁定目标地区。`,
      actionType: 'tool:set-region',
      actionLabel: '重锁地区'
    };
  }
  if (spec.key === '客户端语言') {
    return {
      hint: `${detail || '客户端语言与出口地区不一致'}。可在 Claude 设置切换为对应语言，或将此提示项设为免检。`,
      actionType: 'mute',
      actionLabel: '设为免检'
    };
  }
  if (spec.key === 'ASN属性') {
    return {
      hint: `${detail || '当前出口属于机房或 CDN 网络'}。非阻断参考项：追求原生住宅属性可换 ISP 节点，也可设为免检。`,
      actionType: 'mute',
      actionLabel: '设为免检'
    };
  }
  if (spec.key === 'IP一致') {
    return {
      hint: `${detail || '出口 IP 与上次记录不同'}。频繁跳 IP 易触发风控，建议固定代理节点；确认无误后重检即可。`,
      actionType: 'mute',
      actionLabel: '设为免检'
    };
  }
  if (spec.key === 'WebRTC') {
    if (st === 'warn' && /未收到|不可达|超时|timeout/i.test(detail)) {
      return {
        hint: `${detail}。当前代理节点可能拦截或未转发 UDP STUN，无本机 IP 暴露；可换支持 UDP 的节点或设为免检。`,
        actionType: 'mute',
        actionLabel: '设为免检'
      };
    }
  }
  if (HARD_FIXABLE.has(spec.key) || FIXABLE_OPTIONAL.has(spec.key)) {
    const fixableNow = st === 'fail' || String(rawDetail || '').startsWith('非关键失败已忽略');
    return {
      hint: `${detail || '检测到配置偏差'}。${fixableNow ? '可点击右侧执行自动修复' : '非阻断提示项，可在设置中调整免检策略'}。`,
      actionType: fixableNow ? 'fix' : 'mute',
      actionLabel: fixableNow ? '一键修复' : '设为免检'
    };
  }
  return {
    hint: detail || `状态：${countryTarget || '--'} 待处理`,
    actionType: null,
    actionLabel: ''
  };
}

/**
 * Builds top-priority Anomaly Callout cards for all checks in `fail` or `warn` state.
 */
export function buildAnomalyCallouts(data, checkMap) {
  if (!checkMap || checkMap.size === 0) return [];
  const cc = data?.country_target || '--';
  const items = [];
  for (const spec of CHECK_SPECS) {
    const found = checkMap.get(spec.key);
    if (!found) continue;
    const st = found.state;
    if (st !== 'fail' && st !== 'warn') continue;
    const badge = conciseCheckValue(spec, st, found.detail, cc);
    const guide = describeCheckGuidance(spec, st, found.detail, cc);
    items.push({
      key: spec.key,
      label: spec.label,
      hard: spec.hard,
      state: st,
      badge,
      hint: guide.hint,
      actionType: guide.actionType,
      actionLabel: guide.actionLabel
    });
  }
  // Sort hard/fail items ahead of optional warnings
  items.sort((a, b) => {
    const score = (x) => (x.state === 'fail' ? 0 : 1) * 10 + (x.hard ? 0 : 1);
    return score(a) - score(b);
  });
  return items;
}

/**
 * Summarizes the 14 checks for the collapsible matrix bar.
 */
export function summarizeMatrixCounts(checkMap) {
  let ok = 0;
  let warn = 0;
  let fail = 0;
  let skip = 0;
  for (const spec of CHECK_SPECS) {
    const st = checkMap?.get(spec.key)?.state || 'skip';
    if (st === 'ok') ok++;
    else if (st === 'warn') warn++;
    else if (st === 'fail') fail++;
    else skip++;
  }
  const parts = [`${ok} 正常`];
  if (fail > 0) parts.push(`${fail} 未过`);
  if (warn > 0) parts.push(`${warn} 提示`);
  if (skip > 0) parts.push(`${skip} 免检`);
  return { ok, warn, fail, skip, summaryText: parts.join(' · ') };
}

/**
 * Normalizes a user-typed or pasted URL for the isolated safe browser.
 * Auto-prepends https:// for bare domains like "claude.ai/login".
 */
export function normalizeSafeUrl(rawInput) {
  const trimmed = String(rawInput || '').trim();
  if (!trimmed) return { ok: false, url: '', reason: '请输入或粘贴要隔离打开的链接' };
  if (/[\s"<>`']/.test(trimmed)) {
    return { ok: false, url: '', reason: '链接包含非法空白或引号字符' };
  }
  if (/^https?:\/\/[^/\s]+\S*$/i.test(trimmed)) {
    return { ok: true, url: trimmed, reason: '' };
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) {
    return { ok: false, url: '', reason: '仅支持 https:// 或 http:// 网页链接' };
  }
  if (/^[a-z0-9-]+(\.[a-z0-9-]+)+([/?#]\S*)?$/i.test(trimmed)) {
    return { ok: true, url: `https://${trimmed}`, reason: '' };
  }
  return { ok: false, url: '', reason: '请输入有效的网址（如 claude.ai）' };
}

/**
 * Computes the Compact Dynamic Island primary button role according to Apple HIG:
 * - Busy: muted disabled progress label
 * - Ready: System Green (#30D158) "启动" -> launches Claude Desktop
 * - Blocked + Fixable: System Amber (#FFD60A) "修复" -> runs 1-click fix directly!
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

/**
 * Formats the JSON payload from `check_app_update` into structured UI drawer state.
 */
export function formatUpdateCheckReport(info) {
  if (!info || typeof info !== 'object') {
    return {
      title: '检查更新 · 异常',
      canHotUpdate: false,
      summary: '未能读取更新状态',
      body: '未收到有效的更新检查结果，请稍后重试。'
    };
  }

  const st = String(info.status || 'error');
  const repo = info.repo || 'Javaispythonsb/claude-envguard';
  const curVer = info.current_version || '2.0.0';
  const curSha = info.current_commit && info.current_commit !== 'unknown' ? ` (${info.current_commit})` : '';
  const latVer = info.latest_version || curVer;
  const latSha = info.latest_commit ? ` (${info.latest_commit})` : '';

  if (st === 'update_available') {
    const lines = [
      `状态：发现可用热更新`,
      `本机版本：v${curVer}${curSha}`,
      `最新版本：v${latVer}${latSha}`
    ];
    if (info.notes) lines.push(`更新内容：${info.notes}`);
    if (info.built_at) lines.push(`构建时间：${info.built_at}`);
    lines.push(`开源仓库：https://github.com/${repo}`);
    lines.push('');
    lines.push('点击右上角「立即热更新」将自动校验 SHA-256 签名，保留全部本机设置后无缝替换重启。');
    return {
      title: '检查更新 · 发现新版',
      canHotUpdate: true,
      summary: info.message || `发现 GitHub 新版本 v${latVer}${latSha}`,
      body: lines.join('\n')
    };
  }

  if (st === 'up_to_date') {
    const lines = [
      `状态：当前已是最新版本`,
      `本机版本：v${curVer}${curSha}`,
      `开源仓库：https://github.com/${repo}`
    ];
    if (info.built_at) lines.push(`最新构建：${info.built_at}`);
    if (info.ci_building && info.ci_head_commit) {
      lines.push('');
      lines.push(`提示：检测到 main 分支刚推送新提交 (${info.ci_head_commit}${info.ci_head_title ? ': ' + info.ci_head_title : ''})，GitHub Actions 正在后台编译打包，约 1~2 分钟后再次检查即可拉取热更新。`);
    }
    return {
      title: info.ci_building ? '检查更新 · 云端同步中' : '检查更新 · 已是最新',
      canHotUpdate: false,
      summary: info.message || `当前已是最新版本 v${curVer}${curSha}`,
      body: lines.join('\n')
    };
  }

  if (st === 'building') {
    const lines = [
      `状态：GitHub Actions 正在编译打包首个 Release`,
      `本机版本：v${curVer}${curSha}`,
      `最新提交：${info.latest_commit || '--'}${info.notes ? ' · ' + info.notes : ''}`,
      `构建进度：https://github.com/${repo}/actions`,
      '',
      '云端编译完成后（约 1~2 分钟），再次点击「检查更新」即可一键热更新。'
    ];
    return {
      title: '检查更新 · 云端编译中',
      canHotUpdate: false,
      summary: info.message || 'GitHub Actions 正在后台编译打包中',
      body: lines.join('\n')
    };
  }

  if (st === 'ahead') {
    const lines = [
      `状态：本地构建新于云端 Release（防倒退保护已生效）`,
      `本机版本：v${curVer}${curSha}`,
      `云端构建：v${latVer}${latSha}`,
      `开源仓库：https://github.com/${repo}`
    ];
    if (info.ci_building) {
      lines.push('');
      lines.push('GitHub Actions 正在编译最新提交，请等待云端构建完成后再检查。');
    }
    return {
      title: '检查更新 · 本地超前',
      canHotUpdate: false,
      summary: info.message || '当前本地版本新于云端 Release',
      body: lines.join('\n')
    };
  }

  return {
    title: '检查更新 · 失败',
    canHotUpdate: false,
    summary: info.message || '无法连接 GitHub 更新源',
    body: `${info.message || '检查更新失败'}\n开源仓库：https://github.com/${repo}`
  };
}

/**
 * Computes accessible label and tooltip copy for the Ceramic Shield Orb across standby & gravity play modes.
 */
export function describeOrbModeMeta({ tone = '', playMode = false } = {}) {
  const statusPrefix = tone === 'blocked'
    ? '环境存在阻断项'
    : tone === 'auditing'
      ? '正在审计环境'
      : '环境安全';

  if (playMode) {
    return {
      ariaLabel: `${statusPrefix}，重力解压球模式运行中，单击弹跳，双击或按 Esc 退出`,
      title: `${statusPrefix} · 重力解压模式（抓甩 / 挥鼠颠球 / 单击弹跳 · 双击退出）`
    };
  }

  return {
    ariaLabel: `${statusPrefix}，鼠标悬停或按回车展开控制面板，双击进入重力解压模式`,
    title: `${statusPrefix} · 悬停展开 · 按住拖动 · 双击进入重力解压球`
  };
}

/**
 * Computes GPU-only elastic squash & stretch scale factors from normal impact velocity (px/s).
 */
export function computeImpactSquash(normalSpeed, axis = 'y') {
  const speed = Math.max(0, Number(normalSpeed) || 0);
  const deform = Math.min(0.22, Math.max(0.04, speed / 2500));
  const rebound = Number((deform * 0.38).toFixed(3));
  const d = Number(deform.toFixed(3));

  if (axis === 'x') {
    return {
      compress: { sx: Number((1 - d).toFixed(3)), sy: Number((1 + d).toFixed(3)) },
      rebound: { sx: Number((1 + rebound).toFixed(3)), sy: Number((1 - rebound).toFixed(3)) }
    };
  }
  return {
    compress: { sx: Number((1 + d).toFixed(3)), sy: Number((1 - d).toFixed(3)) },
    rebound: { sx: Number((1 - rebound).toFixed(3)), sy: Number((1 + rebound).toFixed(3)) }
  };
}
