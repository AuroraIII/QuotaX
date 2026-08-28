#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

const WINDOW_W: i32 = 320;
const WINDOW_H: i32 = 340;

/// 手动刷新信号（托盘/按钮 → poller）
struct RefreshSignal(Arc<tokio::sync::Notify>);
/// 置顶状态
struct AlwaysOnTop(AtomicBool);
/// 托盘「置顶」菜单项（状态同步用）
struct TrayTopItem(Mutex<Option<CheckMenuItem<tauri::Wry>>>);
/// 拖动防抖：上次落盘时间
struct LastSave(Mutex<Option<Instant>>);

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    x: Option<i32>,
    y: Option<i32>,
    always_on_top: Option<bool>,
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
        .manage(LastSave(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            refresh_now,
            set_always_on_top,
            get_settings
        ])
        .setup(|app| {
            let settings = load_settings();

            // ---- 窗口位置恢复 ----
            if let Some(win) = app.get_webview_window("main") {
                let mut restored = false;
                if let (Some(x), Some(y)) = (settings.x, settings.y) {
                    // 坐标须落在某个显示器内，否则回退默认位置
                    if let Ok(monitors) = win.available_monitors() {
                        let inside = monitors.iter().any(|m| {
                            let mp = m.position();
                            let ms = m.size();
                            x >= mp.x
                                && y >= mp.y
                                && x + WINDOW_W <= mp.x + ms.width as i32
                                && y + WINDOW_H <= mp.y + ms.height as i32
                        });
                        if inside {
                            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
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
                            mp.x + ms.width as i32 - WINDOW_W - 24,
                            mp.y + ms.height as i32 - WINDOW_H - 64,
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
            let top_item =
                CheckMenuItem::with_id(app, "top", "置顶", true, true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&refresh_item, &top_item, &quit_item])?;
            *app.state::<TrayTopItem>().0.lock().unwrap() = Some(top_item.clone());

            TrayIconBuilder::with_id("quotax-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("QuotaX — Kimi Code 额度")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "refresh" => app.state::<RefreshSignal>().0.notify_one(),
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
