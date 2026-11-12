import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { runMobileSelfCheck } from './mobile-check.js';
import {
  POD_WIDTH,
  POD_COMPACT_HEIGHT,
  FIXABLE_OPTIONAL,
  CHECK_SPECS,
  OPTIONAL_ITEMS,
  CLIENT_NAMES,
  maskDisplayIp,
  conciseCheckValue,
  buildPlatterTiles,
  computePrimaryIslandAction
} from './hig-model.js';

const SVG_CHECK = '<svg class="ico md" viewBox="0 0 24 24" aria-hidden="true"><polyline points="20 6 9 17 4 12"/></svg>';
const SVG_BLOCK = '<svg class="ico md" viewBox="0 0 24 24" aria-hidden="true"><line x1="12" y1="8" x2="12" y2="13"/><circle cx="12" cy="17" r="0.9" fill="currentColor"/></svg>';
const SVG_WAIT = '<svg class="ico md" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8" stroke-dasharray="28 16"/></svg>';

const ORB_MARK_PATHS = {
  ok: 'm9.2 12.1 2 2.05 3.85-3.95',
  blocked: 'M12 8.2v4.6M12 16.2h.01',
  auditing: 'M9.5 12h.01M12 12h.01M14.5 12h.01'
};

const TAB_INDEX = {
  overview: 0,
  tools: 1,
  settings: 2
};

const isTauri = Boolean(window.__TAURI_INTERNALS__);
let isBusy = false;
let currentState = null;
let defaultSubLine = '检测网络隧道与常驻保镖';
let isExpanded = false;
let activeTab = 'overview';
let primaryActionMode = 'auditing';
let lastPodHeight = 0;
let isPinned = false;
let isTucked = false;
let isOrb = false;
let standbyMode = 'edge';
let focusHoldCount = 0;
let shieldArmed = false;

function setSubStatus(text, isErr = false) {
  const el = document.getElementById('heroSub');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('err', isErr);
}

function setStandbyTone(tone) {
  const bar = document.getElementById('edgeStatusBar');
  if (bar) bar.className = tone ? `edge-bar ${tone}` : 'edge-bar';

  const orb = document.getElementById('shieldOrb');
  if (orb) orb.className = tone ? `shield-orb ${tone}` : 'shield-orb';

  const mark = document.getElementById('orbMarkPath');
  if (mark) {
    const key = tone === 'blocked' ? 'blocked' : tone === 'auditing' ? 'auditing' : 'ok';
    mark.setAttribute('d', ORB_MARK_PATHS[key]);
  }

  const pod = document.getElementById('podCard');
  if (pod) {
    if (tone === 'blocked' || tone === 'auditing') {
      pod.setAttribute('data-alert', tone);
    } else {
      pod.removeAttribute('data-alert');
    }
  }
}

function syncDockHold() {
  if (!isTauri) return;
  const shouldHold = focusHoldCount > 0 || shieldArmed;
  invoke('set_dock_hold', { hold: shouldHold }).catch(() => {});
}

function syncPodHeight() {
  if (!isTauri || isTucked || isOrb) return;
  const stage = document.getElementById('pcApp');
  if (!stage) return;
  const h = Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage.offsetHeight));
  if (Math.abs(h - lastPodHeight) < 2) return;
  lastPodHeight = h;
  getCurrentWindow().setSize(new LogicalSize(POD_WIDTH, h)).catch(() => {});
}

function setActiveTab(tabName) {
  const next = Object.prototype.hasOwnProperty.call(TAB_INDEX, tabName) ? tabName : 'overview';
  activeTab = next;

  const track = document.getElementById('mainSegTrack');
  if (track) track.style.setProperty('--seg-idx', String(TAB_INDEX[next]));

  document.querySelectorAll('.seg-tab[data-tab]').forEach((btn) => {
    const on = btn.dataset.tab === next;
    btn.classList.toggle('active', on);
    btn.setAttribute('aria-selected', String(on));
  });

  const panelMap = {
    overview: 'panelOverview',
    tools: 'panelTools',
    settings: 'panelSettings'
  };
  for (const [k, id] of Object.entries(panelMap)) {
    const el = document.getElementById(id);
    if (el) el.hidden = k !== next;
  }

  requestAnimationFrame(syncPodHeight);
}

function setExpanded(open, tabName = null) {
  isExpanded = Boolean(open);
  document.documentElement.setAttribute('data-expanded', String(isExpanded));

  const sheet = document.getElementById('expandedSheet');
  if (sheet) sheet.hidden = !isExpanded;

  const toggleBtn = document.getElementById('btnToggleExpand');
  if (toggleBtn) {
    toggleBtn.setAttribute('aria-expanded', String(isExpanded));
    toggleBtn.title = isExpanded ? '收起控制面板 (Esc)' : '展开控制面板';
  }

  if (isExpanded && tabName) {
    setActiveTab(tabName);
  } else {
    requestAnimationFrame(syncPodHeight);
  }
}

function applyDockPayload(payload) {
  if (!payload) return;
  const root = document.documentElement;
  const edge = payload.edge || 'none';
  const prevOrb = isOrb;
  isTucked = Boolean(payload.tucked);
  isOrb = Boolean(payload.orb);
  standbyMode = payload.standby_mode || 'edge';
  const seam = Boolean(payload.seam);
  const idle = Boolean(payload.idle);
  const preview = payload.preview || 'none';
  isPinned = Boolean(payload.pinned);

  root.setAttribute('data-dock', edge);
  root.setAttribute('data-standby', standbyMode);
  root.setAttribute('data-orb', String(isOrb));
  root.setAttribute('data-tucked', String(isTucked));
  root.setAttribute('data-seam', String(seam));
  root.setAttribute('data-idle', String(idle));
  root.setAttribute('data-preview', preview);

  document.querySelectorAll('[data-standby-pick]').forEach((btn) => {
    btn.classList.toggle('on', btn.dataset.standbyPick === standbyMode);
  });

  const pinBtn = document.getElementById('btnPinPod');
  if (pinBtn) {
    pinBtn.classList.toggle('toggled', isPinned);
    pinBtn.title = isPinned
      ? '已固定展开（点击恢复自动待机）'
      : (standbyMode === 'orb' ? '自动收缩为陶瓷盾球（点击固定展开）' : '自动贴边隐藏中（点击固定展开）');
  }

  const hideBtn = document.getElementById('btnHideWin');
  if (hideBtn) {
    hideBtn.title = standbyMode === 'orb'
      ? '收起为陶瓷盾球 (Esc)'
      : (edge === 'none' ? '隐藏到系统托盘 (Esc)' : '收起至屏幕边缘 (Esc)');
  }

  if (prevOrb && !isOrb) {
    lastPodHeight = 0;
  }
  if (!isTucked && !isOrb) {
    requestAnimationFrame(syncPodHeight);
  }
}

function syncPortVisibility(mode) {
  const wrap = document.getElementById('flclashPortWrap');
  if (wrap) wrap.hidden = !(mode === 'flclash' || mode === 'auto');
  requestAnimationFrame(syncPodHeight);
}

function setBusyState(busy, label = '') {
  isBusy = busy;
  for (const id of ['btnRecheck', 'btnShieldRestore', 'proxyClientSelect', 'btnSavePort', 'btnSaveSettings', 'btnOpenSafe']) {
    const el = document.getElementById(id);
    if (el) el.disabled = busy;
  }

  const complete = (currentState?.checks || []).length === CHECK_SPECS.length;
  const ready = Boolean(currentState?.environmentReady);
  const needsFix = Boolean(currentState?.needsFix);

  const fixBtn = document.getElementById('btnFix');
  const fixTxt = document.getElementById('btnFixText');
  if (fixBtn) {
    const canFix = !busy && needsFix;
    fixBtn.disabled = !canFix;
    if (fixTxt) fixTxt.textContent = canFix ? '一键修复' : (busy ? '处理中…' : '无需修复');
    fixBtn.title = busy
      ? '操作进行中…'
      : canFix
        ? '一键自动修复异常项'
        : '当前环境正常，无需修复';
  }

  const primarySpec = computePrimaryIslandAction({
    busy,
    busyLabel: label,
    complete,
    ready,
    needsFix
  });
  primaryActionMode = primarySpec.mode;

  const pBtn = document.getElementById('btnPrimaryAction');
  if (pBtn) {
    pBtn.disabled = primarySpec.disabled;
    pBtn.textContent = primarySpec.text;
    pBtn.setAttribute('data-tone', primarySpec.tone);
    pBtn.title = primarySpec.title;
  }

  const cBtn = document.getElementById('btnLaunchCli');
  if (cBtn) cBtn.disabled = busy || !ready;
}

function renderCheckSkeleton() {
  const grid = document.getElementById('checkListGrid');
  if (!grid) return;
  grid.innerHTML = CHECK_SPECS.map((spec) => `
    <div class="check-row is-skip" data-ck="${spec.key}">
      <span class="cname">${spec.label}</span>
      <span class="check-right">
        <b class="cval">待检</b>
        <i class="sdot skip"></i>
      </span>
    </div>
  `).join('');

  grid.querySelectorAll('.check-row').forEach((row) => {
    row.addEventListener('mouseenter', () => {
      const tip = row.getAttribute('data-detail');
      if (tip && !isBusy) setSubStatus(tip, row.classList.contains('is-fail'));
    });
    row.addEventListener('mouseleave', () => {
      if (!isBusy && !shieldArmed) setSubStatus(defaultSubLine, false);
    });
  });
}

function setInlineSwitch(id, isOn) {
  const el = document.getElementById(id);
  if (!el) return;
  const on = Boolean(isOn);
  el.classList.toggle('on', on);
  el.setAttribute('aria-checked', String(on));
}

function renderOptionalSwitches(disabledList = []) {
  const box = document.getElementById('optionalChecksContainer');
  if (!box) return;
  const disabled = new Set(disabledList);
  box.innerHTML = OPTIONAL_ITEMS.map((item) => {
    const on = !disabled.has(item.key);
    return `
      <button type="button" class="switch-cell ${on ? 'on' : ''}" role="switch" aria-checked="${on}" data-opt-key="${item.key}">
        <span>${item.title}</span>
        <i class="ios-switch" aria-hidden="true"><b class="ios-thumb"></b></i>
      </button>
    `;
  }).join('');
  box.querySelectorAll('[data-opt-key]').forEach((cell) => {
    cell.onclick = () => {
      const next = !cell.classList.contains('on');
      cell.classList.toggle('on', next);
      cell.setAttribute('aria-checked', String(next));
    };
  });
}

function renderControlCenterTiles(data, checkMap) {
  const tiles = buildPlatterTiles(data, checkMap);
  for (const t of tiles) {
    const card = document.querySelector(`.cc-tile[data-cc="${t.id}"]`);
    if (!card) continue;
    const icon = card.querySelector('.cc-icon');
    const title = card.querySelector('.cc-title');
    const sub = card.querySelector('.cc-sub');
    if (icon) icon.setAttribute('data-tone', t.tone);
    if (title) title.textContent = t.title;
    if (sub) sub.textContent = t.sub;
  }
}

function applyUiState(data) {
  if (!data) return;
  const verEl = document.getElementById('appVersionTag');
  if (verEl) verEl.textContent = data.version && data.version !== 'unknown' ? `EnvGuard v${data.version}` : 'EnvGuard v2.0';

  const sel = document.getElementById('proxyClientSelect');
  if (sel && data.proxy_mode) {
    sel.value = data.proxy_mode;
    syncPortVisibility(data.proxy_mode);
  }
  const portInput = document.getElementById('flclashPortInput');
  if (portInput) portInput.value = data.flclash_port && data.flclash_port !== 'auto' ? data.flclash_port : '';

  const cfg = data.check_config || {};
  setInlineSwitch('tgAllowOptional', Boolean(cfg.allowOptionalFailures));
  setInlineSwitch('tgLogging', cfg.logging !== false);
  renderOptionalSwitches(cfg.disabled || []);

  const map = new Map();
  for (const item of data.checks || []) {
    map.set(item.name, item);
  }

  renderControlCenterTiles(data, map);

  const blockedNames = [];
  let downgradedFixable = false;
  CHECK_SPECS.forEach((spec) => {
    const row = document.querySelector(`.check-row[data-ck="${spec.key}"]`);
    const found = map.get(spec.key);
    const st = found ? found.state : 'skip';
    const rawDetail = maskDisplayIp(found?.detail || '');
    if (row) {
      row.className = `check-row is-${st}`;
      const valEl = row.querySelector('.cval');
      if (valEl) valEl.textContent = conciseCheckValue(spec, st, rawDetail, data.country_target);
      const dot = row.querySelector('.sdot');
      if (dot) dot.className = `sdot ${st === 'ok' ? '' : st}`.trim();
      const summary = `${spec.label}：${rawDetail || st.toUpperCase()}`;
      row.title = summary;
      row.setAttribute('data-detail', summary);
    }
    if (st === 'fail' || (spec.hard && st !== 'ok')) {
      blockedNames.push(spec.label);
    }
    if (FIXABLE_OPTIONAL.has(spec.key) && st === 'warn' && String(found?.detail || '').startsWith('非关键失败已忽略')) {
      downgradedFixable = true;
    }
  });

  const complete = (data.checks || []).length === CHECK_SPECS.length;
  const ready = typeof data.ready === 'boolean' ? (complete && data.ready) : (complete && blockedNames.length === 0);
  const needsFix = typeof data.needs_fix === 'boolean'
    ? (complete && data.needs_fix)
    : (complete && (!ready || downgradedFixable));

  currentState = { ...data, environmentReady: ready, needsFix };

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
    setStandbyTone('auditing');
    heroTitle.textContent = '等待审计';
    defaultSubLine = `${cc} · ${clientName} · 展开面板可重检`;
  } else if (!ready) {
    gauge.className = 'ring-gauge blocked';
    rIcon.innerHTML = SVG_BLOCK;
    setStandbyTone('blocked');
    heroTitle.textContent = `未通过：${blockedNames.slice(0, 2).join('、')}`;
    defaultSubLine = needsFix
      ? '点击右侧「修复」自动处理异常项'
      : '展开控制面板查看阻断原因或调整策略';
  } else {
    gauge.className = 'ring-gauge';
    rIcon.innerHTML = SVG_CHECK;
    setStandbyTone('');
    heroTitle.textContent = '环境安全';
    defaultSubLine = `${cc} · ${clientName} · ${guardState}`;
  }

  if (!shieldArmed) {
    setSubStatus(defaultSubLine, false);
  }
  setBusyState(isBusy);
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
    setStandbyTone('auditing');
    setSubStatus('正在并发检测隧道、出口与泄漏信号…', false);
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
        if (!isBusy && !shieldArmed) setSubStatus(defaultSubLine, false);
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

  document.querySelectorAll('#inlineMobileList .matrix-row').forEach((row) => {
    const val = row.querySelector('.mval');
    if (val) {
      val.textContent = '…';
      val.style.color = '';
    }
  });

  await runMobileSelfCheck((key, res) => {
    const row = document.querySelector(`#inlineMobileList .matrix-row[data-mkey="${key}"]`);
    if (!row) return;
    const val = row.querySelector('.mval');
    const short = String(res.detail || '').split(/[（(·]/)[0].trim() || res.state.toUpperCase();
    val.textContent = short;
    val.style.color = res.state === 'fail'
      ? 'var(--sys-red)'
      : res.state === 'warn'
        ? 'var(--sys-amber)'
        : 'var(--sys-green)';
    row.title = res.detail || '';
    requestAnimationFrame(syncPodHeight);
  });
}

function wireDesktopEvents() {
  const requestTuckOrHide = () => {
    if (isExpanded) setExpanded(false);
    if (isTauri) invoke('tuck_or_hide').catch(() => {});
  };

  document.getElementById('btnToggleExpand').onclick = () => {
    setExpanded(!isExpanded);
  };

  document.querySelectorAll('.seg-tab[data-tab]').forEach((btn) => {
    btn.onclick = () => setActiveTab(btn.dataset.tab);
  });

  document.getElementById('btnHideWin').onclick = requestTuckOrHide;
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      if (isExpanded) setExpanded(false);
      else requestTuckOrHide();
    }
  });

  document.getElementById('btnPinPod').onclick = () => {
    invoke('set_dock_pinned', { pinned: !isPinned })
      .then((st) => applyDockPayload(st))
      .catch(() => {});
  };

  document.querySelectorAll('[data-standby-pick]').forEach((btn) => {
    btn.onclick = () => {
      const mode = btn.dataset.standbyPick;
      invoke('set_standby_mode', { mode })
        .then((st) => {
          applyDockPayload(st);
          setSubStatus(mode === 'orb' ? '待机已切换为：陶瓷黑高光微球' : '待机已切换为：贴边微光条', false);
        })
        .catch(() => {});
    };
  });

  // Minimal Ceramic Orb: short click (<4px movement) expands island; press & move (>4px) drags smoothly
  const orbEl = document.getElementById('shieldOrb');
  if (orbEl) {
    let orbDown = false;
    let startX = 0;
    let startY = 0;
    let dragged = false;
    orbEl.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      orbDown = true;
      dragged = false;
      startX = e.screenX;
      startY = e.screenY;
    });
    window.addEventListener('mousemove', (e) => {
      if (!orbDown || dragged) return;
      if (Math.hypot(e.screenX - startX, e.screenY - startY) > 4) {
        dragged = true;
        orbDown = false;
        if (isTauri) getCurrentWindow().startDragging().catch(() => {});
      }
    });
    orbEl.addEventListener('mouseup', (e) => {
      if (e.button !== 0) return;
      const wasClick = orbDown && !dragged;
      orbDown = false;
      if (wasClick && isTauri) {
        invoke('expand_from_orb').catch(() => {});
      }
    });
  }

  for (const id of ['safeUrlInput', 'proxyClientSelect', 'flclashPortInput']) {
    const el = document.getElementById(id);
    if (!el) continue;
    el.addEventListener('focus', () => {
      focusHoldCount++;
      syncDockHold();
    });
    el.addEventListener('blur', () => {
      focusHoldCount = Math.max(0, focusHoldCount - 1);
      syncDockHold();
    });
  }

  ['tgAllowOptional', 'tgLogging'].forEach((id) => {
    const el = document.getElementById(id);
    if (el) el.onclick = () => setInlineSwitch(id, !el.classList.contains('on'));
  });

  document.getElementById('btnRecheck').onclick = () => refreshDesktopState(true);

  const runOneClickFix = () => {
    if (isBusy || !currentState?.needsFix) return;
    handleAction('修复', () => invoke('run_fix'), true);
  };

  document.getElementById('btnFix').onclick = runOneClickFix;

  // Smart Contextual Primary Action on the Compact Dynamic Island
  document.getElementById('btnPrimaryAction').onclick = () => {
    if (isBusy) return;
    if (primaryActionMode === 'launch') {
      handleAction('启动 Claude', () => invoke('safe_launch', { target: 'desktop' }), false);
    } else if (primaryActionMode === 'fix') {
      runOneClickFix();
    }
  };

  document.getElementById('btnLaunchCli').onclick = () =>
    handleAction('启动 CLI', () => invoke('safe_launch', { target: 'cli' }), false);

  const shieldBtn = document.getElementById('btnShieldRestore');
  let shieldArmTimer = null;
  const disarmShield = () => {
    clearTimeout(shieldArmTimer);
    shieldArmed = false;
    syncDockHold();
    shieldBtn.classList.remove('armed');
    shieldBtn.textContent = '退出护盾';
  };
  shieldBtn.onclick = () => {
    if (!shieldBtn.classList.contains('armed')) {
      shieldArmed = true;
      syncDockHold();
      shieldBtn.classList.add('armed');
      shieldBtn.textContent = '确认退出 (5s)';
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

  document.querySelectorAll('.tool-tile[data-tool]').forEach((tile) => {
    tile.onclick = async () => {
      if (isBusy) return;
      document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
      tile.classList.add('active');
      const tool = tile.dataset.tool;
      document.getElementById('inlineBrowserBox').hidden = true;
      const outBox = document.getElementById('toolOutputBox');
      const outPre = document.getElementById('consoleOut');
      outBox.hidden = false;
      outPre.textContent = `正在执行 ${tool}…`;
      requestAnimationFrame(syncPodHeight);

      const label = tile.querySelector('span')?.textContent || tool;
      const run = tool === 'export-diagnostics'
        ? () => invoke('export_diagnostics')
        : () => invoke('run_tool', { tool, arg: null });
      const res = await handleAction(label, run, tool === 'set-region');
      if (res) {
        outPre.textContent = maskDisplayIp(res.output || '(无输出)');
        requestAnimationFrame(syncPodHeight);
      }
    };
  });

  document.getElementById('btnInlineBrowserCheck').onclick = () => {
    document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
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
      outPre.textContent = maskDisplayIp(res.output || '');
      requestAnimationFrame(syncPodHeight);
    }
  };

  document.getElementById('btnResetDefaults').onclick = () => {
    setInlineSwitch('tgAllowOptional', false);
    setInlineSwitch('tgLogging', true);
    renderOptionalSwitches(['时区', '地区']);
    setSubStatus('已恢复推荐默认，点「保存」生效', false);
  };

  document.getElementById('btnSaveSettings').onclick = async () => {
    const disabled = [];
    document.querySelectorAll('[data-opt-key]').forEach((cell) => {
      if (!cell.classList.contains('on')) disabled.push(cell.dataset.optKey);
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
    if (res?.exit_code === 0 && isExpanded && activeTab === 'settings') {
      setActiveTab('overview');
    }
  };

  listen('collapse-drawers', () => {
    if (isExpanded) setExpanded(false);
  }).catch(() => {});

  listen('dock-state', (event) => {
    applyDockPayload(event?.payload);
  }).catch(() => {});

  listen('tray-action', (event) => {
    const action = String(event?.payload || '');
    if (action === 'recheck') {
      refreshDesktopState(true);
    } else if (action === 'launch-desktop') {
      handleAction('启动 Claude', () => invoke('safe_launch', { target: 'desktop' }), false);
    } else if (action === 'launch-cli') {
      handleAction('启动 CLI', () => invoke('safe_launch', { target: 'cli' }), false);
    } else if (action === 'shield-restore') {
      if (!isExpanded || activeTab !== 'overview') {
        setExpanded(true, 'overview');
      }
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
    runBtn.setAttribute('data-tone', 'muted');
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
      row.querySelector('.sdot').className = `sdot ${res.state === 'ok' ? '' : res.state}`.trim();
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
    runBtn.setAttribute('data-tone', 'green');
  };
}

function init() {
  wireMobileEvents();
  if (isTauri) {
    document.documentElement.setAttribute('data-runtime', 'tauri');
    renderCheckSkeleton();
    renderOptionalSwitches([]);
    wireDesktopEvents();
    invoke('get_dock_state')
      .then((st) => applyDockPayload(st))
      .catch(() => {});
    syncPodHeight();
    refreshDesktopState(false).then(() => refreshDesktopState(true));
  } else {
    document.documentElement.setAttribute('data-runtime', 'web');
    document.getElementById('btnRunMobileCheck').click();
  }
}

init();
