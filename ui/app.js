/* QuotaX 前端：监听 usage-update 渲染 + 倒计时 tick + 悬停展开 + 置顶/刷新 */

const tauri = window.__TAURI__;
const invoke = tauri.core.invoke;
const listen = tauri.event.listen;

const widget = document.getElementById('widget');
const bar = document.getElementById('bar');
const rowsEl = document.getElementById('rows');
const errorEl = document.getElementById('error-line');
const extraRow = document.getElementById('extra-row');
const extraVal = document.getElementById('extra-value');
const refreshTimeEl = document.getElementById('refresh-time');
const btnRefresh = document.getElementById('btn-refresh');
const chkTop = document.getElementById('chk-top');

// 登录视图（needs_login 时显示）
const loginView = document.getElementById('login-view');
const loginIdle = document.getElementById('login-idle');
const loginPanel = document.getElementById('login-panel');
const loginStatusEl = document.getElementById('login-status');
const loginStatusText = document.getElementById('login-status-text');
const loginCountdownEl = document.getElementById('login-countdown');
const btnLogin = document.getElementById('btn-login');
const btnOpenAuth = document.getElementById('btn-open-auth');
const btnCancelLogin = document.getElementById('btn-cancel-login');

// 当前各行的重置时间（ISO 字符串）→ 对应 DOM 引用，每秒 tick 更新
let countdownTargets = [];

// 登录会话前端状态
let loginActive = false;   // 设备码会话进行中（抑制失焦收起，用户需对照用户码）
let loginExpiresAt = 0;    // 设备码有效期截止（ms 时间戳）

function pctLevel(pct) {
  if (pct >= 95) return 'danger';
  if (pct >= 80) return 'warn';
  return '';
}

function setBarRow(idx, row) {
  const fill = document.getElementById(`bar-fill-${idx}`);
  const pct = document.getElementById(`bar-pct-${idx}`);
  if (!row) {
    fill.className = 'fill error';
    fill.style.width = '0%';
    pct.textContent = '--';
    pct.className = 'pct';
    return;
  }
  const p = row.limit > 0 ? Math.round((row.used / row.limit) * 100) : 0;
  const lv = pctLevel(p);
  fill.className = `fill ${lv}`.trim();
  fill.style.width = `${Math.min(p, 100)}%`;
  pct.textContent = `${p}%`;
  pct.className = `pct ${lv}`.trim();
}

function fmtCountdown(iso) {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '';
  const diff = t - Date.now();
  if (diff <= 0) return '已重置';
  const mins = Math.floor(diff / 60000);
  const h = Math.floor(mins / 60), m = mins % 60, d = Math.floor(h / 24);
  if (d >= 1) return `${d} 天 ${h % 24} 小时后重置`;
  if (h >= 1) return `${h}h ${m}m 后恢复`;
  return `${Math.max(m, 1)}m 后恢复`;
}

function fmtClock(epochSecs) {
  const d = new Date(epochSecs * 1000);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function currencySymbol(code) {
  return { CNY: '¥', USD: '$', EUR: '€' }[code] || `${code} `;
}

function renderRow(row, stale) {
  const p = row.limit > 0 ? Math.round((row.used / row.limit) * 100) : 0;
  const lv = pctLevel(p);
  const fillCls = ['fill', lv, stale ? 'error' : ''].filter(Boolean).join(' ');
  const div = document.createElement('div');
  div.className = 'limit-row' + (stale ? ' stale' : '');
  div.innerHTML = `
    <div class="limit-meta">
      <span class="limit-name">${row.name}</span>
      <span class="limit-reset" data-reset="${row.reset_at}">${fmtCountdown(row.reset_at)}</span>
    </div>
    <div class="limit-bar">
      <div class="track"><div class="${fillCls}" style="width:${Math.min(p, 100)}%"></div></div>
      <span class="pct ${lv}">${p}%</span>
      <span class="limit-nums">${row.used} / ${row.limit}</span>
    </div>`;
  return div;
}

function renderOk(payload, stale) {
  const { summary, limits, extra_usage } = payload;

  // 横条：第一行 = summary（缺省取 limits[0]），第二行 = 5h 窗口（缺省取 limits[1]）
  const bar1 = summary || limits[0] || null;
  const bar2 = limits.find(l => l.window_minutes === 300) || limits[1] || null;
  setBarRow(1, bar1);
  setBarRow(2, bar2);
  bar.title = '';

  // 卡片行
  rowsEl.innerHTML = '';
  countdownTargets = [];
  const append = row => {
    if (!row) return;
    const el = renderRow(row, stale);
    rowsEl.appendChild(el);
    countdownTargets.push(el.querySelector('.limit-reset'));
  };
  if (summary) append(summary);
  limits.forEach(append);
  if (!summary && limits.length === 0) {
    rowsEl.innerHTML = '<div class="limit-row limit-name">暂无限额数据</div>';
  }

  // Extra Usage
  if (extra_usage) {
    extraRow.hidden = false;
    extraVal.textContent = `${currencySymbol(extra_usage.currency)} ${(extra_usage.balance_cents / 100).toFixed(2)}`;
  } else {
    extraRow.hidden = true;
  }

  refreshTimeEl.textContent = `${fmtClock(payload.fetched_at)} 刷新${stale ? '（旧）' : ''}`;
}

function renderError(message) {
  errorEl.hidden = false;
  errorEl.textContent = `⚠ ${message}`;
  document.getElementById('bar-fill-1').className = 'fill error';
  document.getElementById('bar-fill-2').className = 'fill error';
  document.getElementById('bar-pct-1').textContent = '⚠';
  document.getElementById('bar-pct-1').className = 'pct warn';
  document.getElementById('bar-pct-2').textContent = '⚠';
  document.getElementById('bar-pct-2').className = 'pct warn';
  bar.title = message;
}

// ============ 登录视图（needs_login 时替代数据行） ============

function setLoginStatus(text) {
  loginStatusEl.hidden = false;
  loginStatusText.textContent = text;
}

function showLoginIdle() {
  loginActive = false;
  loginExpiresAt = 0;
  loginIdle.hidden = false;
  loginPanel.hidden = true;
  loginStatusEl.hidden = true;
  loginCountdownEl.textContent = '';
  loginView.hidden = false;
  // 卡片数据区清空，横条置为「--」
  rowsEl.innerHTML = '';
  countdownTargets = [];
  extraRow.hidden = true;
  refreshTimeEl.textContent = '--';
  for (const i of [1, 2]) {
    const fill = document.getElementById(`bar-fill-${i}`);
    fill.className = 'fill error';
    fill.style.width = '0%';
    const pct = document.getElementById(`bar-pct-${i}`);
    pct.textContent = '--';
    pct.className = 'pct';
  }
  bar.title = '未登录';
}

function hideLoginView() {
  loginActive = false;
  loginExpiresAt = 0;
  loginView.hidden = true;
}

function fmtLoginCountdown() {
  const diff = loginExpiresAt - Date.now();
  if (diff <= 0) return '已过期';
  const s = Math.ceil(diff / 1000);
  const m = Math.floor(s / 60);
  return `有效期 ${m}:${String(s % 60).padStart(2, '0')}`;
}

async function startLogin() {
  btnLogin.disabled = true;
  setLoginStatus('正在申请设备码…');
  try {
    const s = await invoke('start_login');
    loginActive = true;
    loginIdle.hidden = true;
    loginPanel.hidden = false;
    document.getElementById('login-user-code').textContent = s.user_code;
    document.getElementById('login-link').textContent =
      s.verification_uri_complete || s.verification_uri;
    loginExpiresAt = Date.now() + s.expires_in * 1000;
    loginCountdownEl.textContent = fmtLoginCountdown();
    loginStatusText.textContent = '等待授权…';
  } catch (e) {
    loginStatusText.textContent = `发起登录失败: ${e}`;
  } finally {
    btnLogin.disabled = false;
  }
}

function onLoginUpdate(e) {
  const p = e.payload;
  switch (p.status) {
    case 'success':
      loginStatusText.textContent = '登录成功，正在获取数据…';
      loginCountdownEl.textContent = '';
      // usage-update ok 到达后自动切回数据视图（后端已触发立即刷新）
      break;
    case 'cancelled':
      showLoginIdle();
      setLoginStatus('已取消登录');
      break;
    case 'expired':
      showLoginIdle();
      setLoginStatus('登录已过期，请重新发起');
      break;
    case 'denied':
      showLoginIdle();
      setLoginStatus('已取消授权');
      break;
    case 'error':
      showLoginIdle();
      setLoginStatus(`登录失败: ${p.message}`);
      break;
  }
}

function onUsageUpdate(event) {
  const p = event.payload;
  errorEl.hidden = true;
  if (p.kind === 'ok') {
    hideLoginView();
    renderOk(p, false);
  } else if (p.needs_login) {
    // 需要登录：卡片显示登录视图（会话进行中则保持当前进度）
    if (!loginActive) showLoginIdle();
  } else {
    hideLoginView();
    // 失败：显示错误 + 上次成功数据（置灰标注）
    if (p.stale) {
      renderOk(p.stale, true);
      renderError(p.message);
    } else {
      rowsEl.innerHTML = '';
      countdownTargets = [];
      extraRow.hidden = true;
      refreshTimeEl.textContent = '--';
      renderError(p.message);
    }
  }
}

// ============ 交互 ============

// 自定义拖拽：替代 data-tauri-drag-region 的系统移动循环——后者会把窗口钳制在
// 显示器范围内，横条贴屏幕顶时（窗口 36px 透明边距出屏）到不了边；自定义
// setPosition 无此限制，可自由拖到任意边缘，位置由 main.rs 按交集判定恢复。
const mainWindow = tauri.window.getCurrentWindow();
let drag = null; // { id, sx, sy, wx, wy, nx, ny, raf }

function applyDragMove() {
  if (!drag || drag.raf) return;
  // rAF 合并 pointermove，避免高频 IPC
  drag.raf = requestAnimationFrame(() => {
    drag.raf = 0;
    if (!drag) return;
    mainWindow
      .setPosition(new tauri.dpi.PhysicalPosition(drag.nx, drag.ny))
      .catch(console.error);
  });
}

bar.addEventListener('pointerdown', async e => {
  if (e.button !== 0) return;
  try {
    const pos = await mainWindow.outerPosition(); // 物理像素
    drag = {
      id: e.pointerId,
      sx: e.screenX,
      sy: e.screenY,
      wx: pos.x,
      wy: pos.y,
      nx: pos.x,
      ny: pos.y,
      raf: 0,
    };
    bar.setPointerCapture(e.pointerId);
    bar.classList.add('dragging');
  } catch (err) { console.error(err); }
});

bar.addEventListener('pointermove', e => {
  if (!drag || e.pointerId !== drag.id) return;
  // 必须用 screenX/Y（屏幕坐标）：clientX/Y 相对窗口视口，窗口一动 clientX
  // 就反向变化，形成反馈回路导致疯狂抖动。screenX/Y 与窗口位置无关，
  // 屏幕逻辑像素差 × dpr = 物理像素位移。
  const dpr = window.devicePixelRatio || 1;
  drag.nx = drag.wx + Math.round((e.screenX - drag.sx) * dpr);
  drag.ny = drag.wy + Math.round((e.screenY - drag.sy) * dpr);
  applyDragMove();
});

function endDrag(e) {
  if (!drag || (e && e.pointerId !== drag.id)) return;
  if (drag.raf) cancelAnimationFrame(drag.raf);
  try { bar.releasePointerCapture(drag.id); } catch (_) { /* 已释放 */ }
  drag = null;
  bar.classList.remove('dragging');
}
bar.addEventListener('pointerup', endDrag);
bar.addEventListener('pointercancel', endDrag);
window.addEventListener('blur', () => endDrag());

// 悬停 200ms 展开，移出 300ms 收起（与 preview 一致）
let openTimer, closeTimer;
widget.addEventListener('mouseenter', () => {
  clearTimeout(closeTimer);
  openTimer = setTimeout(() => widget.classList.add('expanded'), 200);
});
widget.addEventListener('mouseleave', () => {
  clearTimeout(openTimer);
  closeTimer = setTimeout(() => widget.classList.remove('expanded'), 300);
});
// 失焦自动收起（可通过 settings.json collapse_on_blur 关闭，默认开启）；
// 登录会话进行中不收起——用户需在浏览器授权页对照用户码
let collapseOnBlur = true;
window.addEventListener('blur', () => {
  if (collapseOnBlur && !loginActive) widget.classList.remove('expanded');
});

// 登录视图按钮
btnLogin.addEventListener('click', startLogin);
btnOpenAuth.addEventListener('click', async () => {
  try { await invoke('open_auth_url'); }
  catch (e) { setLoginStatus(`打开授权页失败: ${e}`); }
});
btnCancelLogin.addEventListener('click', () => {
  invoke('cancel_login').catch(console.error);
});

// 手动刷新
btnRefresh.addEventListener('click', async () => {
  btnRefresh.disabled = true;
  try { await invoke('refresh_now'); } catch (e) { console.error(e); }
  setTimeout(() => { btnRefresh.disabled = false; }, 2000);
});

// 置顶开关（托盘与卡片双向同步）
chkTop.addEventListener('change', () => {
  invoke('set_always_on_top', { enabled: chkTop.checked }).catch(console.error);
});
listen('always-on-top-changed', e => { chkTop.checked = e.payload; });

// 倒计时：本地每秒 tick（限额重置时间 + 设备码有效期）
setInterval(() => {
  for (const el of countdownTargets) {
    el.textContent = fmtCountdown(el.dataset.reset);
  }
  if (loginActive && loginExpiresAt > 0) {
    loginCountdownEl.textContent = fmtLoginCountdown();
  }
}, 1000);

// 初始化
(async () => {
  try {
    const s = await invoke('get_settings');
    chkTop.checked = s.always_on_top !== false;
    collapseOnBlur = s.collapse_on_blur !== false;
  } catch (e) { console.error(e); }
  await listen('usage-update', onUsageUpdate);
  await listen('login-update', onLoginUpdate);
})();
