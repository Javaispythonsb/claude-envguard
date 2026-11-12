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
  describeCheckGuidance,
  buildAnomalyCallouts,
  summarizeMatrixCounts,
  normalizeSafeUrl,
  computePrimaryIslandAction,
  formatUpdateCheckReport,
  describeOrbModeMeta,
  computeImpactSquash
} from './hig-model.js';

const EASE_OUT = 'cubic-bezier(0.23, 1, 0.32, 1)';
const EASE_DRAWER = 'cubic-bezier(0.32, 0.72, 0, 1)';

const ORB_MARK_PATHS = {
  ok: 'm9.2 12.1 2 2.05 3.85-3.95',
  blocked: 'M12 8.2v4.6M12 16.2h.01',
  auditing: 'M9.5 12h.01M12 12h.01M14.5 12h.01'
};

const TAB_ORDER = ['overview', 'tools', 'settings'];
const TAB_INDEX = {
  overview: 0,
  tools: 1,
  settings: 2
};

const DOMAIN_NAMES = {
  tunnel: '隧道与规则',
  egress: '出口与网络',
  leak: '防漏与保镖',
  locale: '区域与语言'
};

const isTauri = Boolean(window.__TAURI_INTERNALS__);
const reducedMotionQuery = typeof window !== 'undefined' && window.matchMedia
  ? window.matchMedia('(prefers-reduced-motion: reduce)')
  : { matches: false };
const prefersReducedMotion = () => Boolean(reducedMotionQuery.matches);
const nextFrame = () => new Promise((resolve) => requestAnimationFrame(resolve));

let isBusy = false;
let currentState = null;
let latestCheckMap = new Map();
let defaultSubLine = '检测网络隧道与常驻保镖';
let subRevertTimer = null;

let isExpanded = false;
let activeTab = 'overview';
let primaryActionMode = 'auditing';
let lastPodHeight = 0;

let isPinned = false;
let isTucked = false;
let isOrb = false;
let isPlayMode = false;
let standbyTone = '';
let standbyMode = 'edge';
let hasFocusWithin = false;
let popupOpen = false;
let shieldArmed = false;
let orbSquashAnim = null;
let orbComboPopAnim = null;

// Overview progressive disclosure state
let matrixOpen = false;
let domainFilter = null;
let selectedCheckKey = null;

// Tools drawer state
let activeToolId = null;

// Settings debounced auto-save state
let configDirty = false;
let configSaveTimer = null;
let pendingConfigFlush = false;
let syncBadgeTimer = null;

function setSubStatus(text, isErr = false, autoRevertMs = 0) {
  clearTimeout(subRevertTimer);
  const el = document.getElementById('heroSub');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('err', isErr);
  el.setAttribute('role', isErr ? 'alert' : 'status');
  if (autoRevertMs > 0) {
    subRevertTimer = setTimeout(() => {
      if (!isBusy && !shieldArmed) {
        el.textContent = defaultSubLine;
        el.classList.remove('err');
        el.setAttribute('role', 'status');
      }
    }, autoRevertMs);
  }
}

function syncOrbA11yMeta() {
  const orb = document.getElementById('shieldOrb');
  if (!orb) return;
  const meta = describeOrbModeMeta({ tone: standbyTone, playMode: isPlayMode });
  orb.setAttribute('aria-label', meta.ariaLabel);
  orb.title = meta.title;
}

function setStandbyTone(tone) {
  standbyTone = tone || '';
  const bar = document.getElementById('edgeStatusBar');
  if (bar) bar.className = tone ? `edge-bar ${tone}` : 'edge-bar';

  const orb = document.getElementById('shieldOrb');
  if (orb) {
    orb.className = tone ? `shield-orb ${tone}` : 'shield-orb';
    syncOrbA11yMeta();
  }

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

function applyOrbFxPayload(fx) {
  if (!fx) return;
  const rotor = document.getElementById('orbRotor');
  const body = document.getElementById('orbBody');
  const badge = document.getElementById('orbComboBadge');
  const reduced = prefersReducedMotion();

  if (rotor) {
    const deg = reduced || !fx.angle ? 0 : Number(fx.angle.toFixed(1));
    rotor.style.transform = deg ? `rotate(${deg}deg)` : '';
  }

  if (badge) {
    const combo = Number(fx.combo) || 0;
    if (isPlayMode && combo >= 2) {
      badge.textContent = `×${combo}`;
      badge.hidden = false;
      if (!reduced && typeof badge.animate === 'function') {
        if (orbComboPopAnim) {
          try { orbComboPopAnim.cancel(); } catch {}
        }
        orbComboPopAnim = badge.animate(
          [
            { transform: 'scale3d(0.86, 0.86, 1)' },
            { transform: 'scale3d(1.18, 1.18, 1)' },
            { transform: 'scale3d(1, 1, 1)' }
          ],
          { duration: 210, easing: EASE_OUT }
        );
      }
    } else {
      badge.hidden = true;
    }
  }

  if (!reduced && body && fx.impact_axis && fx.impact_speed >= 95 && typeof body.animate === 'function') {
    const sq = computeImpactSquash(fx.impact_speed, fx.impact_axis);
    if (orbSquashAnim) {
      try { orbSquashAnim.cancel(); } catch {}
    }
    orbSquashAnim = body.animate(
      [
        { transform: 'scale3d(1, 1, 1)' },
        { transform: `scale3d(${sq.compress.sx}, ${sq.compress.sy}, 1)`, offset: 0.28 },
        { transform: `scale3d(${sq.rebound.sx}, ${sq.rebound.sy}, 1)`, offset: 0.66 },
        { transform: 'scale3d(1, 1, 1)' }
      ],
      { duration: 230, easing: EASE_OUT }
    );
  }
}

function syncDockHold() {
  if (!isTauri) return;
  const shouldHold = hasFocusWithin || popupOpen || shieldArmed || configDirty;
  invoke('set_dock_hold', { hold: shouldHold }).catch(() => {});
}

/**
 * Zero-clip, interruptible WAAPI & Atomic HWND Envelope Controller
 */
class IslandEnvelopeController {
  constructor() {
    this.seq = 0;
    this.hwndHeight = POD_COMPACT_HEIGHT;
    this.visualHeight = POD_COMPACT_HEIGHT;
    this.isAnimating = false;
    this.podAnim = null;
    this.sheetAnim = null;
    this.panelAnims = [];
    this.animRange = null;
  }

  panelIdOf(tab) {
    return tab === 'tools' ? 'panelTools' : tab === 'settings' ? 'panelSettings' : 'panelOverview';
  }

  podRadiiCss(open) {
    const r = open ? '28px' : '32px';
    const dock = document.documentElement.getAttribute('data-dock') || 'none';
    if (dock === 'top') return `0px 0px ${r} ${r}`;
    if (dock === 'left') return `0px ${r} ${r} 0px`;
    if (dock === 'right') return `${r} 0px 0px ${r}`;
    return r;
  }

  sampleCurrentVisualHeight() {
    if (this.podAnim && this.animRange && this.podAnim.playState === 'running') {
      const timing = this.podAnim.effect?.getComputedTiming?.();
      const p = typeof timing?.progress === 'number' ? timing.progress : 1;
      return Math.round(this.animRange.fromH + (this.animRange.toH - this.animRange.fromH) * p);
    }
    const stage = document.getElementById('pcApp');
    return stage ? Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage.offsetHeight)) : this.visualHeight;
  }

  cancelInFlight() {
    this.visualHeight = this.sampleCurrentVisualHeight();
    for (const a of [this.podAnim, this.sheetAnim, ...this.panelAnims]) {
      if (a) {
        try { a.cancel(); } catch {}
      }
    }
    this.podAnim = null;
    this.sheetAnim = null;
    this.panelAnims = [];
    this.animRange = null;

    const tabStage = document.getElementById('tabStage');
    if (tabStage) tabStage.style.minHeight = '';
    document.querySelectorAll('.tab-panel.is-exiting').forEach((el) => {
      el.classList.remove('is-exiting');
      el.hidden = el.id !== this.panelIdOf(activeTab);
    });
  }

  async commitHwndEnvelope(targetH, collapsed = false) {
    if (!isTauri || isTucked || isOrb) return;
    const h = Math.max(POD_COMPACT_HEIGHT, Math.ceil(targetH));
    if (Math.abs(h - this.hwndHeight) < 2 && !collapsed) return;
    this.hwndHeight = h;
    lastPodHeight = h;
    try {
      await invoke('set_pod_envelope', { logicalH: h, collapsed });
    } catch {
      await getCurrentWindow().setSize(new LogicalSize(POD_WIDTH, h)).catch(() => {});
    }
  }

  applyTabDomInstant(next) {
    activeTab = next;
    const glider = document.getElementById('segGlider');
    if (glider) glider.style.transform = `translate3d(${TAB_INDEX[next] * 100}%, 0, 0)`;

    document.querySelectorAll('.seg-tab[data-tab]').forEach((btn) => {
      const on = btn.dataset.tab === next;
      btn.classList.toggle('active', on);
      btn.setAttribute('aria-selected', String(on));
      btn.setAttribute('tabindex', on ? '0' : '-1');
    });

    for (const k of TAB_ORDER) {
      const el = document.getElementById(this.panelIdOf(k));
      if (el) {
        el.classList.remove('is-exiting');
        el.hidden = k !== next;
      }
    }
  }

  syncExpandedAria(open) {
    const toggleBtn = document.getElementById('btnToggleExpand');
    if (toggleBtn) {
      toggleBtn.setAttribute('aria-expanded', String(open));
      toggleBtn.title = open ? '收起控制面板 (Esc)' : '展开控制面板';
    }
    const sumBtn = document.getElementById('btnSummaryToggle');
    if (sumBtn) {
      sumBtn.setAttribute('aria-expanded', String(open));
    }
  }

  async setExpanded(open, { tabName = null, source = 'pointer' } = {}) {
    const seq = ++this.seq;
    const wantOpen = Boolean(open);
    const instant = source === 'keyboard' || prefersReducedMotion();

    if (!wantOpen && popupOpen) {
      closeProxyPopup();
    }

    this.cancelInFlight();
    const fromH = this.visualHeight;

    isExpanded = wantOpen;
    document.documentElement.setAttribute('data-expanded', String(wantOpen));
    this.syncExpandedAria(wantOpen);

    const pod = document.getElementById('podCard');
    const sheet = document.getElementById('expandedSheet');
    if (!pod || !sheet) return;

    if (wantOpen && tabName) {
      this.applyTabDomInstant(tabName);
    }

    if (instant || typeof pod.animate !== 'function') {
      this.isAnimating = false;
      pod.style.clipPath = '';
      sheet.hidden = !wantOpen;
      const stage = document.getElementById('pcApp');
      const targetH = wantOpen
        ? Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage?.offsetHeight || POD_COMPACT_HEIGHT))
        : POD_COMPACT_HEIGHT;
      this.visualHeight = targetH;
      await this.commitHwndEnvelope(targetH, !wantOpen);
      return;
    }

    this.isAnimating = true;

    if (wantOpen) {
      const radii = this.podRadiiCss(true);
      sheet.hidden = false;
      sheet.style.opacity = '0';
      const stage = document.getElementById('pcApp');
      const targetH = Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage.offsetHeight));
      const clipStart = Math.max(0, targetH - fromH);

      pod.style.clipPath = `inset(0px 0px ${clipStart}px 0px round ${radii})`;
      if (targetH > this.hwndHeight) {
        await this.commitHwndEnvelope(targetH, false);
        await nextFrame();
      }
      if (seq !== this.seq) return;

      sheet.style.opacity = '';
      this.animRange = { fromH, toH: targetH };
      this.podAnim = pod.animate(
        [
          { clipPath: `inset(0px 0px ${clipStart}px 0px round ${radii})` },
          { clipPath: `inset(0px 0px 0px 0px round ${radii})` }
        ],
        { duration: 210, easing: EASE_DRAWER, fill: 'both' }
      );
      this.sheetAnim = sheet.animate(
        [
          { opacity: 0, transform: 'translate3d(0, -6px, 0) scale(0.98)' },
          { opacity: 1, transform: 'translate3d(0, 0, 0) scale(1)' }
        ],
        { duration: 190, easing: EASE_OUT, fill: 'both' }
      );

      try { await Promise.all([this.podAnim.finished, this.sheetAnim.finished]); } catch { return; }
      if (seq !== this.seq) return;

      this.podAnim.cancel();
      this.sheetAnim.cancel();
      pod.style.clipPath = '';
      this.visualHeight = targetH;
      this.isAnimating = false;
      if (targetH < this.hwndHeight) {
        await this.commitHwndEnvelope(targetH, false);
      }
    } else {
      const radii = this.podRadiiCss(false);
      const stage = document.getElementById('pcApp');
      const domH = Math.max(fromH, Math.ceil(stage.offsetHeight));
      const startClip = Math.max(0, domH - fromH);
      const endClip = Math.max(0, domH - POD_COMPACT_HEIGHT);

      this.animRange = { fromH, toH: POD_COMPACT_HEIGHT };
      this.podAnim = pod.animate(
        [
          { clipPath: `inset(0px 0px ${startClip}px 0px round ${radii})` },
          { clipPath: `inset(0px 0px ${endClip}px 0px round ${radii})` }
        ],
        { duration: 140, easing: EASE_OUT, fill: 'both' }
      );
      this.sheetAnim = sheet.animate(
        [
          { opacity: 1, transform: 'translate3d(0, 0, 0) scale(1)' },
          { opacity: 0, transform: 'translate3d(0, -5px, 0) scale(0.98)' }
        ],
        { duration: 125, easing: EASE_OUT, fill: 'both' }
      );

      try { await Promise.all([this.podAnim.finished, this.sheetAnim.finished]); } catch { return; }
      if (seq !== this.seq) return;

      this.podAnim.cancel();
      this.sheetAnim.cancel();
      pod.style.clipPath = '';
      sheet.hidden = true;
      this.visualHeight = POD_COMPACT_HEIGHT;
      this.isAnimating = false;
      await this.commitHwndEnvelope(POD_COMPACT_HEIGHT, true);
    }
  }

  async switchTab(tabName, { source = 'pointer' } = {}) {
    const next = Object.prototype.hasOwnProperty.call(TAB_INDEX, tabName) ? tabName : 'overview';
    if (popupOpen) closeProxyPopup();
    if (!isBusy && !shieldArmed) setSubStatus(defaultSubLine, false);

    if (next === activeTab && !this.isAnimating) return;

    const instant = !isExpanded || source === 'keyboard' || prefersReducedMotion();
    const pod = document.getElementById('podCard');
    if (instant || !pod || typeof pod.animate !== 'function') {
      this.cancelInFlight();
      this.applyTabDomInstant(next);
      if (isExpanded) {
        const stage = document.getElementById('pcApp');
        const h = Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage?.offsetHeight || POD_COMPACT_HEIGHT));
        this.visualHeight = h;
        await this.commitHwndEnvelope(h, false);
      }
      return;
    }

    const seq = ++this.seq;
    const prev = activeTab;
    const dir = Math.sign(TAB_INDEX[next] - TAB_INDEX[prev]) || 1;
    this.cancelInFlight();

    const fromH = this.visualHeight;
    const stage = document.getElementById('pcApp');
    const tabStage = document.getElementById('tabStage');
    const prevEl = document.getElementById(this.panelIdOf(prev));
    const nextEl = document.getElementById(this.panelIdOf(next));

    activeTab = next;
    const glider = document.getElementById('segGlider');
    if (glider) glider.style.transform = `translate3d(${TAB_INDEX[next] * 100}%, 0, 0)`;
    document.querySelectorAll('.seg-tab[data-tab]').forEach((btn) => {
      const on = btn.dataset.tab === next;
      btn.classList.toggle('active', on);
      btn.setAttribute('aria-selected', String(on));
      btn.setAttribute('tabindex', on ? '0' : '-1');
    });

    if (!stage || !tabStage || !prevEl || !nextEl || prevEl === nextEl) return;
    this.isAnimating = true;

    const prevPanelH = prevEl.offsetHeight;
    prevEl.classList.add('is-exiting');
    nextEl.hidden = false;
    nextEl.style.opacity = '0';

    const toH = Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage.offsetHeight));
    const nextPanelH = nextEl.offsetHeight;
    const envelopeH = Math.max(fromH, toH);
    const radii = this.podRadiiCss(true);

    if (toH < fromH) {
      tabStage.style.minHeight = `${Math.max(prevPanelH, nextPanelH)}px`;
    }

    const clipStart = Math.max(0, envelopeH - fromH);
    const clipEnd = Math.max(0, envelopeH - toH);
    pod.style.clipPath = `inset(0px 0px ${clipStart}px 0px round ${radii})`;

    if (envelopeH > this.hwndHeight) {
      await this.commitHwndEnvelope(envelopeH, false);
      await nextFrame();
    }
    if (seq !== this.seq) return;

    nextEl.style.opacity = '';
    this.animRange = { fromH, toH };
    this.podAnim = pod.animate(
      [
        { clipPath: `inset(0px 0px ${clipStart}px 0px round ${radii})` },
        { clipPath: `inset(0px 0px ${clipEnd}px 0px round ${radii})` }
      ],
      { duration: 190, easing: EASE_DRAWER, fill: 'both' }
    );
    const outAnim = prevEl.animate(
      [
        { opacity: 1, transform: 'translate3d(0, 0, 0)' },
        { opacity: 0, transform: `translate3d(${-dir * 8}px, 0, 0)` }
      ],
      { duration: 115, easing: EASE_OUT, fill: 'both' }
    );
    const inAnim = nextEl.animate(
      [
        { opacity: 0, transform: `translate3d(${dir * 8}px, 0, 0)` },
        { opacity: 1, transform: 'translate3d(0, 0, 0)' }
      ],
      { duration: 175, easing: EASE_OUT, fill: 'both' }
    );
    this.panelAnims = [outAnim, inAnim];

    try { await Promise.all([this.podAnim.finished, inAnim.finished]); } catch { return; }
    if (seq !== this.seq) return;

    this.podAnim.cancel();
    outAnim.cancel();
    inAnim.cancel();
    prevEl.classList.remove('is-exiting');
    prevEl.hidden = true;
    tabStage.style.minHeight = '';
    pod.style.clipPath = '';
    this.visualHeight = toH;
    this.isAnimating = false;

    if (this.hwndHeight !== toH) {
      await this.commitHwndEnvelope(toH, false);
    }
  }

  syncIdleHeight() {
    if (this.isAnimating || !isTauri || isTucked || isOrb) return;
    const stage = document.getElementById('pcApp');
    if (!stage) return;
    const h = Math.max(POD_COMPACT_HEIGHT, Math.ceil(stage.offsetHeight));
    this.visualHeight = h;
    this.commitHwndEnvelope(h, !isExpanded);
  }
}

const envelopeCtrl = new IslandEnvelopeController();

function syncPodHeight() {
  envelopeCtrl.syncIdleHeight();
}

function applyDockPayload(payload) {
  if (!payload) return;
  const root = document.documentElement;
  const edge = payload.edge || 'none';
  const prevOrb = isOrb;
  isTucked = Boolean(payload.tucked);
  isOrb = Boolean(payload.orb);
  isPlayMode = Boolean(payload.play);
  standbyMode = payload.standby_mode || 'edge';
  const seam = Boolean(payload.seam);
  const idle = Boolean(payload.idle);
  const preview = payload.preview || 'none';
  isPinned = Boolean(payload.pinned);

  root.setAttribute('data-dock', edge);
  root.setAttribute('data-standby', standbyMode);
  root.setAttribute('data-orb', String(isOrb));
  root.setAttribute('data-play', String(isPlayMode));
  root.setAttribute('data-tucked', String(isTucked));
  root.setAttribute('data-seam', String(seam));
  root.setAttribute('data-idle', String(idle));
  root.setAttribute('data-preview', preview);
  syncOrbA11yMeta();
  if (!isPlayMode) {
    const badge = document.getElementById('orbComboBadge');
    if (badge) badge.hidden = true;
  }

  document.querySelectorAll('[data-standby-pick]').forEach((btn) => {
    const on = btn.dataset.standbyPick === standbyMode;
    btn.classList.toggle('on', on);
    btn.setAttribute('aria-checked', String(on));
  });

  const pinBtn = document.getElementById('btnPinPod');
  if (pinBtn) {
    pinBtn.classList.toggle('toggled', isPinned);
    pinBtn.setAttribute('aria-pressed', String(isPinned));
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
    envelopeCtrl.hwndHeight = 0;
    lastPodHeight = 0;
  }
  if (!isTucked && !isOrb) {
    requestAnimationFrame(syncPodHeight);
  }
}

/* ================= CUSTOM DARK POP-UP MENU FOR PROXY CLIENT ================= */
function syncProxyPopupDisplay(value) {
  const sel = document.getElementById('proxyClientSelect');
  const labelEl = document.getElementById('proxyPopupLabel');
  if (sel && value) sel.value = value;
  const activeVal = sel ? sel.value : value;
  if (labelEl) {
    labelEl.textContent = CLIENT_NAMES[activeVal] || activeVal || 'Clash Verge';
  }
  document.querySelectorAll('#proxyPopupMenu .apple-popup-item').forEach((item) => {
    item.setAttribute('aria-selected', String(item.dataset.val === activeVal));
  });
}

function closeProxyPopup() {
  const menu = document.getElementById('proxyPopupMenu');
  const trig = document.getElementById('proxyPopupTrigger');
  if (!menu || !trig) return;
  menu.hidden = true;
  trig.setAttribute('aria-expanded', 'false');
  popupOpen = false;
  syncDockHold();
  requestAnimationFrame(syncPodHeight);
}

function initProxyPopup() {
  const sel = document.getElementById('proxyClientSelect');
  const trig = document.getElementById('proxyPopupTrigger');
  const menu = document.getElementById('proxyPopupMenu');
  if (!sel || !trig || !menu) return;

  menu.innerHTML = Array.from(sel.options).map((opt) => `
    <button type="button" class="apple-popup-item" role="option" data-val="${opt.value}" aria-selected="${opt.selected}">
      ${opt.textContent}
    </button>
  `).join('');

  trig.onclick = (e) => {
    e.stopPropagation();
    if (isBusy) return;
    const nextOpen = menu.hidden;
    menu.hidden = !nextOpen;
    trig.setAttribute('aria-expanded', String(nextOpen));
    popupOpen = nextOpen;
    syncDockHold();
    requestAnimationFrame(syncPodHeight);
    if (nextOpen) {
      const selectedItem = menu.querySelector('[aria-selected="true"]') || menu.querySelector('.apple-popup-item');
      selectedItem?.focus();
    }
  };

  menu.querySelectorAll('.apple-popup-item').forEach((item) => {
    item.onclick = (e) => {
      e.stopPropagation();
      const val = item.dataset.val;
      const changed = sel.value !== val;
      syncProxyPopupDisplay(val);
      closeProxyPopup();
      trig.focus();
      if (changed) {
        sel.dispatchEvent(new Event('change'));
      }
    };
  });

  menu.addEventListener('keydown', (e) => {
    const items = Array.from(menu.querySelectorAll('.apple-popup-item'));
    const idx = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      items[(idx + 1) % items.length]?.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      items[(idx - 1 + items.length) % items.length]?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeProxyPopup();
      trig.focus();
    }
  });

  document.addEventListener('pointerdown', (e) => {
    if (popupOpen && !document.getElementById('proxyClientPopup')?.contains(e.target)) {
      closeProxyPopup();
    }
  });
}

function syncPortVisibility(mode) {
  const wrap = document.getElementById('flclashPortWrap');
  if (wrap) wrap.hidden = !(mode === 'flclash' || mode === 'auto');
  requestAnimationFrame(syncPodHeight);
}

/* ================= DEBOUNCED INSTANT AUTO-SAVE FOR SETTINGS SWITCHES ================= */
function showSettingsSyncBadge(text, state = 'ok', hideAfterMs = 2000) {
  clearTimeout(syncBadgeTimer);
  const badge = document.getElementById('settingsSyncBadge');
  if (!badge) return;
  badge.hidden = false;
  badge.textContent = text;
  badge.className = state === 'saving'
    ? 'sync-badge saving'
    : state === 'err'
      ? 'sync-badge err'
      : 'sync-badge';
  if (hideAfterMs > 0) {
    syncBadgeTimer = setTimeout(() => {
      badge.hidden = true;
    }, hideAfterMs);
  }
}

function collectCurrentCheckConfigPayload() {
  const disabled = [];
  document.querySelectorAll('#optionalChecksContainer [data-opt-key]').forEach((cell) => {
    if (!cell.classList.contains('on')) disabled.push(cell.dataset.optKey);
  });
  return {
    disabled,
    allowOptionalFailures: Boolean(document.getElementById('tgAllowOptional')?.classList.contains('on')),
    logging: Boolean(document.getElementById('tgLogging')?.classList.contains('on'))
  };
}

function scheduleCheckConfigAutoSave() {
  configDirty = true;
  syncDockHold();
  showSettingsSyncBadge('正在保存…', 'saving', 0);
  clearTimeout(configSaveTimer);
  configSaveTimer = setTimeout(() => {
    flushCheckConfigAutoSave();
  }, 380);
}

async function flushCheckConfigAutoSave() {
  if (!configDirty) return;
  if (isBusy) {
    pendingConfigFlush = true;
    return;
  }
  pendingConfigFlush = false;
  configDirty = false;
  syncDockHold();

  const payload = collectCurrentCheckConfigPayload();
  if (!isTauri) {
    showSettingsSyncBadge('✓ 已自动保存', 'ok', 1800);
    return;
  }

  isBusy = true;
  updateActionControlsForBusy(true, '');
  try {
    const out = await invoke('save_check_config', { configJson: JSON.stringify(payload) });
    if (out?.exit_code === 0) {
      showSettingsSyncBadge('✓ 已自动保存', 'ok', 2000);
      const freshState = await invoke('get_ui_state', { fresh: true });
      applyUiState(freshState);
    } else {
      showSettingsSyncBadge('保存失败', 'err', 3200);
    }
  } catch (err) {
    showSettingsSyncBadge('保存失败', 'err', 3200);
    setSubStatus(`设置保存失败: ${err}`, true, 3200);
  } finally {
    isBusy = false;
    updateActionControlsForBusy(false, '');
    if (configDirty || pendingConfigFlush) {
      flushCheckConfigAutoSave();
    }
  }
}

async function muteOptionalCheckImmediately(checkKey) {
  const optExists = OPTIONAL_ITEMS.some((o) => o.key === checkKey);
  if (!optExists) return;
  const cell = document.querySelector(`#optionalChecksContainer [data-opt-key="${checkKey}"]`);
  if (cell) {
    cell.classList.remove('on');
    cell.setAttribute('aria-checked', 'false');
  } else if (currentState?.check_config) {
    const prev = new Set(currentState.check_config.disabled || []);
    prev.add(checkKey);
    renderOptionalSwitches([...prev]);
  }
  setSubStatus(`已将「${checkKey}」设为免检，正在同步…`, false, 2400);
  scheduleCheckConfigAutoSave();
}

/* ================= BUSY & PRIMARY ACTION STATE ================= */
function updateActionControlsForBusy(busy, label = '') {
  for (const id of ['btnRecheck', 'btnShieldRestore', 'proxyPopupTrigger', 'btnSavePort', 'btnOpenSafe', 'btnApplyHotUpdate']) {
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
    if (fixTxt) fixTxt.textContent = canFix ? '一键修复' : (busy && label ? '处理中…' : '无需修复');
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

function setBusyState(busy, label = '') {
  isBusy = busy;
  updateActionControlsForBusy(busy, label);
  if (!busy && (pendingConfigFlush || configDirty)) {
    flushCheckConfigAutoSave();
  }
}

/* ================= OVERVIEW: TILES, ANOMALY CALLOUTS & INTERACTIVE MATRIX ================= */
function setSwitchCell(id, isOn) {
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
      scheduleCheckConfigAutoSave();
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
    const isFiltering = domainFilter === t.id;
    card.classList.toggle('is-filtering', isFiltering);
    card.setAttribute('aria-pressed', String(isFiltering));
  }
}

function triggerGuidanceAction(actionType, checkKey) {
  if (!actionType || isBusy) return;
  if (actionType === 'fix') {
    handleAction('修复', () => invoke('run_fix'), true);
  } else if (actionType === 'tool:set-region') {
    handleAction('重锁地区', () => invoke('run_tool', { tool: 'set-region', arg: null }), true);
  } else if (actionType === 'mute') {
    muteOptionalCheckImmediately(checkKey);
  } else if (actionType === 'goto-settings') {
    envelopeCtrl.switchTab('settings', { source: 'pointer' });
  }
}

function renderAnomalyCallouts(data, checkMap) {
  const listEl = document.getElementById('anomalyCalloutList');
  if (!listEl) return;
  const callouts = buildAnomalyCallouts(data, checkMap);
  if (callouts.length === 0) {
    listEl.hidden = true;
    listEl.innerHTML = '';
    return;
  }
  listEl.hidden = false;
  listEl.innerHTML = callouts.slice(0, 3).map((item) => `
    <div class="anomaly-card is-${item.state}">
      <div class="anomaly-body">
        <div class="anomaly-head">
          <i class="sdot ${item.state}"></i>
          <span>${item.label} · ${item.badge}</span>
        </div>
        <p class="anomaly-hint">${item.hint}</p>
      </div>
      ${item.actionType ? `<button type="button" class="mini-pill" data-anomaly-act="${item.actionType}" data-anomaly-key="${item.key}">${item.actionLabel}</button>` : ''}
    </div>
  `).join('');

  listEl.querySelectorAll('[data-anomaly-act]').forEach((btn) => {
    btn.onclick = () => triggerGuidanceAction(btn.dataset.anomalyAct, btn.dataset.anomalyKey);
  });
}

function renderCheckMatrix() {
  const grid = document.getElementById('checkListGrid');
  const foldBar = document.getElementById('btnToggleMatrix');
  const foldTitle = document.getElementById('matrixFoldTitle');
  const sumEl = document.getElementById('matrixSummaryText');
  if (!grid || !foldBar) return;

  const counts = summarizeMatrixCounts(latestCheckMap);
  if (sumEl) {
    sumEl.textContent = latestCheckMap.size > 0 ? counts.summaryText : '等待审计';
  }
  if (foldTitle) {
    foldTitle.textContent = domainFilter
      ? `${DOMAIN_NAMES[domainFilter]}明细（点磁贴取消筛选）`
      : '全部 14 项安全明细';
  }

  foldBar.setAttribute('aria-expanded', String(matrixOpen));
  grid.hidden = !matrixOpen;
  if (!matrixOpen) {
    grid.innerHTML = '';
    return;
  }

  const specs = domainFilter
    ? CHECK_SPECS.filter((s) => s.domain === domainFilter)
    : CHECK_SPECS;
  const cc = currentState?.country_target || '--';

  const htmlParts = [];
  for (let i = 0; i < specs.length; i += 2) {
    const rowSpecs = specs.slice(i, i + 2);
    rowSpecs.forEach((spec, colIdx) => {
      const found = latestCheckMap.get(spec.key);
      const st = found ? found.state : 'skip';
      const val = conciseCheckValue(spec, st, found?.detail || '', cc);
      const isSel = selectedCheckKey === spec.key;
      const evenCls = colIdx === 1 ? 'col-even' : '';
      htmlParts.push(`
        <button type="button" class="check-row is-${st} ${evenCls} ${isSel ? 'is-selected' : ''}" data-ck="${spec.key}" aria-expanded="${isSel}">
          <span class="cname">${spec.label}</span>
          <span class="check-right">
            <b class="cval">${val}</b>
            <i class="sdot ${st === 'ok' ? '' : st}"></i>
          </span>
        </button>
      `);
    });

    const selectedInPair = rowSpecs.find((s) => s.key === selectedCheckKey);
    if (selectedInPair) {
      const found = latestCheckMap.get(selectedInPair.key);
      const st = found ? found.state : 'skip';
      const guide = describeCheckGuidance(selectedInPair, st, found?.detail || '', cc);
      htmlParts.push(`
        <div class="check-inline-detail">
          <span class="check-inline-text"><b>${selectedInPair.label}：</b>${guide.hint}</span>
          ${guide.actionType ? `<button type="button" class="mini-pill" data-inline-act="${guide.actionType}" data-inline-key="${selectedInPair.key}">${guide.actionLabel}</button>` : ''}
        </div>
      `);
    }
  }

  grid.innerHTML = htmlParts.join('');

  grid.querySelectorAll('.check-row[data-ck]').forEach((row) => {
    row.onclick = () => {
      const key = row.dataset.ck;
      selectedCheckKey = selectedCheckKey === key ? null : key;
      renderCheckMatrix();
      requestAnimationFrame(syncPodHeight);
    };
  });

  grid.querySelectorAll('[data-inline-act]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      triggerGuidanceAction(btn.dataset.inlineAct, btn.dataset.inlineKey);
    };
  });
}

function applyUiState(data) {
  if (!data) return;
  const verEl = document.getElementById('appVersionTag');
  if (verEl) {
    const vStr = data.version && data.version !== 'unknown' ? `v${data.version}` : 'v2.0';
    const shaStr = data.git_short_sha && data.git_short_sha !== 'unknown' ? ` · ${data.git_short_sha}` : '';
    verEl.textContent = `EnvGuard ${vStr}${shaStr}`;
  }

  if (data.proxy_mode) {
    syncProxyPopupDisplay(data.proxy_mode);
    syncPortVisibility(data.proxy_mode);
  }
  const portInput = document.getElementById('flclashPortInput');
  if (portInput && document.activeElement !== portInput) {
    portInput.value = data.flclash_port && data.flclash_port !== 'auto' ? data.flclash_port : '';
  }

  // Only overwrite switch UI if user does not have unsaved debounced edits in flight
  if (!configDirty) {
    const cfg = data.check_config || {};
    setSwitchCell('tgAllowOptional', Boolean(cfg.allowOptionalFailures));
    setSwitchCell('tgLogging', cfg.logging !== false);
    renderOptionalSwitches(cfg.disabled || []);
  }

  const map = new Map();
  for (const item of data.checks || []) {
    map.set(item.name, item);
  }
  latestCheckMap = map;

  const blockedNames = [];
  let downgradedFixable = false;
  CHECK_SPECS.forEach((spec) => {
    const found = map.get(spec.key);
    const st = found ? found.state : 'skip';
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

  renderControlCenterTiles(data, map);
  renderAnomalyCallouts(data, map);
  renderCheckMatrix();

  const gauge = document.getElementById('ringGauge');
  const heroTitle = document.getElementById('heroTitle');

  const cc = data.country_target || '--';
  const clientName = CLIENT_NAMES[data.proxy_mode] || data.proxy_mode || '--';
  const bg = data.bodyguard || {};
  const guardState = bg.state === 'ok' ? '保镖在线' : bg.state === 'warn' ? '保镖待升级' : '保镖离线';

  if (!complete) {
    if (gauge) gauge.className = 'ring-gauge auditing';
    setStandbyTone('auditing');
    if (heroTitle) heroTitle.textContent = '等待审计';
    defaultSubLine = `${cc} · ${clientName} · 点击展开面板`;
  } else if (!ready) {
    if (gauge) gauge.className = 'ring-gauge blocked';
    setStandbyTone('blocked');
    if (heroTitle) heroTitle.textContent = `未通过：${blockedNames.slice(0, 2).join('、')}`;
    defaultSubLine = needsFix
      ? '点右侧「修复」自动处理，或点此查看原因'
      : '点此展开面板查看阻断原因与排障指引';
  } else {
    if (gauge) gauge.className = 'ring-gauge';
    setStandbyTone('');
    if (heroTitle) heroTitle.textContent = '环境安全';
    defaultSubLine = `${cc} · ${clientName} · ${guardState}`;
  }

  if (!shieldArmed) {
    setSubStatus(defaultSubLine, false);
  }
  updateActionControlsForBusy(isBusy);
  requestAnimationFrame(syncPodHeight);
}

/* ================= CORE IPC ACTIONS ================= */
async function refreshDesktopState(fresh = true) {
  if (isBusy) return;
  const recheckBtn = document.getElementById('btnRecheck');
  if (fresh && recheckBtn) {
    recheckBtn.classList.remove('is-spinning');
    void recheckBtn.offsetWidth;
    recheckBtn.classList.add('is-spinning');
  }
  setBusyState(true, fresh ? '审计中…' : '读取中…');
  if (fresh) {
    const gauge = document.getElementById('ringGauge');
    if (gauge) gauge.className = 'ring-gauge auditing';
    setStandbyTone('auditing');
    setSubStatus('正在并发检测隧道、出口与泄漏信号…', false);
  }
  try {
    const res = await invoke('get_ui_state', { fresh });
    applyUiState(res);
  } catch (err) {
    setSubStatus(`审计失败: ${err}`, true, 4000);
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
    const lastLine = maskDisplayIp(
      (text.split(/\r?\n/).filter(Boolean).pop() || `${label}完成`).replace(/^(OK|WARN|FAIL|INFO):/, '')
    );
    setSubStatus(lastLine, !ok, ok && !recheckAfter ? 3000 : 0);
    setBusyState(false);
    if (recheckAfter) {
      await refreshDesktopState(true);
    }
    return out;
  } catch (err) {
    setSubStatus(`${label}失败: ${err}`, true, 4000);
    setBusyState(false);
    return null;
  }
}

/* ================= TOOLS TAB: URL WELL, SELF-CHECK & COPYABLE DRAWER ================= */
function syncUrlInputButtons() {
  const input = document.getElementById('safeUrlInput');
  const pasteBtn = document.getElementById('btnPasteUrl');
  const clearBtn = document.getElementById('btnClearUrl');
  if (!input || !pasteBtn || !clearBtn) return;
  const hasVal = input.value.trim().length > 0;
  pasteBtn.hidden = hasVal;
  clearBtn.hidden = !hasVal;
}

function closeToolDrawers() {
  activeToolId = null;
  const outBox = document.getElementById('toolOutputBox');
  const bBox = document.getElementById('inlineBrowserBox');
  const hotBtn = document.getElementById('btnApplyHotUpdate');
  if (outBox) outBox.hidden = true;
  if (bBox) bBox.hidden = true;
  if (hotBtn) hotBtn.hidden = true;
  document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
  requestAnimationFrame(syncPodHeight);
}

async function runInlineBrowserSelfCheck() {
  activeToolId = 'inline-browser';
  const outBox = document.getElementById('toolOutputBox');
  const bBox = document.getElementById('inlineBrowserBox');
  if (outBox) outBox.hidden = true;
  if (bBox) bBox.hidden = false;
  requestAnimationFrame(syncPodHeight);

  document.querySelectorAll('#inlineMobileList .matrix-row').forEach((row) => {
    const val = row.querySelector('.mval');
    const dot = row.querySelector('.sdot');
    if (val) {
      val.textContent = '检测中…';
      val.style.color = '';
    }
    if (dot) dot.className = 'sdot skip';
  });

  await runMobileSelfCheck((key, res) => {
    const row = document.querySelector(`#inlineMobileList .matrix-row[data-mkey="${key}"]`);
    if (!row) return;
    const val = row.querySelector('.mval');
    const dot = row.querySelector('.sdot');
    const short = String(res.detail || '').split(/[（(·]/)[0].trim() || res.state.toUpperCase();
    if (val) {
      val.textContent = short;
      val.style.color = res.state === 'fail'
        ? 'var(--sys-red)'
        : res.state === 'warn'
          ? 'var(--sys-amber-text)'
          : 'var(--label-1)';
    }
    if (dot) dot.className = `sdot ${res.state === 'ok' ? '' : res.state}`.trim();
    row.title = res.detail || '';
    requestAnimationFrame(syncPodHeight);
  });
}

/* ================= 5s DUAL-LAYER SHIELD RESTORE RITUAL ================= */
function wireShieldDestructiveRitual(onConfirmRestore) {
  const btn = document.getElementById('btnShieldRestore');
  const fill = document.getElementById('shieldArmedFill');
  if (!btn || !fill) return { disarm: () => {} };

  const labels = btn.querySelectorAll('.dest-text');
  let drainAnim = null;
  let tickTimer = null;
  let armGuardUntil = 0;
  let secondsLeft = 5;

  const setLabelText = (txt) => {
    labels.forEach((el) => { el.textContent = txt; });
  };

  const currentClipRightPct = () => {
    if (drainAnim && drainAnim.playState === 'running') {
      const p = drainAnim.effect?.getComputedTiming?.()?.progress ?? 1;
      return Math.min(100, Math.max(0, p * 100));
    }
    return 100;
  };

  const disarm = (fastAnimate = true) => {
    clearInterval(tickTimer);
    tickTimer = null;
    if (!shieldArmed) return;

    const fromPct = currentClipRightPct();
    if (drainAnim) {
      try { drainAnim.cancel(); } catch {}
      drainAnim = null;
    }

    shieldArmed = false;
    syncDockHold();
    btn.classList.remove('armed');
    setLabelText('退出护盾');

    if (fastAnimate && !prefersReducedMotion() && typeof fill.animate === 'function' && fromPct < 99) {
      drainAnim = fill.animate(
        [
          { clipPath: `inset(0 ${fromPct.toFixed(1)}% 0 0)` },
          { clipPath: 'inset(0 100% 0 0)' }
        ],
        { duration: 180, easing: EASE_OUT, fill: 'forwards' }
      );
    } else {
      fill.style.clipPath = 'inset(0 100% 0 0)';
    }
  };

  const arm = () => {
    shieldArmed = true;
    secondsLeft = 5;
    armGuardUntil = performance.now() + 280;
    syncDockHold();
    btn.classList.add('armed');
    setLabelText(`确认退出 · ${secondsLeft}s`);
    setSubStatus('将关闭 Claude/CLI 并还原网络，再点确认 (Esc 取消)', true);

    if (drainAnim) {
      try { drainAnim.cancel(); } catch {}
    }
    if (prefersReducedMotion() || typeof fill.animate !== 'function') {
      fill.style.clipPath = 'inset(0 0% 0 0)';
    } else {
      drainAnim = fill.animate(
        [
          { clipPath: 'inset(0 0% 0 0)' },
          { clipPath: 'inset(0 100% 0 0)' }
        ],
        { duration: 5000, easing: 'linear', fill: 'forwards' }
      );
      drainAnim.onfinish = () => {
        if (shieldArmed) {
          disarm(false);
          if (!isBusy) setSubStatus(defaultSubLine, false);
        }
      };
    }

    tickTimer = setInterval(() => {
      secondsLeft -= 1;
      if (secondsLeft <= 0) {
        disarm(false);
        if (!isBusy) setSubStatus(defaultSubLine, false);
      } else {
        setLabelText(`确认退出 · ${secondsLeft}s`);
      }
    }, 1000);
  };

  btn.onclick = (e) => {
    e.stopPropagation();
    if (isBusy) return;
    if (!shieldArmed) {
      arm();
      return;
    }
    if (performance.now() < armGuardUntil) return;
    disarm(false);
    onConfirmRestore();
  };

  document.addEventListener('pointerdown', (e) => {
    if (shieldArmed && !btn.contains(e.target)) {
      disarm(true);
      if (!isBusy) setSubStatus(defaultSubLine, false);
    }
  });

  return { arm, disarm };
}

/* ================= 4px DEADZONE DRAG SURFACE & EVENT WIRING ================= */
function wireDeadzoneDragSurfaces() {
  const interactiveSelector = 'button:not(.pod-summary-trigger), input, select, a, pre, .apple-popup';
  let downSurface = null;
  let startX = 0;
  let startY = 0;
  let dragged = false;
  let suppressSummaryClick = false;

  document.querySelectorAll('.js-drag-surface').forEach((surface) => {
    surface.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      if (e.target.closest(interactiveSelector)) return;
      downSurface = surface;
      dragged = false;
      startX = e.screenX;
      startY = e.screenY;
    });
  });

  window.addEventListener('mousemove', (e) => {
    if (!downSurface || dragged) return;
    if (Math.hypot(e.screenX - startX, e.screenY - startY) > 4) {
      dragged = true;
      suppressSummaryClick = true;
      downSurface = null;
      if (isTauri) invoke('begin_window_drag').catch(() => {});
      setTimeout(() => { suppressSummaryClick = false; }, 320);
    }
  });

  window.addEventListener('mouseup', () => {
    downSurface = null;
  });

  const sumBtn = document.getElementById('btnSummaryToggle');
  if (sumBtn) {
    let ringClickTimer = null;
    sumBtn.onclick = (e) => {
      if (suppressSummaryClick) {
        e.preventDefault();
        suppressSummaryClick = false;
        return;
      }
      if (e.detail === 0) {
        envelopeCtrl.setExpanded(!isExpanded, { source: 'keyboard' });
        return;
      }

      // Double-clicking the left activity ring morphs the Island directly into the Stress-Relief Gravity Orb!
      const ringEl = document.getElementById('ringGauge');
      let hitRing = false;
      if (ringEl) {
        const rect = ringEl.getBoundingClientRect();
        hitRing = e.clientX >= rect.left - 4
          && e.clientX <= rect.right + 4
          && e.clientY >= rect.top - 4
          && e.clientY <= rect.bottom + 4;
      }

      if (hitRing && isTauri) {
        if (e.detail >= 2) {
          clearTimeout(ringClickTimer);
          ringClickTimer = null;
          invoke('toggle_orb_play_mode')
            .then((st) => applyDockPayload(st))
            .catch(() => {});
          return;
        }
        clearTimeout(ringClickTimer);
        ringClickTimer = setTimeout(() => {
          ringClickTimer = null;
          envelopeCtrl.setExpanded(!isExpanded, { source: 'pointer' });
        }, 210);
        return;
      }

      envelopeCtrl.setExpanded(!isExpanded, { source: 'pointer' });
    };
  }
}

function wireDesktopEvents() {
  initProxyPopup();
  wireDeadzoneDragSurfaces();

  const shieldRitual = wireShieldDestructiveRitual(() => {
    handleAction('退出护盾', () => invoke('shield_restore'), true);
  });

  const requestTuckOrHide = () => {
    if (shieldArmed) shieldRitual.disarm(false);
    if (isExpanded) {
      envelopeCtrl.setExpanded(false, { source: 'keyboard' });
    }
    if (isTauri) invoke('tuck_or_hide').catch(() => {});
  };

  document.getElementById('btnToggleExpand').onclick = (e) => {
    envelopeCtrl.setExpanded(!isExpanded, { source: e.detail === 0 ? 'keyboard' : 'pointer' });
  };

  // Segmented Control click & WAI-ARIA ArrowLeft/ArrowRight keyboard navigation
  document.querySelectorAll('.seg-tab[data-tab]').forEach((btn) => {
    btn.onclick = (e) => {
      envelopeCtrl.switchTab(btn.dataset.tab, { source: e.detail === 0 ? 'keyboard' : 'pointer' });
    };
  });
  const segTrack = document.getElementById('mainSegTrack');
  if (segTrack) {
    segTrack.addEventListener('keydown', (e) => {
      let nextIdx = -1;
      const curIdx = TAB_INDEX[activeTab] ?? 0;
      if (e.key === 'ArrowRight') nextIdx = (curIdx + 1) % TAB_ORDER.length;
      else if (e.key === 'ArrowLeft') nextIdx = (curIdx - 1 + TAB_ORDER.length) % TAB_ORDER.length;
      else if (e.key === 'Home') nextIdx = 0;
      else if (e.key === 'End') nextIdx = TAB_ORDER.length - 1;

      if (nextIdx >= 0) {
        e.preventDefault();
        const nextTab = TAB_ORDER[nextIdx];
        envelopeCtrl.switchTab(nextTab, { source: 'keyboard' });
        document.querySelector(`.seg-tab[data-tab="${nextTab}"]`)?.focus();
      }
    });
  }

  document.getElementById('btnHideWin').onclick = requestTuckOrHide;

  // Stacked Escape key consumption (Frequency-gated 0ms keyboard response)
  window.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (popupOpen) {
      closeProxyPopup();
      return;
    }
    if (shieldArmed) {
      shieldRitual.disarm(true);
      if (!isBusy) setSubStatus(defaultSubLine, false);
      return;
    }
    if (selectedCheckKey) {
      selectedCheckKey = null;
      renderCheckMatrix();
      requestAnimationFrame(syncPodHeight);
      return;
    }
    if (activeTab === 'tools' && activeToolId) {
      closeToolDrawers();
      return;
    }
    if (isExpanded) {
      if (document.activeElement instanceof HTMLElement) {
        document.activeElement.blur();
      }
      envelopeCtrl.setExpanded(false, { source: 'keyboard' });
      return;
    }
    requestTuckOrHide();
  });

  // Prevent 1.2s auto-tuck trap while keyboard focus is anywhere inside #podCard!
  const podCard = document.getElementById('podCard');
  if (podCard) {
    podCard.addEventListener('focusin', () => {
      const ae = document.activeElement;
      const isText = ae && (ae.tagName === 'INPUT' || ae.tagName === 'SELECT');
      const isKbdFocus = ae && typeof ae.matches === 'function' && ae.matches(':focus-visible');
      if (isText || isKbdFocus) {
        hasFocusWithin = true;
        syncDockHold();
      }
    });
    podCard.addEventListener('focusout', () => {
      requestAnimationFrame(() => {
        const stillInside = podCard.contains(document.activeElement)
          && (document.activeElement.tagName === 'INPUT' || document.activeElement.matches?.(':focus-visible'));
        hasFocusWithin = Boolean(stillInside);
        syncDockHold();
      });
    });
  }

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
          setSubStatus(mode === 'orb' ? '待机已切换为：陶瓷盾球' : '待机已切换为：贴边光条', false, 2400);
        })
        .catch(() => {});
    };
  });

  // Luminous Ceramic Orb: Hover-to-expand (260ms in Rust), Live 120Hz Drag (>4px), Double-click Gravity Play Mode, Single-click Bounce
  const orbEl = document.getElementById('shieldOrb');
  if (orbEl) {
    let orbDown = false;
    let startX = 0;
    let startY = 0;
    let orbDragged = false;
    let tapBounceTimer = null;

    orbEl.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      orbDown = true;
      orbDragged = false;
      startX = e.screenX;
      startY = e.screenY;
      // Immediately cancel any pending hover-to-expand so drag & double-click never get preempted!
      if (isTauri) invoke('suppress_orb_hover', { ms: 620 }).catch(() => {});
    });

    window.addEventListener('mousemove', (e) => {
      if (!orbDown || orbDragged) return;
      if (Math.hypot(e.screenX - startX, e.screenY - startY) > 4) {
        orbDragged = true;
        orbDown = false;
        clearTimeout(tapBounceTimer);
        tapBounceTimer = null;
        if (isTauri) invoke('begin_window_drag').catch(() => {});
      }
    });

    window.addEventListener('mouseup', () => {
      orbDown = false;
    });

    orbEl.addEventListener('click', (e) => {
      if (orbDragged) {
        e.preventDefault();
        orbDragged = false;
        return;
      }
      orbDown = false;
      if (!isTauri) return;

      // Keyboard Enter / Space accessibility: always expand immediately
      if (e.detail === 0) {
        invoke('expand_from_orb').catch(() => {});
        return;
      }

      // Double-click (<300ms): Toggle Stress-Relief Gravity Play Mode!
      if (e.detail >= 2) {
        clearTimeout(tapBounceTimer);
        tapBounceTimer = null;
        invoke('toggle_orb_play_mode')
          .then((st) => applyDockPayload(st))
          .catch(() => {});
        return;
      }

      // Single-click inside Gravity Play Mode: bouncy upward tap impulse (delayed slightly to yield to dblclick)
      if (isPlayMode) {
        clearTimeout(tapBounceTimer);
        tapBounceTimer = setTimeout(() => {
          tapBounceTimer = null;
          if (isPlayMode) invoke('orb_tap_impulse').catch(() => {});
        }, 190);
      } else {
        // In normal Orb standby, hover (260ms) already expands; pause briefly on single click so 2nd click can trigger Play Mode
        invoke('suppress_orb_hover', { ms: 420 }).catch(() => {});
      }
    });
  }

  // Overview: 2x2 Control Center Tiles filter & expand check matrix
  document.querySelectorAll('.cc-tile[data-cc]').forEach((tile) => {
    tile.onclick = () => {
      const dom = tile.dataset.cc;
      if (matrixOpen && domainFilter === dom) {
        domainFilter = null;
      } else {
        domainFilter = dom;
        matrixOpen = true;
      }
      selectedCheckKey = null;
      renderControlCenterTiles(currentState, latestCheckMap);
      renderCheckMatrix();
      requestAnimationFrame(syncPodHeight);
    };
  });

  document.getElementById('btnToggleMatrix').onclick = () => {
    matrixOpen = !matrixOpen;
    if (!matrixOpen) {
      domainFilter = null;
      selectedCheckKey = null;
      renderControlCenterTiles(currentState, latestCheckMap);
    }
    renderCheckMatrix();
    requestAnimationFrame(syncPodHeight);
  };

  // Settings: Policy switches auto-save immediately
  ['tgAllowOptional', 'tgLogging'].forEach((id) => {
    const el = document.getElementById(id);
    if (el) {
      el.onclick = () => {
        setSwitchCell(id, !el.classList.contains('on'));
        scheduleCheckConfigAutoSave();
      };
    }
  });

  document.getElementById('btnResetDefaults').onclick = () => {
    setSwitchCell('tgAllowOptional', false);
    setSwitchCell('tgLogging', true);
    renderOptionalSwitches(['时区', '地区']);
    scheduleCheckConfigAutoSave();
  };

  document.getElementById('btnRecheck').onclick = () => refreshDesktopState(true);

  const runOneClickFix = () => {
    if (isBusy || !currentState?.needsFix) return;
    handleAction('修复', () => invoke('run_fix'), true);
  };
  document.getElementById('btnFix').onclick = runOneClickFix;

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

  document.getElementById('proxyClientSelect').onchange = (e) => {
    const mode = e.target.value;
    syncPortVisibility(mode);
    handleAction('切换内核', () => invoke('set_client_mode', { mode }), true);
  };

  const portInput = document.getElementById('flclashPortInput');
  const saveFlclashPort = () => {
    const raw = portInput.value.trim() || 'auto';
    handleAction('保存端口', () => invoke('set_flclash_port', { port: raw }), true);
  };
  document.getElementById('btnSavePort').onclick = saveFlclashPort;
  if (portInput) {
    portInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        saveFlclashPort();
      }
    });
  }

  // Tools Tab: Toggleable 3x2 tiles + Copyable / Closable output drawer
  document.querySelectorAll('.tool-tile[data-tool]').forEach((tile) => {
    tile.onclick = async () => {
      if (isBusy) return;
      const tool = tile.dataset.tool;
      if (activeToolId === tool && !document.getElementById('toolOutputBox')?.hidden) {
        closeToolDrawers();
        return;
      }
      activeToolId = tool;
      document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
      tile.classList.add('active');

      document.getElementById('inlineBrowserBox').hidden = true;
      const hotBtn = document.getElementById('btnApplyHotUpdate');
      if (hotBtn) hotBtn.hidden = true;
      const outBox = document.getElementById('toolOutputBox');
      const outTitle = document.getElementById('toolOutputTitle');
      const outPre = document.getElementById('consoleOut');
      const label = tile.querySelector('span')?.textContent || tool;
      if (outTitle) outTitle.textContent = label;
      outBox.hidden = false;
      outPre.textContent = `正在执行 ${label}…`;
      requestAnimationFrame(syncPodHeight);

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

  // GitHub Check Update & One-Click Hot Update
  const checkUpdateTile = document.getElementById('btnCheckAppUpdate');
  const hotUpdateBtn = document.getElementById('btnApplyHotUpdate');
  if (checkUpdateTile) {
    checkUpdateTile.onclick = async () => {
      if (isBusy) return;
      if (activeToolId === 'check-app-update' && !document.getElementById('toolOutputBox')?.hidden) {
        closeToolDrawers();
        return;
      }
      activeToolId = 'check-app-update';
      document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
      checkUpdateTile.classList.add('active');

      document.getElementById('inlineBrowserBox').hidden = true;
      if (hotUpdateBtn) {
        hotUpdateBtn.hidden = true;
        hotUpdateBtn.textContent = '立即热更新';
      }
      const outBox = document.getElementById('toolOutputBox');
      const outTitle = document.getElementById('toolOutputTitle');
      const outPre = document.getElementById('consoleOut');
      if (outTitle) outTitle.textContent = '检查更新';
      outBox.hidden = false;
      outPre.textContent = '正在连接 GitHub 检查最新版本…';
      requestAnimationFrame(syncPodHeight);

      setBusyState(true, '检查更新…');
      setSubStatus('正在从 GitHub 获取最新构建信息…', false);
      try {
        const info = await invoke('check_app_update');
        const report = formatUpdateCheckReport(info);
        if (outTitle) outTitle.textContent = report.title;
        outPre.textContent = report.body;
        if (hotUpdateBtn) {
          hotUpdateBtn.hidden = !report.canHotUpdate;
          hotUpdateBtn.disabled = false;
        }
        setSubStatus(report.summary, info?.status === 'error', report.canHotUpdate ? 0 : 3500);
      } catch (err) {
        if (outTitle) outTitle.textContent = '检查更新 · 失败';
        outPre.textContent = `检查更新失败: ${err}`;
        setSubStatus(`检查更新失败: ${err}`, true, 3500);
      } finally {
        setBusyState(false);
        requestAnimationFrame(syncPodHeight);
      }
    };
  }

  if (hotUpdateBtn) {
    hotUpdateBtn.onclick = async () => {
      if (isBusy) return;
      const outTitle = document.getElementById('toolOutputTitle');
      const outPre = document.getElementById('consoleOut');
      hotUpdateBtn.textContent = '拉取校验中…';
      if (outTitle) outTitle.textContent = '正在热更新…';
      if (outPre) {
        outPre.textContent = '正在从 GitHub Release 下载最新固件包并核验 SHA-256 签名，请勿关闭窗口…';
      }
      hasFocusWithin = true;
      syncDockHold();
      requestAnimationFrame(syncPodHeight);

      const res = await handleAction('热更新', () => invoke('apply_app_update'), false);
      if (res?.exit_code === 0) {
        hotUpdateBtn.hidden = true;
        if (outTitle) outTitle.textContent = '热更新完成 · 正在重启';
        if (outPre) {
          outPre.textContent = `${maskDisplayIp(res.output || '更新校验完成')}\n窗口将在 1 秒内自动重启至最新版本…`;
        }
      } else {
        hasFocusWithin = false;
        syncDockHold();
        hotUpdateBtn.textContent = '重试热更新';
        if (outTitle) outTitle.textContent = '热更新未完成';
        if (outPre && res) {
          outPre.textContent = maskDisplayIp(res.output || '热更新失败');
        }
      }
      requestAnimationFrame(syncPodHeight);
    };
  }

  document.getElementById('btnInlineBrowserCheck').onclick = () => {
    if (activeToolId === 'inline-browser' && !document.getElementById('inlineBrowserBox')?.hidden) {
      closeToolDrawers();
      return;
    }
    document.querySelectorAll('.tool-tile').forEach((p) => p.classList.remove('active'));
    document.getElementById('btnInlineBrowserCheck').classList.add('active');
    runInlineBrowserSelfCheck();
  };

  document.getElementById('btnCloseToolOut').onclick = closeToolDrawers;
  document.getElementById('btnCloseBrowserBox').onclick = closeToolDrawers;

  const copyBtn = document.getElementById('btnCopyToolOut');
  if (copyBtn) {
    copyBtn.onclick = async () => {
      const txt = maskDisplayIp(document.getElementById('consoleOut')?.textContent || '');
      try {
        await navigator.clipboard.writeText(txt);
        copyBtn.textContent = '✓ 已复制';
        setTimeout(() => { copyBtn.textContent = '复制结果'; }, 1800);
      } catch {
        setSubStatus('剪贴板写入受限，请直接拖选复制', true, 2500);
      }
    };
  }

  // Safe Isolated URL Form: Enter submit, Paste, Clear, and Auto-Wipe after launch
  const urlInput = document.getElementById('safeUrlInput');
  const urlForm = document.getElementById('safeUrlForm');
  if (urlInput) {
    urlInput.addEventListener('input', syncUrlInputButtons);
  }
  document.getElementById('btnPasteUrl').onclick = async () => {
    try {
      const clip = (await navigator.clipboard.readText()).trim();
      if (clip && urlInput) {
        urlInput.value = clip;
        syncUrlInputButtons();
        urlInput.focus();
      }
    } catch {
      urlInput?.focus();
    }
  };
  document.getElementById('btnClearUrl').onclick = () => {
    if (!urlInput) return;
    urlInput.value = '';
    syncUrlInputButtons();
    urlInput.focus();
  };
  if (urlForm) {
    urlForm.onsubmit = async (e) => {
      e.preventDefault();
      if (isBusy) return;
      const parsed = normalizeSafeUrl(urlInput?.value || '');
      if (!parsed.ok) {
        setSubStatus(parsed.reason, true, 3000);
        urlInput?.focus();
        return;
      }
      const res = await handleAction('隔离打开', () => invoke('run_tool', { tool: 'open-safe', arg: parsed.url }), false);
      if (res?.exit_code === 0 && urlInput) {
        // Privacy wipe: never leave magic tokens / sensitive URLs lingering in the input!
        urlInput.value = '';
        syncUrlInputButtons();
        const origPlaceholder = urlInput.placeholder;
        urlInput.classList.add('ok-flash');
        urlInput.placeholder = '✓ 已在一次性无痕隔离窗口启动';
        setTimeout(() => {
          urlInput.classList.remove('ok-flash');
          urlInput.placeholder = origPlaceholder;
        }, 2600);
      } else if (res) {
        const outBox = document.getElementById('toolOutputBox');
        const outPre = document.getElementById('consoleOut');
        const outTitle = document.getElementById('toolOutputTitle');
        if (outTitle) outTitle.textContent = '隔离打开';
        if (outBox) outBox.hidden = false;
        if (outPre) outPre.textContent = maskDisplayIp(res.output || '');
        requestAnimationFrame(syncPodHeight);
      }
    };
  }

  listen('collapse-drawers', (ev) => {
    if (shieldArmed) shieldRitual.disarm(false);
    if (popupOpen) closeProxyPopup();
    if (isExpanded) {
      const immediate = ev?.payload?.immediate !== false;
      envelopeCtrl.setExpanded(false, { source: immediate ? 'keyboard' : 'pointer' });
    }
  }).catch(() => {});

  listen('dock-state', (event) => {
    applyDockPayload(event?.payload);
  }).catch(() => {});

  listen('orb-fx', (event) => {
    applyOrbFxPayload(event?.payload);
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
      envelopeCtrl.setExpanded(true, { tabName: 'overview', source: 'keyboard' }).then(() => {
        if (!shieldArmed) shieldRitual.arm();
      });
    }
  }).catch(() => {});

  const stage = document.getElementById('pcApp');
  if (stage && typeof ResizeObserver !== 'undefined') {
    new ResizeObserver(() => {
      if (!envelopeCtrl.isAnimating) {
        syncPodHeight();
      }
    }).observe(stage);
  }
}

/* ================= STANDALONE WEB / MOBILE PWA ================= */
function wireMobileEvents() {
  const runBtn = document.getElementById('btnRunMobileCheck');
  if (!runBtn) return;
  runBtn.onclick = async () => {
    runBtn.disabled = true;
    runBtn.textContent = '检测中';
    runBtn.setAttribute('data-tone', 'muted');
    const gauge = document.getElementById('mRingGauge');
    if (gauge) gauge.className = 'ring-gauge sm auditing';
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
      if (gauge) gauge.className = 'ring-gauge sm blocked';
      document.getElementById('mHeroTitle').textContent = `${summary.fails} 项泄漏风险`;
      document.getElementById('mHeroDesc').textContent = '请检查代理 TUN 与分流规则';
    } else if (summary.warns > 0) {
      if (gauge) gauge.className = 'ring-gauge sm';
      document.getElementById('mHeroTitle').textContent = `通过 · ${summary.warns} 项提示`;
      document.getElementById('mHeroDesc').textContent = '未发现国内出口或 DNS 泄漏';
    } else {
      if (gauge) gauge.className = 'ring-gauge sm';
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
    renderOptionalSwitches([]);
    renderCheckMatrix();
    syncUrlInputButtons();
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
