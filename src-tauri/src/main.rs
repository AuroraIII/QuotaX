#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod credentials;
mod poller;
mod usage;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

// 窗口严格等于可视内容外框，零透明边距——透明区域不穿透鼠标事件，任何边距都是
// 拦截下方窗口点击的「空气墙」（B.11 用户决策：点到下方内容比 box-shadow 投影重要，
// 类比搜狗输入法：框体外所有区域都可点）。因此 CSS 侧已移除全部 box-shadow，
// 窗口随展开/收起动态调整大小、原点恒定不动，展开只向右下扩：
// - 收起态 210×42：即横条本体；
// - 展开态 300×(48+卡片高)：卡片 300 宽、顶边 y=48（横条 42 + 6px 间隙），
//   高度由前端实测卡片高上报、ResizeObserver 跟随（内容多变：错误行/登录视图/行数）。
// 原点恒定 → 横条永远钉在窗口 (0,0)，切换全程内容零位移、无闪跳（B.10 闪跳回归的教训：
// CSS 边距切换与窗口平移无法跨进程同帧变更）。
const BAR_W: i32 = 210;
const BAR_H: i32 = 42;
const COLLAPSED_W: i32 = BAR_W; // 210
const COLLAPSED_H: i32 = BAR_H; // 42
const CARD_W: i32 = 300;
/// 卡片顶边在窗口中的 y：CSS top:48（横条 42 + 6px 间隙）
const CARD_TOP: i32 = 48;
const EXPANDED_W: i32 = CARD_W; // 300
/// 卡片高度防御性上限（#rows 已限高 144px，正常远低于此）
const MAX_CARD_H: i32 = 560;

/// 展开态窗口高 = 卡片顶 + 卡片实测高
fn expanded_h(card_h: i32) -> i32 {
    CARD_TOP + card_h.clamp(60, MAX_CARD_H)
}

/// CSS 像素 → 物理像素（outer_position/set_position 均为物理像素）
fn css_px(v: i32, scale: f64) -> i32 {
    (v as f64 * scale).round() as i32
}

/// 手动刷新信号（托盘/按钮 → poller；auth 登录成功后也经此立即抓取）
struct RefreshSignal(Arc<tokio::sync::Notify>);
/// 置顶状态
struct AlwaysOnTop(AtomicBool);
/// 托盘「置顶」菜单项（状态同步用）
struct TrayTopItem(Mutex<Option<CheckMenuItem<tauri::Wry>>>);
/// 托盘「登录账号」菜单项（poller 按 needs_login 翻转可用状态）
struct TrayLoginItem(Mutex<Option<MenuItem<tauri::Wry>>>);
/// 拖动防抖：上次落盘时间
struct LastSave(Mutex<Option<Instant>>);
/// 当前窗口几何（展开态, 卡片高）：尺寸切换去重——展开周期内卡片长大会重复调用
struct WidgetGeom(Mutex<(bool, i32)>);

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    x: Option<i32>,
    y: Option<i32>,
    always_on_top: Option<bool>,
    /// 轮询间隔（秒），默认 60，钳制在 30–600（poller 每轮读取，免重启生效）
    poll_interval_secs: Option<u64>,
    /// 窗口失焦自动收起展开卡片，默认 true
    collapse_on_blur: Option<bool>,
}

impl Settings {
    fn poll_interval_secs_clamped(&self) -> u64 {
        self.poll_interval_secs.unwrap_or(60).clamp(30, 600)
    }
}

fn settings_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "QuotaX")
        .expect("cannot resolve config dir")
        .config_dir()
        .join("settings.json")
}

fn load_settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(path, body);
    }
}

/// 保存窗口位置（带 500ms 防抖 + 强制模式）
fn save_window_pos(app: &AppHandle, force: bool) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if !force {
        let st = app.state::<LastSave>();
        let last = st.0.lock().unwrap();
        if let Some(t) = *last {
            if t.elapsed() < Duration::from_millis(500) {
                return; // 节流：拖动中不频繁落盘；最终位置由 exit 时 force 保存
            }
        }
    }
    if let Ok(pos) = win.outer_position() {
        // 两态窗口原点相同（展开只向右下扩尺寸、不平移），直接落盘，无需按展开态归一化
        let mut s = load_settings();
        s.x = Some(pos.x);
        s.y = Some(pos.y);
        save_settings(&s);
        *app.state::<LastSave>().0.lock().unwrap() = Some(Instant::now());
    }
}

fn apply_always_on_top(app: &AppHandle, enabled: bool) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(enabled);
    }
    app.state::<AlwaysOnTop>().0.store(enabled, Ordering::SeqCst);
    if let Some(item) = app.state::<TrayTopItem>().0.lock().unwrap().as_ref() {
        let _ = item.set_checked(enabled);
    }
    let _ = app.emit("always-on-top-changed", enabled);
}

#[tauri::command]
fn refresh_now(app: AppHandle) {
    app.state::<RefreshSignal>().0.notify_one();
}

#[tauri::command]
fn set_always_on_top(app: AppHandle, enabled: bool) {
    apply_always_on_top(&app, enabled);
    let mut s = load_settings();
    s.always_on_top = Some(enabled);
    save_settings(&s);
}

/// 展开/收起切换：窗口原点恒定（零边距，横条固定于窗口 (0,0)），仅向右下
/// 扩缩尺寸并贴合实际内容，窗口外不存在拦截点击的透明区；全程不平移窗口。
/// 展开时高度按前端实测卡片高度计算（内容多变：错误行/登录视图/限额行数），
/// 同一展开周期内卡片长高会重复调用本命令，按 (expanded, card_h) 去重
#[tauri::command]
fn set_widget_expanded(app: AppHandle, expanded: bool, card_h: Option<i32>) {
    let ch = card_h.unwrap_or(0);
    let geom = app.state::<WidgetGeom>();
    {
        let mut g = geom.0.lock().unwrap();
        if *g == (expanded, ch) {
            return;
        }
        *g = (expanded, ch);
    }
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let (w, h) = if expanded {
        (EXPANDED_W, expanded_h(ch))
    } else {
        (COLLAPSED_W, COLLAPSED_H)
    };
    let _ = win.set_size(tauri::PhysicalSize::new(
        css_px(w, scale) as u32,
        css_px(h, scale) as u32,
    ));
}

#[tauri::command]
fn get_settings() -> Settings {
    let app_settings = load_settings();
    app_settings
}

fn main() {
    tauri::Builder::default()
        .manage(RefreshSignal(Arc::new(tokio::sync::Notify::new())))
        .manage(AlwaysOnTop(AtomicBool::new(true)))
        .manage(TrayTopItem(Mutex::new(None)))
        .manage(TrayLoginItem(Mutex::new(None)))
        .manage(auth::LoginState::default())
        .manage(LastSave(Mutex::new(None)))
        .manage(WidgetGeom(Mutex::new((false, 0))))
        .invoke_handler(tauri::generate_handler![
            refresh_now,
            set_always_on_top,
            get_settings,
            set_widget_expanded,
            auth::start_login,
            auth::cancel_login,
            auth::open_auth_url
        ])
        .setup(|app| {
            let settings = load_settings();

            // ---- 窗口位置恢复（启动即为收起态 210×42）----
            if let Some(win) = app.get_webview_window("main") {
                let scale = win.scale_factor().unwrap_or(1.0);
                let cw = css_px(COLLAPSED_W, scale);
                let ch = css_px(COLLAPSED_H, scale);
                // 两态窗口原点相同，存档即窗口原点，直接恢复
                let mut restored = false;
                if let (Some(x), Some(y)) = (settings.x, settings.y) {
                    let (cx, cy) = (x, y);
                    // 坐标与某显示器有交集即可恢复（允许贴边/少量出屏，如横条贴屏幕顶时
                    // 窗口上缘出屏）；完全离开所有显示器才回退默认位置
                    if let Ok(monitors) = win.available_monitors() {
                        let inside = monitors.iter().any(|m| {
                            let mp = m.position();
                            let ms = m.size();
                            cx < mp.x + ms.width as i32
                                && cx + cw > mp.x
                                && cy < mp.y + ms.height as i32
                                && cy + ch > mp.y
                        });
                        if inside {
                            let _ = win.set_position(tauri::PhysicalPosition::new(cx, cy));
                            restored = true;
                        }
                    }
                }
                if !restored {
                    // 默认：主屏右下角
                    if let Ok(Some(m)) = win.primary_monitor() {
                        let mp = m.position();
                        let ms = m.size();
                        let _ = win.set_position(tauri::PhysicalPosition::new(
                            mp.x + ms.width as i32 - cw - 24,
                            mp.y + ms.height as i32 - ch - 64,
                        ));
                    }
                }

                // 置顶初始状态
                let top = settings.always_on_top.unwrap_or(true);
                let _ = win.set_always_on_top(top);
                app.state::<AlwaysOnTop>().0.store(top, Ordering::SeqCst);

                // 拖动位置持久化（Moved 事件在 Rust 侧监听，无需前端权限）
                let app_handle = app.handle().clone();
                win.on_window_event(move |e| {
                    if let WindowEvent::Moved(_) = e {
                        save_window_pos(&app_handle, false);
                    }
                });
            }

            // ---- 系统托盘 ----
            let refresh_item = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
            // 登录账号：默认禁用，仅 needs_login（凭证缺失/refresh 被拒）时由 poller 翻转可用
            let login_item = MenuItem::with_id(app, "login", "登录账号", false, None::<&str>)?;
            let top_item =
                CheckMenuItem::with_id(app, "top", "置顶", true, true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&refresh_item, &login_item, &top_item, &quit_item])?;
            *app.state::<TrayTopItem>().0.lock().unwrap() = Some(top_item.clone());
            *app.state::<TrayLoginItem>().0.lock().unwrap() = Some(login_item.clone());

            TrayIconBuilder::with_id("quotax-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("QuotaX — Kimi Code 额度")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "refresh" => app.state::<RefreshSignal>().0.notify_one(),
                    "login" => {
                        // 需要登录时可用：聚焦主窗，卡片正展示登录视图
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "top" => {
                        let cur = app.state::<AlwaysOnTop>().0.load(Ordering::SeqCst);
                        apply_always_on_top(app, !cur);
                        let mut s = load_settings();
                        s.always_on_top = Some(!cur);
                        save_settings(&s);
                    }
                    "quit" => {
                        save_window_pos(app, true);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            // ---- 轮询任务 ----
            let notify = app.state::<RefreshSignal>().0.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(poller::run(handle, notify));

            Ok(())
        })
        .on_window_event(|window, event| {
            // 窗口失焦自动收起由前端处理（blur 事件）；关闭即退出整个应用
            if let WindowEvent::CloseRequested { .. } = event {
                save_window_pos(window.app_handle(), true);
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running QuotaX");
}
