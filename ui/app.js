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

// 当前各行的重置时间（ISO 字符串）→ 对应 DOM 引用，每秒 tick 更新
let countdownTargets = [];

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

function onUsageUpdate(event) {
  const p = event.payload;
  errorEl.hidden = true;
  if (p.kind === 'ok') {
    renderOk(p, false);
  } else {
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
window.addEventListener('blur', () => widget.classList.remove('expanded'));

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

// 倒计时：本地每秒 tick
setInterval(() => {
  for (const el of countdownTargets) {
    el.textContent = fmtCountdown(el.dataset.reset);
  }
}, 1000);

// 初始化
(async () => {
  try {
    const s = await invoke('get_settings');
    chkTop.checked = s.always_on_top !== false;
  } catch (e) { console.error(e); }
  await listen('usage-update', onUsageUpdate);
})();
