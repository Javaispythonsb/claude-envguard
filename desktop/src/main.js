import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { runMobileSelfCheck } from './mobile-check.js';

const CHECK_SPECS = [
  { key: '代理内核', label: '代理内核', hard: true },
  { key: 'TUN网卡', label: 'TUN 网卡', hard: true },
  { key: '出口IP', label: '出口国家与 IP', hard: true },
  { key: 'IPv6', label: 'IPv6 旁路防护', hard: true },
  { key: '保镖', label: '保镖进程急停', hard: true },
  { key: '规则置顶', label: '置顶规则 / 隧道接管', hard: true },
  { key: '时区', label: '时区对齐', hard: false },
  { key: '地区', label: '区域格式 Locale', hard: false },
  { key: 'IP一致', label: 'IP 稳定性 (48h)', hard: false },
  { key: 'DNS', label: 'DNS 隧道与解析器归属', hard: false },
  { key: 'WebRTC', label: 'WebRTC STUN 探针', hard: false },
  { key: '客户端语言', label: '客户端语言', hard: false },
  { key: 'ASN属性', label: 'ASN 网络属性', hard: false },
  { key: '生态可达性', label: 'Claude 生态可达性', hard: false }
];

const OPTIONAL_ITEMS = [
  { key: '时区', title: '系统时区对齐（关掉则保留本机时钟，一键修复不改时间）' },
  { key: '地区', title: '区域格式 Locale 对齐（关掉则保留中文日期/数字格式）' },
  { key: 'WebRTC', title: 'WebRTC 国内外 STUN 交叉探针' },
  { key: 'DNS', title: 'DNS 隧道与实际解析器归属校验' },
  { key: 'IP一致', title: '出口 IP 48 小时跳变提醒' },
  { key: 'ASN属性', title: 'ASN 运营商类型核对' },
  { key: '客户端语言', title: 'Claude 客户端语言比对' },
  { key: '生态可达性', title: 'claude.ai / API 连通性探测' }
];

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
  document.querySelectorAll('.nav a').forEach((a) => {
    a.classList.toggle('active', a.dataset.view === viewName);
  });
}

function setBanner(text, isErr = false) {
  const el = document.getElementById('statusBanner');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('err', isErr);
}

function setBusyState(busy, label = '') {
  isBusy = busy;
  for (const id of ['btnRecheck', 'btnFix', 'btnShieldRestore', 'proxyClientSelect', 'btnSavePort', 'btnSaveSettings', 'btnOpenSafe']) {
    const el = document.getElementById(id);
    if (el) el.disabled = busy;
  }
  const canLaunch = !busy && Boolean(currentState?.environmentReady);
  const dBtn = document.getElementById('btnLaunchDesktop');
  const cBtn = document.getElementById('btnLaunchCli');
  if (dBtn) {
    dBtn.disabled = !canLaunch;
    if (busy && label) {
      dBtn.textContent = `⏳ ${label}`;
    } else if (canLaunch) {
      dBtn.textContent = '▶ 安全启动 Claude';
    } else {
      dBtn.textContent = '🔒 未过检已阻断启动';
    }
  }
  if (cBtn) cBtn.disabled = !canLaunch;
}

function stateBadgeText(st) {
  if (st === 'ok') return '[ OK ]';
  if (st === 'warn') return '[WARN]';
  if (st === 'skip') return '[SKIP]';
  if (st === 'fail') return '[FAIL]';
  return '[WAIT]';
}

function renderCheckSkeleton() {
  const grid = document.getElementById('checkListGrid');
  const meter = document.getElementById('heroMeter');
  if (!grid || !meter) return;
  grid.innerHTML = CHECK_SPECS.map((spec, idx) => `
    <div class="citem" data-ck="${spec.key}">
      <span class="idx">${String(idx + 1).padStart(2, '0')}</span>
      <span class="nm">${spec.label}${spec.hard ? ' <span class="hard">硬性</span>' : ''}</span>
      <span class="dt">等待检查…</span>
      <span class="st skip">[WAIT]</span>
    </div>
  `).join('');
  meter.innerHTML = CHECK_SPECS.map(() => '<i class="seg"></i>').join('');
}

function renderOptionalToggles(disabledList = []) {
  const box = document.getElementById('optionalChecksContainer');
  if (!box) return;
  const disabled = new Set(disabledList);
  box.innerHTML = OPTIONAL_ITEMS.map((item) => {
    const on = !disabled.has(item.key);
    return `
      <div class="optrow">
        <div><div class="ol">${item.title}</div></div>
        <button class="tg ${on ? 'on' : ''}" type="button" data-opt-key="${item.key}">${on ? 'ON' : 'OFF'}</button>
      </div>
    `;
  }).join('');
  box.querySelectorAll('[data-opt-key]').forEach((btn) => {
    btn.onclick = () => {
      btn.classList.toggle('on');
      btn.textContent = btn.classList.contains('on') ? 'ON' : 'OFF';
    };
  });
}

function setToggle(id, isOn) {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.toggle('on', Boolean(isOn));
  el.textContent = isOn ? 'ON' : 'OFF';
}

function applyUiState(data, elapsedMs = 0) {
  if (!data) return;
  document.getElementById('appVersionTag').textContent = data.version && data.version !== 'unknown' ? `v${data.version}` : 'v2.0';
  document.getElementById('chipCountry').textContent = data.country_target || '--';
  document.getElementById('chipSystemTz').textContent = data.system_tz || '--';
  document.getElementById('chipIanaTz').textContent = data.iana_tz_target || '--';
  document.getElementById('chipProxy').textContent = data.proxy_mode || '--';

  const sel = document.getElementById('proxyClientSelect');
  if (sel && data.proxy_mode) sel.value = data.proxy_mode;
  const portInput = document.getElementById('flclashPortInput');
  if (portInput) portInput.value = data.flclash_port && data.flclash_port !== 'auto' ? data.flclash_port : '';

  // Bodyguard sidebar card
  const bg = data.bodyguard || {};
  const gDot = document.getElementById('guardDot');
  const gBadge = document.getElementById('guardBadge');
  const gDesc = document.getElementById('guardDesc');
  gDot.className = 'dot ' + (bg.state === 'ok' ? '' : bg.state === 'warn' ? 'warn' : 'fail');
  gBadge.textContent = bg.state === 'ok' ? 'ACTIVE' : bg.state === 'warn' ? 'LEGACY' : 'OFFLINE';
  gDesc.textContent = bg.detail ? `状态：${bg.detail}。10s 巡检进程级急停（非防火墙级阻断）。` : '状态以环境审计为准。';

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
    const detail = found ? found.detail || (st === 'ok' ? '通过' : '') : '尚未执行';
    if (row) {
      row.querySelector('.dt').textContent = detail;
      row.querySelector('.dt').title = detail;
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

  auditMeta.textContent = elapsedMs > 0 ? `耗时 ${(elapsedMs / 1000).toFixed(1)}s · 单次 JSON 聚合` : '已载入最近快照';

  if (!complete) {
    heroIcon.className = 'sicon warn';
    heroIcon.textContent = '…';
    heroTitle.textContent = '尚未完成完整 14 项环境审计';
    heroDesc.textContent = '请点击右上角「重新检查」完成实时审计后再启动 Claude。';
    heroPill.className = 'pill warn';
    heroPill.textContent = 'INCOMPLETE';
  } else if (!ready) {
    heroIcon.className = 'sicon blocked';
    heroIcon.textContent = '✕';
    heroTitle.textContent = `启动门禁已阻断（${blockedNames.slice(0, 3).join('、')}）`;
    heroDesc.textContent = '6 项硬性防漏红线必须全部为 [ OK ] 且无失败项：可点「一键修复」处理，或在「护盾与设置」中关闭时区/区域格式等可选项。';
    heroPill.className = 'pill blocked';
    heroPill.textContent = `BLOCKED · ${blockedNames.length}`;
  } else if (warnCount > 0 || skipCount > 0) {
    heroIcon.className = 'sicon warn';
    heroIcon.textContent = '!';
    heroTitle.textContent = '6 项硬性防漏红线已全部通过 · 允许启动（可选项含提示/跳过）';
    heroDesc.textContent = `通过 ${okCount} 项，提示 ${warnCount} 项${skipCount ? `，跳过 ${skipCount} 项` : ''}；保镖以 10s 周期执行进程级急停。`;
    heroPill.className = 'pill warn';
    heroPill.textContent = `READY · ${okCount}/14 OK`;
  } else {
    heroIcon.className = 'sicon';
    heroIcon.textContent = '✓';
    heroTitle.textContent = '14 项检查全部通过 · 允许安全启动';
    heroDesc.textContent = '启动门禁已放行 Claude 桌面版与 CLI；运行期由保镖按 10s 周期监控内核与 TUN。';
    heroPill.className = 'pill';
    heroPill.textContent = `PASS · ${okCount}/14 OK`;
  }
}

async function refreshDesktopState(fresh = true) {
  if (isBusy) return;
  setBusyState(true, fresh ? '审计中…' : '读取中…');
  setBanner(fresh ? '● 正在执行完整 14 项环境安全审计…' : '● 正在读取本地缓存状态…');
  const t0 = performance.now();
  try {
    const res = await invoke('get_ui_state', { fresh });
    applyUiState(res, fresh ? performance.now() - t0 : 0);
    setBanner(res.checks?.length === 14 ? '● 环境审计已完成' : '● 已读取状态，正在准备实时复检…');
  } catch (err) {
    setBanner(`✕ 审计调用失败: ${err}`, true);
  } finally {
    setBusyState(false);
  }
}

async function handleAction(label, fn, recheckAfter = false) {
  if (isBusy) return;
  setBusyState(true, label);
  setBanner(`● 正在执行: ${label}…`);
  try {
    const out = await fn();
    const text = String(out?.output || '').trim();
    const ok = out?.exit_code === 0 && !/^FAIL:/m.test(text);
    const lastLine = text.split(/\r?\n/).filter(Boolean).pop() || `${label}完成`;
    setBanner(`${ok ? '✓' : '✕'} ${lastLine}`, !ok);
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
  document.querySelectorAll('.nav a').forEach((a) => {
    a.onclick = () => setView(a.dataset.view);
  });

  ['tgAllowOptional', 'tgLogging'].forEach((id) => {
    const el = document.getElementById(id);
    if (el) {
      el.onclick = () => {
        el.classList.toggle('on');
        el.textContent = el.classList.contains('on') ? 'ON' : 'OFF';
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
      shieldBtn.textContent = '再次点击确认';
      setBanner('! 退出护盾会关闭 Claude 桌面版和所有 Claude Code CLI 会话；5 秒内再次点击确认', true);
      shieldArmTimer = setTimeout(() => {
        disarmShield();
        setBanner('● 已取消退出护盾');
      }, 5000);
      return;
    }
    disarmShield();
    handleAction('退出护盾', () => invoke('shield_restore'), true);
  };

  document.getElementById('proxyClientSelect').onchange = (e) => {
    const mode = e.target.value;
    handleAction(`切换客户端 (${mode})`, () => invoke('set_client_mode', { mode }), true);
  };

  document.getElementById('btnSavePort').onclick = () => {
    const raw = document.getElementById('flclashPortInput').value.trim() || 'auto';
    handleAction('保存 FlClash 端口', () => invoke('set_flclash_port', { port: raw }), true);
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
      document.getElementById('consoleOut').textContent = 'FAIL: 请输入完整的 https:// 或 http:// 链接';
      return;
    }
    document.getElementById('consoleBar').textContent = 'OUTPUT // open-safe';
    document.getElementById('consoleOut').textContent = '正在核查隧道并启动一次性隔离浏览器…';
    const res = await handleAction('隔离打开链接', () => invoke('run_tool', { tool: 'open-safe', arg: url }), false);
    if (res) document.getElementById('consoleOut').textContent = res.output || '';
  };

  document.getElementById('btnResetDefaults').onclick = () => {
    setToggle('tgAllowOptional', false);
    setToggle('tgLogging', true);
    renderOptionalToggles([]);
    document.getElementById('settingsMsg').textContent = '已恢复默认选项，点击「保存策略并重新检查」生效。';
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
      '保存检查策略',
      () => invoke('save_check_config', { configJson: JSON.stringify(payload) }),
      true
    );
    if (res?.exit_code === 0) {
      document.getElementById('settingsMsg').textContent = '✓ 策略已保存，正在按新策略重新检查环境。';
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
    runBtn.textContent = '⏳ 正在实时探测 5 项防漏信号与硬件摘要…';
    document.getElementById('mHeroTitle').textContent = '正在执行浏览器侧实时自检…';

    document.querySelectorAll('#mobileCheckList .mcard').forEach((card) => {
      card.querySelector('small').textContent = '探测中…';
      const st = card.querySelector('.st');
      st.className = 'st skip';
      st.textContent = '[....]';
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
      document.getElementById('mHeroTitle').textContent = `发现 ${summary.fails} 项浏览器侧泄漏风险`;
      document.getElementById('mHeroDesc').textContent = '请先检查当前设备代理工具的全局/TUN 与分流规则，消除红项后再复测。';
    } else if (summary.warns > 0) {
      document.getElementById('mHeroTitle').textContent = `5 项防漏无红项 · 含 ${summary.warns} 项提示`;
      document.getElementById('mHeroDesc').textContent = '未发现浏览器侧国内出口或 DNS 泄漏，请留意下方黄色提示项（不代表电脑端进程级状态）。';
    } else {
      document.getElementById('mHeroTitle').textContent = '5/5 浏览器防漏信号通过（附硬件摘要）';
      document.getElementById('mHeroDesc').textContent = '当前浏览器出口、DNS、WebRTC、IPv6 与时区均无异常暴露；电脑端启动仍须通过桌面版 14 项审计。';
    }
    runBtn.disabled = false;
    runBtn.textContent = '↻ 重新检测浏览器信号';
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
    // Instant cached load (<40ms) followed by fresh background 14-point audit
    refreshDesktopState(false).then(() => refreshDesktopState(true));
  } else {
    document.documentElement.setAttribute('data-runtime', 'web');
    setDeviceMode('mobile');
    // Auto-run mobile check when opened in browser
    document.getElementById('btnRunMobileCheck').click();
  }
}

init();
