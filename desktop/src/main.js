import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { runMobileSelfCheck } from './mobile-check.js';

const CHECK_SPECS = [
  { key: '代理内核', label: '代理内核', hard: true },
  { key: 'TUN网卡', label: 'TUN 网卡', hard: true },
  { key: '出口IP', label: '出口地区', hard: true },
  { key: 'IPv6', label: 'IPv6 关闭', hard: true },
  { key: '保镖', label: '常驻保镖', hard: true },
  { key: '规则置顶', label: '置顶规则', hard: true },
  { key: '时区', label: '系统时区', hard: false },
  { key: '地区', label: '区域格式', hard: false },
  { key: 'IP一致', label: 'IP 稳定性', hard: false },
  { key: 'DNS', label: 'DNS 归属', hard: false },
  { key: 'WebRTC', label: 'WebRTC', hard: false },
  { key: '客户端语言', label: '客户端语言', hard: false },
  { key: 'ASN属性', label: 'ASN 属性', hard: false },
  { key: '生态可达性', label: '服务连通', hard: false }
];

const OPTIONAL_ITEMS = [
  { key: '时区', title: '系统时区', hint: '关 = 保留本机时间' },
  { key: '地区', title: '区域格式', hint: '关 = 保留日期格式' },
  { key: 'DNS', title: 'DNS 归属', hint: '校验上游解析器' },
  { key: 'WebRTC', title: 'WebRTC', hint: 'STUN 候选探测' },
  { key: 'IP一致', title: 'IP 稳定性', hint: '48h 跳变提醒' },
  { key: 'ASN属性', title: 'ASN 属性', hint: '运营商网络类型' },
  { key: '客户端语言', title: '客户端语言', hint: '比对配置语言' },
  { key: '生态可达性', title: '服务连通', hint: '网页与 API 可达' }
];

const SHORT_TZ_MAP = {
  'China Standard Time': 'UTC+8 北京',
  'Pacific Standard Time': 'UTC-8 美西',
  'Mountain Standard Time': 'UTC-7 美山',
  'Central Standard Time': 'UTC-6 美中',
  'Eastern Standard Time': 'UTC-5 美东',
  'Tokyo Standard Time': 'UTC+9 东京',
  'Korea Standard Time': 'UTC+9 首尔',
  'Taipei Standard Time': 'UTC+8 台北',
  'Singapore Standard Time': 'UTC+8 新加坡',
  'GMT Standard Time': 'UTC+0 伦敦',
  'W. Europe Standard Time': 'UTC+1 柏林',
  'Romance Standard Time': 'UTC+1 巴黎',
  'AUS Eastern Standard Time': 'UTC+10 悉尼'
};

const isTauri = Boolean(window.__TAURI_INTERNALS__);
let isBusy = false;
let currentState = null;

function setTheme(theme) {
  const valid = theme === 'cyber' ? 'cyber' : 'terminal';
  document.documentElement.setAttribute('data-theme', valid);
  try { localStorage.setItem('envguard-theme', valid); } catch {}
  document.querySelectorAll('[data-theme-pick]').forEach((btn) => {
    btn.classList.toggle('on', btn.dataset.themePick === valid);
  });
}

function setDeviceMode(mode) {
  document.documentElement.setAttribute('data-device', mode);
  const backBtn = document.getElementById('btnBackToPc');
  if (backBtn) backBtn.style.display = isTauri ? '' : 'none';
}

function setView(viewName) {
  document.querySelectorAll('.view').forEach((sec) => {
    sec.classList.toggle('active', sec.dataset.view === viewName);
  });
  document.querySelectorAll('.nav-tabs .tab').forEach((tab) => {
    const active = tab.dataset.view === viewName;
    tab.classList.toggle('active', active);
    tab.setAttribute('aria-selected', String(active));
  });
}

function setBanner(text, isErr = false) {
  const el = document.getElementById('statusBanner');
  if (!el) return;
  el.textContent = text;
  el.title = text;
  el.classList.toggle('err', isErr);
}

function syncPortVisibility(mode) {
  const wrap = document.getElementById('flclashPortWrap');
  if (wrap) wrap.hidden = !(mode === 'flclash' || mode === 'auto');
}

function setBusyState(busy, label = '') {
  isBusy = busy;
  for (const id of ['btnRecheck', 'btnFix', 'btnShieldRestore', 'proxyClientSelect', 'btnSavePort', 'btnSaveSettings', 'btnOpenSafe']) {
    const el = document.getElementById(id);
    if (el) el.disabled = busy;
  }
  const complete = (currentState?.checks || []).length === CHECK_SPECS.length;
  const ready = Boolean(currentState?.environmentReady);
  const canLaunch = !busy && ready;
  const dBtn = document.getElementById('btnLaunchDesktop');
  const cBtn = document.getElementById('btnLaunchCli');
  if (dBtn) {
    dBtn.disabled = !canLaunch;
    dBtn.classList.toggle('blocked', !busy && complete && !ready);
    if (busy && label) {
      dBtn.textContent = label;
    } else if (canLaunch) {
      dBtn.textContent = '启动 Claude';
    } else if (complete && !ready) {
      dBtn.textContent = '门禁锁定';
    } else {
      dBtn.textContent = '等待审计';
    }
  }
  if (cBtn) cBtn.disabled = !canLaunch;
}

function stateBadgeText(st) {
  if (st === 'ok') return 'OK';
  if (st === 'warn') return 'WARN';
  if (st === 'skip') return 'SKIP';
  if (st === 'fail') return 'FAIL';
  return 'WAIT';
}

function maskDisplayIp(text) {
  return String(text || '').replace(/\b(?!198\.1[89]\.)(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}\b/g, '$1.$2.*.*');
}

function compactDetail(key, st, raw, countryTarget = '') {
  const s = maskDisplayIp(String(raw || '').trim());
  if (!s) return st === 'ok' ? '正常' : st === 'skip' ? '已跳过' : '--';
  if (s === '用户已禁用') return '已跳过';
  if (s === '仅暴露代理IP') return '与代理一致';
  if (s === '在线(10s巡检)') return '10s 巡检';
  if (s.endsWith(' 运行中')) return s.slice(0, -4);
  if (key === '出口IP' && st === 'ok') {
    return countryTarget ? `${countryTarget} · ${s}` : s;
  }
  if (s.startsWith('Fake-IP走隧道 · 解析器位于 ')) {
    return `Fake-IP · ${s.replace('Fake-IP走隧道 · 解析器位于 ', '')}`;
  }
  const asnMatch = s.match(/^([A-Za-z/]+)\((AS\d+\s+[^;)]+)/);
  if (asnMatch) {
    const org = asnMatch[2].replace(/,?\s*(Inc\.|LLC|Ltd\.?|Corp\.?)$/i, '').trim();
    return `${org} · ${asnMatch[1]}`;
  }
  if (s.startsWith('Claude=')) {
    return s.replace(/^Claude=/, '').replace(/,期望\s*/, ' · 期望 ');
  }
  if (s.startsWith('在线(含72h')) return '旧策略 · 需点修复';
  return s;
}

function renderCheckSkeleton() {
  const grid = document.getElementById('checkListGrid');
  const meter = document.getElementById('heroMeter');
  if (!grid || !meter) return;
  grid.innerHTML = CHECK_SPECS.map((spec) => `
    <div class="citem" data-ck="${spec.key}">
      <span class="nm">${spec.label}${spec.hard ? '<span class="hard">必检</span>' : ''}</span>
      <span class="dt">--</span>
      <span class="st skip">WAIT</span>
    </div>
  `).join('');
  meter.innerHTML = CHECK_SPECS.map(() => '<i class="seg"></i>').join('');
}

function setToggle(id, isOn) {
  const el = document.getElementById(id);
  if (!el) return;
  const on = Boolean(isOn);
  el.classList.toggle('on', on);
  el.setAttribute('aria-checked', String(on));
  el.textContent = on ? 'ON' : 'OFF';
}

function renderOptionalToggles(disabledList = []) {
  const box = document.getElementById('optionalChecksContainer');
  if (!box) return;
  const disabled = new Set(disabledList);
  box.innerHTML = OPTIONAL_ITEMS.map((item) => {
    const on = !disabled.has(item.key);
    return `
      <div class="optrow">
        <div class="opt-txt">
          <span class="ol">${item.title}</span>
          <span class="ohint">${item.hint}</span>
        </div>
        <button class="tg ${on ? 'on' : ''}" type="button" role="switch" aria-checked="${on}" data-opt-key="${item.key}">${on ? 'ON' : 'OFF'}</button>
      </div>
    `;
  }).join('');
  box.querySelectorAll('[data-opt-key]').forEach((btn) => {
    btn.onclick = () => {
      const next = !btn.classList.contains('on');
      btn.classList.toggle('on', next);
      btn.setAttribute('aria-checked', String(next));
      btn.textContent = next ? 'ON' : 'OFF';
    };
  });
}

function applyUiState(data, elapsedMs = 0) {
  if (!data) return;
  document.getElementById('appVersionTag').textContent = data.version && data.version !== 'unknown' ? `v${data.version}` : 'v2.0';
  document.getElementById('chipCountry').textContent = data.country_target || '--';

  const tzEl = document.getElementById('chipSystemTz');
  const rawTz = data.system_tz || '';
  tzEl.textContent = SHORT_TZ_MAP[rawTz] || rawTz || '--';
  tzEl.title = rawTz ? `系统时区: ${rawTz} | 目标: ${data.iana_tz_target || '--'}` : '';

  const sel = document.getElementById('proxyClientSelect');
  if (sel && data.proxy_mode) {
    sel.value = data.proxy_mode;
    syncPortVisibility(data.proxy_mode);
  }
  const portInput = document.getElementById('flclashPortInput');
  if (portInput) portInput.value = data.flclash_port && data.flclash_port !== 'auto' ? data.flclash_port : '';

  // Topbar bodyguard pill
  const bg = data.bodyguard || {};
  const gDot = document.getElementById('guardDot');
  const gText = document.getElementById('guardText');
  const gPill = document.getElementById('guardPill');
  gDot.className = 'dot ' + (bg.state === 'ok' ? '' : bg.state === 'warn' ? 'warn' : 'fail');
  gText.textContent = bg.state === 'ok' ? '保镖在线' : bg.state === 'warn' ? '保镖待升级' : '保镖离线';
  gPill.title = bg.detail ? `保镖状态：${bg.detail}（10s 进程级急停）` : '保镖状态';

  // Settings sync
  const cfg = data.check_config || {};
  setToggle('tgAllowOptional', Boolean(cfg.allowOptionalFailures));
  setToggle('tgLogging', cfg.logging !== false);
  renderOptionalToggles(cfg.disabled || []);

  // Checks matrix
  const map = new Map();
  for (const item of data.checks || []) {
    map.set(item.name, item);
  }
  const segs = document.querySelectorAll('#heroMeter .seg');
  let okCount = 0;
  let warnCount = 0;
  let failCount = 0;
  let skipCount = 0;
  const blockedNames = [];

  CHECK_SPECS.forEach((spec, idx) => {
    const row = document.querySelector(`.citem[data-ck="${spec.key}"]`);
    const found = map.get(spec.key);
    const st = found ? found.state : 'skip';
    const rawDetail = found ? found.detail || '' : '';
    const shortDetail = found ? compactDetail(spec.key, st, rawDetail, data.country_target) : '待检';
    if (row) {
      const dtEl = row.querySelector('.dt');
      dtEl.textContent = shortDetail;
      dtEl.title = maskDisplayIp(rawDetail) || shortDetail;
      const badge = row.querySelector('.st');
      badge.className = `st ${st}`;
      badge.textContent = stateBadgeText(st);
    }
    if (segs[idx]) {
      segs[idx].className = `seg ${st === 'ok' ? 'ok' : st === 'warn' ? 'warn' : st === 'fail' ? 'fail' : ''}`;
    }
    if (st === 'ok') okCount++;
    else if (st === 'warn') { warnCount++; if (spec.hard) blockedNames.push(spec.label); }
    else if (st === 'fail') { failCount++; blockedNames.push(spec.label); }
    else { skipCount++; if (spec.hard) blockedNames.push(spec.label); }
  });

  const complete = (data.checks || []).length === CHECK_SPECS.length;
  const ready = typeof data.ready === 'boolean' ? (complete && data.ready) : (complete && blockedNames.length === 0);
  currentState = { ...data, environmentReady: ready };

  const heroIcon = document.getElementById('heroIcon');
  const heroTitle = document.getElementById('heroTitle');
  const heroDesc = document.getElementById('heroDesc');
  const heroPill = document.getElementById('heroPill');
  const auditMeta = document.getElementById('auditMeta');

  auditMeta.textContent = elapsedMs > 0 ? `${(elapsedMs / 1000).toFixed(1)}s` : '缓存';

  if (!complete) {
    heroIcon.className = 'sicon warn';
    heroIcon.textContent = '…';
    heroTitle.textContent = '等待审计';
    heroDesc.textContent = '点击右侧「重检」完成检查';
    heroPill.className = 'pill warn';
    heroPill.textContent = 'WAIT';
  } else if (!ready) {
    heroIcon.className = 'sicon blocked';
    heroIcon.textContent = '✕';
    heroTitle.textContent = `未通过：${blockedNames.slice(0, 3).join('、')}`;
    heroDesc.textContent = '点「修复」自动处理，或在「设置」关闭可选项';
    heroPill.className = 'pill blocked';
    heroPill.textContent = `${blockedNames.length} 阻断`;
  } else if (warnCount > 0 || skipCount > 0) {
    heroIcon.className = 'sicon';
    heroIcon.textContent = '✓';
    heroTitle.textContent = '环境就绪';
    const parts = [`${okCount} 通过`];
    if (warnCount) parts.push(`${warnCount} 提示`);
    if (skipCount) parts.push(`${skipCount} 跳过`);
    heroDesc.textContent = parts.join(' · ');
    heroPill.className = 'pill';
    heroPill.textContent = `READY · ${okCount}/14`;
  } else {
    heroIcon.className = 'sicon';
    heroIcon.textContent = '✓';
    heroTitle.textContent = '环境就绪';
    heroDesc.textContent = '14 项全部通过';
    heroPill.className = 'pill';
    heroPill.textContent = 'PASS · 14/14';
  }
}

async function refreshDesktopState(fresh = true) {
  if (isBusy) return;
  setBusyState(true, fresh ? '审计中…' : '读取中…');
  setBanner(fresh ? '正在审计…' : '读取缓存…');
  const t0 = performance.now();
  try {
    const res = await invoke('get_ui_state', { fresh });
    applyUiState(res, fresh ? performance.now() - t0 : 0);
    setBanner(res.checks?.length === 14 ? '审计完成' : '准备实时复检…');
  } catch (err) {
    setBanner(`审计失败: ${err}`, true);
  } finally {
    setBusyState(false);
  }
}

async function handleAction(label, fn, recheckAfter = false) {
  if (isBusy) return;
  setBusyState(true, `${label}…`);
  setBanner(`执行中: ${label}…`);
  try {
    const out = await fn();
    const text = String(out?.output || '').trim();
    const ok = out?.exit_code === 0 && !/^FAIL:/m.test(text);
    const lastLine = text.split(/\r?\n/).filter(Boolean).pop() || `${label}完成`;
    setBanner(`${ok ? '✓' : '✕'} ${lastLine.replace(/^(OK|WARN|FAIL|INFO):/, '')}`, !ok);
    setBusyState(false);
    if (recheckAfter) await refreshDesktopState(true);
    return out;
  } catch (err) {
    setBanner(`✕ ${label}失败: ${err}`, true);
    setBusyState(false);
    return null;
  }
}

function wireDesktopEvents() {
  document.querySelectorAll('.nav-tabs .tab').forEach((tab) => {
    tab.onclick = () => setView(tab.dataset.view);
  });

  ['tgAllowOptional', 'tgLogging'].forEach((id) => {
    const el = document.getElementById(id);
    if (el) {
      el.onclick = () => {
        const next = !el.classList.contains('on');
        setToggle(id, next);
      };
    }
  });

  document.getElementById('btnRecheck').onclick = () => refreshDesktopState(true);
  document.getElementById('btnFix').onclick = () =>
    handleAction('一键修复', () => invoke('run_fix'), true);
  document.getElementById('btnLaunchDesktop').onclick = () =>
    handleAction('启动 Claude', () => invoke('safe_launch', { target: 'desktop' }), false);
  document.getElementById('btnLaunchCli').onclick = () =>
    handleAction('启动 CLI', () => invoke('safe_launch', { target: 'cli' }), false);

  const shieldBtn = document.getElementById('btnShieldRestore');
  let shieldArmTimer = null;
  const disarmShield = () => {
    clearTimeout(shieldArmTimer);
    shieldBtn.classList.remove('armed');
    shieldBtn.textContent = '退出护盾';
  };
  shieldBtn.onclick = () => {
    if (!shieldBtn.classList.contains('armed')) {
      shieldBtn.classList.add('armed');
      shieldBtn.textContent = '确认退出?';
      setBanner('将关闭 Claude/CLI 并还原网络，5s 内再点确认', true);
      shieldArmTimer = setTimeout(() => {
        disarmShield();
        setBanner('已取消');
      }, 5000);
      return;
    }
    disarmShield();
    handleAction('退出护盾', () => invoke('shield_restore'), true);
  };

  document.getElementById('proxyClientSelect').onchange = (e) => {
    const mode = e.target.value;
    syncPortVisibility(mode);
    handleAction(`切换客户端`, () => invoke('set_client_mode', { mode }), true);
  };

  document.getElementById('btnSavePort').onclick = () => {
    const raw = document.getElementById('flclashPortInput').value.trim() || 'auto';
    handleAction('保存端口', () => invoke('set_flclash_port', { port: raw }), true);
  };

  document.querySelectorAll('.toolcard').forEach((card) => {
    card.onclick = async () => {
      if (isBusy) return;
      document.querySelectorAll('.toolcard').forEach((c) => c.classList.remove('active'));
      card.classList.add('active');
      const tool = card.dataset.tool;
      document.getElementById('consoleBar').textContent = `OUTPUT // ${tool}`;
      document.getElementById('consoleOut').textContent = `正在执行 ${tool}…`;
      const run = tool === 'export-diagnostics'
        ? () => invoke('export_diagnostics')
        : () => invoke('run_tool', { tool, arg: null });
      const res = await handleAction(tool, run, tool === 'set-region');
      if (res) {
        document.getElementById('consoleOut').textContent = res.output || '(无输出)';
      }
    };
  });

  document.getElementById('btnOpenSafe').onclick = async () => {
    const url = document.getElementById('safeUrlInput').value.trim();
    if (!/^https?:\/\/\S+$/i.test(url)) {
      document.getElementById('consoleBar').textContent = 'OUTPUT // open-safe';
      document.getElementById('consoleOut').textContent = '请输入完整的 https:// 或 http:// 链接';
      return;
    }
    document.getElementById('consoleBar').textContent = 'OUTPUT // open-safe';
    document.getElementById('consoleOut').textContent = '正在启动隔离浏览器…';
    const res = await handleAction('隔离打开', () => invoke('run_tool', { tool: 'open-safe', arg: url }), false);
    if (res) document.getElementById('consoleOut').textContent = res.output || '';
  };

  document.getElementById('btnResetDefaults').onclick = () => {
    setToggle('tgAllowOptional', false);
    setToggle('tgLogging', true);
    renderOptionalToggles(['时区', '地区']);
    document.getElementById('settingsMsg').textContent = '已恢复推荐默认，点「保存并重检」生效';
  };

  document.getElementById('btnSaveSettings').onclick = async () => {
    const disabled = [];
    document.querySelectorAll('[data-opt-key]').forEach((btn) => {
      if (!btn.classList.contains('on')) disabled.push(btn.dataset.optKey);
    });
    const payload = {
      disabled,
      allowOptionalFailures: document.getElementById('tgAllowOptional').classList.contains('on'),
      logging: document.getElementById('tgLogging').classList.contains('on')
    };
    const res = await handleAction(
      '保存策略',
      () => invoke('save_check_config', { configJson: JSON.stringify(payload) }),
      true
    );
    if (res?.exit_code === 0) {
      document.getElementById('settingsMsg').textContent = '';
      setView('overview');
    }
  };

  listen('tray-action', (event) => {
    const action = String(event?.payload || '');
    setDeviceMode('pc');
    setView('overview');
    if (action === 'recheck') {
      refreshDesktopState(true);
    } else if (action === 'launch-desktop') {
      handleAction('启动 Claude', () => invoke('safe_launch', { target: 'desktop' }), false);
    } else if (action === 'launch-cli') {
      handleAction('启动 CLI', () => invoke('safe_launch', { target: 'cli' }), false);
    } else if (action === 'shield-restore') {
      if (!shieldBtn.classList.contains('armed')) {
        shieldBtn.click();
      }
    }
  }).catch(() => {});
}

function wireMobileEvents() {
  const runBtn = document.getElementById('btnRunMobileCheck');
  if (!runBtn) return;
  runBtn.onclick = async () => {
    runBtn.disabled = true;
    runBtn.textContent = '检测中…';
    document.getElementById('mHeroTitle').textContent = '正在检测…';

    document.querySelectorAll('#mobileCheckList .mcard').forEach((card) => {
      card.querySelector('small').textContent = '检测中…';
      const st = card.querySelector('.st');
      st.className = 'st skip';
      st.textContent = '....';
    });

    const summary = await runMobileSelfCheck((key, res) => {
      const card = document.querySelector(`#mobileCheckList .mcard[data-mkey="${key}"]`);
      if (!card) return;
      card.querySelector('small').textContent = res.detail;
      const st = card.querySelector('.st');
      st.className = `st ${res.state}`;
      st.textContent = stateBadgeText(res.state);
    });

    if (summary.fails > 0) {
      document.getElementById('mHeroTitle').textContent = `发现 ${summary.fails} 项风险`;
      document.getElementById('mHeroDesc').textContent = '请检查代理全局/TUN 与分流规则';
    } else if (summary.warns > 0) {
      document.getElementById('mHeroTitle').textContent = `通过 · 含 ${summary.warns} 项提示`;
      document.getElementById('mHeroDesc').textContent = '未见国内出口或 DNS 泄漏';
    } else {
      document.getElementById('mHeroTitle').textContent = '5/5 防漏信号通过';
      document.getElementById('mHeroDesc').textContent = '出口、DNS、WebRTC 与 IPv6 无异常暴露';
    }
    runBtn.disabled = false;
    runBtn.textContent = '重新检测';
  };
}

function init() {
  let savedTheme = 'terminal';
  try { savedTheme = localStorage.getItem('envguard-theme') || 'terminal'; } catch {}
  setTheme(savedTheme);

  document.querySelectorAll('[data-theme-pick]').forEach((btn) => {
    btn.onclick = () => setTheme(btn.dataset.themePick);
  });

  document.getElementById('btnSwitchMobileView').onclick = () => setDeviceMode('mobile');
  document.getElementById('btnBackToPc').onclick = () => setDeviceMode('pc');

  wireMobileEvents();

  if (isTauri) {
    document.documentElement.setAttribute('data-runtime', 'tauri');
    setDeviceMode('pc');
    renderCheckSkeleton();
    renderOptionalToggles([]);
    wireDesktopEvents();
    refreshDesktopState(false).then(() => refreshDesktopState(true));
  } else {
    document.documentElement.setAttribute('data-runtime', 'web');
    setDeviceMode('mobile');
    document.getElementById('btnRunMobileCheck').click();
  }
}

init();
