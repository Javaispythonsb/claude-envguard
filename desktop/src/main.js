import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { runMobileSelfCheck } from './mobile-check.js';

const POD_WIDTH = 588;

const CHECK_SPECS = [
  { key: '代理内核', label: '代理内核', hard: true },
  { key: 'TUN网卡', label: 'TUN 网卡', hard: true },
  { key: '出口IP', label: '出口地区', hard: true },
  { key: 'IPv6', label: 'IPv6 关闭', hard: true },
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

const OPTIONAL_ITEMS = [
  { key: '时区', title: '系统时区' },
  { key: '地区', title: '区域格式' },
  { key: 'DNS', title: 'DNS 归属' },
  { key: 'WebRTC', title: 'WebRTC' },
  { key: 'IP一致', title: 'IP 稳定' },
  { key: 'ASN属性', title: 'ASN 属性' },
  { key: '客户端语言', title: '客户端语言' },
  { key: '生态可达性', title: '服务连通' }
];

const CLIENT_NAMES = {
  'clash-verge': 'Clash Verge',
  flclash: 'FlClash',
  'sing-box': 'sing-box',
  clashx: 'ClashX',
  surge: 'Surge',
  wireguard: 'WireGuard',
  other: '其他 TUN',
  auto: '自动识别'
};

const SVG_CHECK = '<svg class="ico lg" viewBox="0 0 24 24" aria-hidden="true"><polyline points="20 6 9 17 4 12"/></svg>';
const SVG_BLOCK = '<svg class="ico lg" viewBox="0 0 24 24" aria-hidden="true"><line x1="12" y1="8" x2="12" y2="13"/><circle cx="12" cy="17" r="0.8" fill="currentColor"/></svg>';
const SVG_WAIT = '<svg class="ico lg" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8" stroke-dasharray="28 16"/></svg>';

const isTauri = Boolean(window.__TAURI_INTERNALS__);
let isBusy = false;
let currentState = null;
let defaultSubLine = '检测网络与保镖状态';
let activeDrawer = null;
let lastPodHeight = 0;

function maskDisplayIp(text) {
  return String(text || '').replace(/\b(?!198\.1[89]\.)(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}\b/g, '$1.$2.*.*');
}

function setSubStatus(text, isErr = false) {
  const el = document.getElementById('heroSub');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('err', isErr);
}

function syncPodHeight() {
  if (!isTauri) return;
  const stage = document.getElementById('pcApp');
  if (!stage) return;
  const h = Math.max(152, Math.ceil(stage.getBoundingClientRect().height));
  if (Math.abs(h - lastPodHeight) < 2) return;
  lastPodHeight = h;
  getCurrentWindow().setSize(new LogicalSize(POD_WIDTH, h)).catch(() => {});
}

function toggleDrawer(name) {
  const next = activeDrawer === name ? null : name;
  activeDrawer = next;
  const map = {
    details: 'drawerDetails',
    tools: 'drawerTools',
    settings: 'drawerSettings'
  };
  for (const [k, id] of Object.entries(map)) {
    const sec = document.getElementById(id);
    if (sec) sec.hidden = k !== next;
  }
  document.querySelectorAll('[data-drawer-btn]').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.drawerBtn === next);
  });
  requestAnimationFrame(syncPodHeight);
}

function syncPortVisibility(mode) {
  const wrap = document.getElementById('flclashPortWrap');
  if (wrap) wrap.hidden = !(mode === 'flclash' || mode === 'auto');
  requestAnimationFrame(syncPodHeight);
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
      dBtn.textContent = '已锁定';
    } else {
      dBtn.textContent = '审计中…';
    }
  }
  if (cBtn) cBtn.disabled = !canLaunch;
}

function renderCheckSkeleton() {
  const grid = document.getElementById('checkListGrid');
  if (!grid) return;
  grid.innerHTML = CHECK_SPECS.map((spec) => `
    <div class="pod-chip" data-ck="${spec.key}">
      <span class="cname">${spec.label}</span>
      <i class="sdot skip"></i>
    </div>
  `).join('');
}

function setFlagChip(id, isOn) {
  const el = document.getElementById(id);
  if (!el) return;
  const on = Boolean(isOn);
  el.classList.toggle('on', on);
  el.setAttribute('aria-checked', String(on));
}

function renderOptionalTiles(disabledList = []) {
  const box = document.getElementById('optionalChecksContainer');
  if (!box) return;
  const disabled = new Set(disabledList);
  box.innerHTML = OPTIONAL_ITEMS.map((item) => {
    const on = !disabled.has(item.key);
    return `
      <button type="button" class="opt-tile ${on ? 'on' : ''}" role="switch" aria-checked="${on}" data-opt-key="${item.key}">
        <span>${item.title}</span>
        <i class="sdot"></i>
      </button>
    `;
  }).join('');
  box.querySelectorAll('[data-opt-key]').forEach((tile) => {
    tile.onclick = () => {
      const next = !tile.classList.contains('on');
      tile.classList.toggle('on', next);
      tile.setAttribute('aria-checked', String(next));
    };
  });
}

function chipLabelFor(spec, st, rawDetail, countryTarget) {
  if (spec.key === '出口IP' && st === 'ok' && countryTarget) {
    return `出口 ${countryTarget}`;
  }
  if (spec.key === 'DNS' && st === 'ok' && rawDetail.includes('解析器位于 ')) {
    const cc = rawDetail.split('解析器位于 ').pop().trim();
    return `DNS · ${cc}`;
  }
  return spec.label;
}

function applyUiState(data) {
  if (!data) return;
  const verEl = document.getElementById('appVersionTag');
  if (verEl) verEl.textContent = data.version && data.version !== 'unknown' ? `v${data.version}` : 'v2.0';

  const sel = document.getElementById('proxyClientSelect');
  if (sel && data.proxy_mode) {
    sel.value = data.proxy_mode;
    syncPortVisibility(data.proxy_mode);
  }
  const portInput = document.getElementById('flclashPortInput');
  if (portInput) portInput.value = data.flclash_port && data.flclash_port !== 'auto' ? data.flclash_port : '';

  const cfg = data.check_config || {};
  setFlagChip('tgAllowOptional', Boolean(cfg.allowOptionalFailures));
  setFlagChip('tgLogging', cfg.logging !== false);
  renderOptionalTiles(cfg.disabled || []);

  const map = new Map();
  for (const item of data.checks || []) {
    map.set(item.name, item);
  }

  const blockedNames = [];
  CHECK_SPECS.forEach((spec) => {
    const chip = document.querySelector(`.pod-chip[data-ck="${spec.key}"]`);
    const found = map.get(spec.key);
    const st = found ? found.state : 'skip';
    const rawDetail = maskDisplayIp(found?.detail || '');
    if (chip) {
      chip.querySelector('.cname').textContent = chipLabelFor(spec, st, rawDetail, data.country_target);
      const dot = chip.querySelector('.sdot');
      dot.className = `sdot ${st === 'ok' ? '' : st}`;
      chip.title = `${spec.label}：${rawDetail || st.toUpperCase()}`;
    }
    if (st === 'fail' || (spec.hard && st !== 'ok')) {
      blockedNames.push(spec.label);
    }
  });

  const complete = (data.checks || []).length === CHECK_SPECS.length;
  const ready = typeof data.ready === 'boolean' ? (complete && data.ready) : (complete && blockedNames.length === 0);
  currentState = { ...data, environmentReady: ready };

  const gauge = document.getElementById('ringGauge');
  const rIcon = document.getElementById('ringIcon');
  const heroTitle = document.getElementById('heroTitle');

  const cc = data.country_target || '--';
  const clientName = CLIENT_NAMES[data.proxy_mode] || data.proxy_mode || '--';
  const bg = data.bodyguard || {};
  const guardState = bg.state === 'ok' ? '保镖在线' : bg.state === 'warn' ? '保镖待升级' : '保镖离线';

  if (!complete) {
    gauge.className = 'ring-gauge auditing';
    rIcon.innerHTML = SVG_WAIT;
    heroTitle.textContent = '等待审计';
    defaultSubLine = `${cc} · ${clientName} · 点「重检」开始`;
  } else if (!ready) {
    gauge.className = 'ring-gauge blocked';
    rIcon.innerHTML = SVG_BLOCK;
    heroTitle.textContent = `未通过：${blockedNames.slice(0, 2).join('、')}`;
    defaultSubLine = '点下方「修复」自动处理，或开「设置」调整';
  } else {
    gauge.className = 'ring-gauge';
    rIcon.innerHTML = SVG_CHECK;
    heroTitle.textContent = '环境安全';
    defaultSubLine = `${cc} · ${clientName} · ${guardState}`;
  }
  setSubStatus(defaultSubLine, false);
  requestAnimationFrame(syncPodHeight);
}

async function refreshDesktopState(fresh = true) {
  if (isBusy) return;
  setBusyState(true, fresh ? '审计中…' : '读取中…');
  if (fresh) {
    const gauge = document.getElementById('ringGauge');
    const rIcon = document.getElementById('ringIcon');
    if (gauge) gauge.className = 'ring-gauge auditing';
    if (rIcon) rIcon.innerHTML = SVG_WAIT;
    setSubStatus('正在检测网络与隧道…', false);
  }
  try {
    const res = await invoke('get_ui_state', { fresh });
    applyUiState(res);
  } catch (err) {
    setSubStatus(`审计失败: ${err}`, true);
  } finally {
    setBusyState(false);
  }
}

async function handleAction(label, fn, recheckAfter = false) {
  if (isBusy) return null;
  setBusyState(true, `${label}…`);
  setSubStatus(`正在${label}…`, false);
  try {
    const out = await fn();
    const text = String(out?.output || '').trim();
    const ok = out?.exit_code === 0 && !/^FAIL:/m.test(text);
    const lastLine = (text.split(/\r?\n/).filter(Boolean).pop() || `${label}完成`).replace(/^(OK|WARN|FAIL|INFO):/, '');
    setSubStatus(lastLine, !ok);
    setBusyState(false);
    if (recheckAfter) {
      await refreshDesktopState(true);
    } else if (ok) {
      setTimeout(() => {
        if (!isBusy) setSubStatus(defaultSubLine, false);
      }, 3200);
    }
    return out;
  } catch (err) {
    setSubStatus(`${label}失败: ${err}`, true);
    setBusyState(false);
    return null;
  }
}

async function runInlineBrowserSelfCheck() {
  const outBox = document.getElementById('toolOutputBox');
  const bBox = document.getElementById('inlineBrowserBox');
  if (outBox) outBox.hidden = true;
  if (bBox) bBox.hidden = false;
  requestAnimationFrame(syncPodHeight);

  document.querySelectorAll('#inlineMobileList .pod-chip').forEach((chip) => {
    chip.querySelector('.mval').textContent = '…';
  });

  await runMobileSelfCheck((key, res) => {
    const chip = document.querySelector(`#inlineMobileList .pod-chip[data-mkey="${key}"]`);
    if (!chip) return;
    const val = chip.querySelector('.mval');
    const short = String(res.detail || '').split(/[（(·]/)[0].trim() || res.state.toUpperCase();
    val.textContent = short;
    val.style.color = res.state === 'fail' ? 'var(--red)' : res.state === 'warn' ? 'var(--amber)' : 'var(--green)';
    chip.title = res.detail || '';
    requestAnimationFrame(syncPodHeight);
  });
}

function wireDesktopEvents() {
  const hideToTray = () => {
    if (isTauri) getCurrentWindow().hide().catch(() => {});
  };
  document.getElementById('btnHideWin').onclick = hideToTray;
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      if (activeDrawer) toggleDrawer(activeDrawer);
      else hideToTray();
    }
  });

  document.querySelectorAll('[data-drawer-btn]').forEach((btn) => {
    btn.onclick = () => toggleDrawer(btn.dataset.drawerBtn);
  });

  ['tgAllowOptional', 'tgLogging'].forEach((id) => {
    const el = document.getElementById(id);
    if (el) el.onclick = () => setFlagChip(id, !el.classList.contains('on'));
  });

  document.getElementById('btnRecheck').onclick = () => refreshDesktopState(true);
  document.getElementById('btnFix').onclick = () =>
    handleAction('修复', () => invoke('run_fix'), true);
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
      setSubStatus('将关闭 Claude/CLI 并还原网络，5s 内再点确认', true);
      shieldArmTimer = setTimeout(() => {
        disarmShield();
        setSubStatus(defaultSubLine, false);
      }, 5000);
      return;
    }
    disarmShield();
    handleAction('退出护盾', () => invoke('shield_restore'), true);
  };

  document.getElementById('proxyClientSelect').onchange = (e) => {
    const mode = e.target.value;
    syncPortVisibility(mode);
    handleAction('切换内核', () => invoke('set_client_mode', { mode }), true);
  };

  document.getElementById('btnSavePort').onclick = () => {
    const raw = document.getElementById('flclashPortInput').value.trim() || 'auto';
    handleAction('保存端口', () => invoke('set_flclash_port', { port: raw }), true);
  };

  document.querySelectorAll('.tool-pill[data-tool]').forEach((pill) => {
    pill.onclick = async () => {
      if (isBusy) return;
      document.querySelectorAll('.tool-pill').forEach((p) => p.classList.remove('active'));
      pill.classList.add('active');
      const tool = pill.dataset.tool;
      document.getElementById('inlineBrowserBox').hidden = true;
      const outBox = document.getElementById('toolOutputBox');
      const outPre = document.getElementById('consoleOut');
      outBox.hidden = false;
      outPre.textContent = `正在执行 ${tool}…`;
      requestAnimationFrame(syncPodHeight);

      const run = tool === 'export-diagnostics'
        ? () => invoke('export_diagnostics')
        : () => invoke('run_tool', { tool, arg: null });
      const res = await handleAction(pill.textContent, run, tool === 'set-region');
      if (res) {
        outPre.textContent = maskDisplayIp(res.output || '(无输出)');
        requestAnimationFrame(syncPodHeight);
      }
    };
  });

  document.getElementById('btnInlineBrowserCheck').onclick = () => {
    document.querySelectorAll('.tool-pill').forEach((p) => p.classList.remove('active'));
    document.getElementById('btnInlineBrowserCheck').classList.add('active');
    runInlineBrowserSelfCheck();
  };

  document.getElementById('btnOpenSafe').onclick = async () => {
    const url = document.getElementById('safeUrlInput').value.trim();
    const outBox = document.getElementById('toolOutputBox');
    const outPre = document.getElementById('consoleOut');
    document.getElementById('inlineBrowserBox').hidden = true;
    outBox.hidden = false;
    if (!/^https?:\/\/\S+$/i.test(url)) {
      outPre.textContent = '请输入完整的 https:// 或 http:// 链接';
      requestAnimationFrame(syncPodHeight);
      return;
    }
    outPre.textContent = '正在启动一次性隔离窗口…';
    requestAnimationFrame(syncPodHeight);
    const res = await handleAction('隔离打开', () => invoke('run_tool', { tool: 'open-safe', arg: url }), false);
    if (res) {
      outPre.textContent = res.output || '';
      requestAnimationFrame(syncPodHeight);
    }
  };

  document.getElementById('btnResetDefaults').onclick = () => {
    setFlagChip('tgAllowOptional', false);
    setFlagChip('tgLogging', true);
    renderOptionalTiles(['时区', '地区']);
    setSubStatus('已恢复推荐默认，点「保存」生效', false);
  };

  document.getElementById('btnSaveSettings').onclick = async () => {
    const disabled = [];
    document.querySelectorAll('[data-opt-key]').forEach((tile) => {
      if (!tile.classList.contains('on')) disabled.push(tile.dataset.optKey);
    });
    const payload = {
      disabled,
      allowOptionalFailures: document.getElementById('tgAllowOptional').classList.contains('on'),
      logging: document.getElementById('tgLogging').classList.contains('on')
    };
    const res = await handleAction(
      '保存设置',
      () => invoke('save_check_config', { configJson: JSON.stringify(payload) }),
      true
    );
    if (res?.exit_code === 0 && activeDrawer === 'settings') {
      toggleDrawer('settings');
    }
  };

  listen('tray-action', (event) => {
    const action = String(event?.payload || '');
    if (action === 'recheck') {
      refreshDesktopState(true);
    } else if (action === 'launch-desktop') {
      handleAction('启动 Claude', () => invoke('safe_launch', { target: 'desktop' }), false);
    } else if (action === 'launch-cli') {
      handleAction('启动 CLI', () => invoke('safe_launch', { target: 'cli' }), false);
    } else if (action === 'shield-restore') {
      if (!shieldBtn.classList.contains('armed')) shieldBtn.click();
    }
  }).catch(() => {});

  const stage = document.getElementById('pcApp');
  if (stage && typeof ResizeObserver !== 'undefined') {
    new ResizeObserver(() => syncPodHeight()).observe(stage);
  }
}

function wireMobileEvents() {
  const runBtn = document.getElementById('btnRunMobileCheck');
  if (!runBtn) return;
  runBtn.onclick = async () => {
    runBtn.disabled = true;
    runBtn.textContent = '检测中';
    const gauge = document.getElementById('mRingGauge');
    const icon = document.getElementById('mRingIcon');
    gauge.className = 'ring-gauge sm auditing';
    icon.innerHTML = SVG_WAIT;
    document.getElementById('mHeroTitle').textContent = '正在检测…';

    document.querySelectorAll('#mobileCheckList .web-row').forEach((row) => {
      row.querySelector('.wval').textContent = '检测中…';
      row.querySelector('.sdot').className = 'sdot skip';
    });

    const summary = await runMobileSelfCheck((key, res) => {
      const row = document.querySelector(`#mobileCheckList .web-row[data-mkey="${key}"]`);
      if (!row) return;
      row.querySelector('.wval').textContent = res.detail;
      row.querySelector('.wval').title = res.detail;
      row.querySelector('.sdot').className = `sdot ${res.state === 'ok' ? '' : res.state}`;
    });

    if (summary.fails > 0) {
      gauge.className = 'ring-gauge sm blocked';
      icon.innerHTML = SVG_BLOCK;
      document.getElementById('mHeroTitle').textContent = `${summary.fails} 项泄漏风险`;
      document.getElementById('mHeroDesc').textContent = '请检查代理 TUN 与分流规则';
    } else if (summary.warns > 0) {
      gauge.className = 'ring-gauge sm';
      icon.innerHTML = SVG_CHECK;
      document.getElementById('mHeroTitle').textContent = `通过 · ${summary.warns} 项提示`;
      document.getElementById('mHeroDesc').textContent = '未发现国内出口或 DNS 泄漏';
    } else {
      gauge.className = 'ring-gauge sm';
      icon.innerHTML = SVG_CHECK;
      document.getElementById('mHeroTitle').textContent = '5/5 防漏通过';
      document.getElementById('mHeroDesc').textContent = '出口、DNS、WebRTC 与 IPv6 无暴露';
    }
    runBtn.disabled = false;
    runBtn.textContent = '重测';
  };
}

function init() {
  wireMobileEvents();
  if (isTauri) {
    document.documentElement.setAttribute('data-runtime', 'tauri');
    renderCheckSkeleton();
    renderOptionalTiles([]);
    wireDesktopEvents();
    syncPodHeight();
    refreshDesktopState(false).then(() => refreshDesktopState(true));
  } else {
    document.documentElement.setAttribute('data-runtime', 'web');
    document.getElementById('btnRunMobileCheck').click();
  }
}

init();
