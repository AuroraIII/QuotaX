//! 轮询（间隔可配置，默认 60s）+ 指数退避 + 401 重读凭证/刷新重试，经 Tauri Event 推送前端；
//! 任一限额行用量 ≥ 80% 时托盘切换橙色角标图标。
//! 凭证写回失败（如 CLI 持有文件句柄）时暂存 pending_write，每轮重试直至成功或被 CLI 更新取代。

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::image::Image;
use tauri::AppHandle;
use tauri::Emitter;

use crate::credentials::{self, Credentials, CredError};
use crate::usage::{self, RawUsageResponse, UsagePayload};

/// 失败退避：1m → 2m → 5m（上限 5m），成功后重置
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];
/// 托盘超量提醒阈值（%），与 UI 阈值变色一致
const TRAY_WARN_PCT: f64 = 80.0;

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum UpdateEvent {
    Ok {
        #[serde(flatten)]
        payload: UsagePayload,
    },
    Error {
        message: String,
        /// 上次成功数据（置灰展示）
        stale: Option<UsagePayload>,
    },
}

fn reqwest_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时".to_string()
    } else if e.is_connect() {
        "无法连接服务器".to_string()
    } else {
        format!("网络错误: {e}")
    }
}

async fn get_usage(
    client: &reqwest::Client,
    token: &str,
) -> Result<RawUsageResponse, (bool, String)> {
    let resp = client
        .get(usage::usage_url())
        .bearer_auth(token)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| (false, reqwest_error(&e)))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| (false, reqwest_error(&e)))?;
    match status {
        200 => serde_json::from_str(&body)
            .map_err(|e| (false, format!("响应解析失败: {e}"))),
        401 => Err((true, "token 失效 (401)".to_string())),
        s => Err((false, format!("服务端错误 (HTTP {s}): {}", body.chars().take(200).collect::<String>()))),
    }
}

/// 单次抓取全流程：
/// 0. 先处理上轮遗留的待写回凭证（见 retry_pending_write）；
/// 1. 读凭证（每次重新读文件，天然兼容 CLI 侧刷新/旋转）；
/// 2. token 临期 → refresh（refresh 失败且可能是 refresh_token 被 CLI 旋转时重读文件再试一次）；
/// 3. 401 → 重读文件（CLI 刚刷过 token）重试一次，仍失败再 refresh + 重试。
pub async fn fetch_once(
    client: &reqwest::Client,
    pending_write: &mut Option<Credentials>,
) -> Result<UsagePayload, String> {
    retry_pending_write(pending_write);

    let creds = credentials::read().map_err(|e| e.to_string())?;

    let mut token = creds.access_token.clone();
    if credentials::is_stale(&creds) {
        match refresh_guarded(client, &creds, pending_write).await {
            Ok(fresh) => token = fresh.access_token,
            Err(msg) => {
                // 刷新失败：仍用旧 token 试一次（时钟偏差时可能仍有效）
                log::warn_or_print(&format!("[quotax] {msg}，先用现有 token 尝试"));
            }
        }
    }

    match get_usage(client, &token).await {
        Ok(raw) => Ok(usage::to_payload(&raw)),
        Err((is_401, msg)) => {
            if !is_401 {
                return Err(msg);
            }
            // 401：重新读文件（CLI 可能刚刷新过），文件 token 变了则直接重试
            let reread = credentials::read().map_err(|e| e.to_string())?;
            if reread.access_token != token {
                return get_usage(client, &reread.access_token)
                    .await
                    .map(|raw| usage::to_payload(&raw))
                    .map_err(|(_, m)| m);
            }
            // 文件没变 → 自己 refresh 再重试
            let fresh = refresh_guarded(client, &reread, pending_write).await?;
            get_usage(client, &fresh.access_token)
                .await
                .map(|raw| usage::to_payload(&raw))
                .map_err(|(_, m)| m)
        }
    }
}

/// 重试上轮遗留的待写回凭证（写回被拒时的兜底，每轮 fetch 前调用）：
/// - 文件 token 与 pending 一致 → 实际已写入，视为成功；
/// - 文件 token 比 pending 更新（CLI 已自行刷新）→ 丢弃 pending；
/// - 否则重试原子写回，成功清空，失败保留等下轮（不刷日志）。
fn retry_pending_write(pending: &mut Option<Credentials>) {
    let Some(creds) = pending.as_ref() else { return };
    let Ok(cur) = credentials::read() else {
        return; // 读失败（可能正被 CLI 写入），保留下轮再试
    };
    if cur.access_token == creds.access_token {
        log::warn_or_print("[quotax] 凭证写回重试成功");
        *pending = None;
    } else if cur.expires_at > creds.expires_at {
        log::warn_or_print("[quotax] 检测到 CLI 已刷新凭证，丢弃待写回");
        *pending = None;
    } else if credentials::write_atomic(&credentials::credentials_file(), creds).is_ok() {
        log::warn_or_print("[quotax] 凭证写回重试成功");
        *pending = None;
    }
}

/// 刷新并原子写回；若 refresh 端点拒绝（refresh_token 已被 CLI 旋转），
/// 重读文件一次：CLI 已刷新过则直接复用其结果。
/// 写回失败（如 CLI 持有文件句柄）时暂存 pending_write 由主循环重试。
async fn refresh_guarded(
    client: &reqwest::Client,
    creds: &Credentials,
    pending_write: &mut Option<Credentials>,
) -> Result<Credentials, String> {
    match credentials::refresh(client, &creds.refresh_token).await {
        Ok(fresh) => {
            if let Err(e) = credentials::write_atomic(&credentials::credentials_file(), &fresh) {
                // 仅首次失败打印日志，重试过程由 retry_pending_write 按状态变化打印
                if pending_write.is_none() {
                    log::warn_or_print(&format!("[quotax] 写回凭证失败（将自动重试）: {e}"));
                }
                *pending_write = Some(fresh.clone());
            }
            Ok(fresh)
        }
        Err(err) => {
            let msg = err.to_string();
            if matches!(err, CredError::Refresh { .. }) {
                if let Ok(reread) = credentials::read() {
                    if !credentials::is_stale(&reread) && reread.access_token != creds.access_token
                    {
                        return Ok(reread); // CLI 刚刷新过，直接复用
                    }
                }
            }
            Err(msg)
        }
    }
}

/// 各行 used/limit 的最高百分比（0–100；limit=0 的行忽略）。
fn max_used_pct(payload: &UsagePayload) -> f64 {
    payload
        .summary
        .iter()
        .chain(payload.limits.iter())
        .filter(|r| r.limit > 0)
        .map(|r| r.used as f64 / r.limit as f64 * 100.0)
        .fold(0.0, f64::max)
}

/// 解码内置的超量提醒托盘图标（icons/icon-warn.ico，取 ≤48px 的最大尺寸适配托盘 DPI）。
fn warn_tray_icon() -> Option<Image<'static>> {
    let dir = ico::IconDir::read(std::io::Cursor::new(include_bytes!(
        "../icons/icon-warn.ico"
    )))
    .ok()?;
    let entry = dir
        .entries()
        .iter()
        .filter(|e| e.width() <= 48 && e.height() <= 48)
        .max_by_key(|e| e.width())
        .or_else(|| dir.entries().last())?;
    let img = entry.decode().ok()?;
    Some(Image::new_owned(
        img.rgba_data().to_vec(),
        img.width(),
        img.height(),
    ))
}

/// 主轮询循环：立即执行一次，之后按间隔/退避；notify 触发立即轮询（手动刷新）。
pub async fn run(app: AppHandle, notify: std::sync::Arc<tokio::sync::Notify>) {
    let client = reqwest::Client::builder()
        .build()
        .expect("failed to build http client");
    let stale_cache: Mutex<Option<UsagePayload>> = Mutex::new(None);
    let mut fail_count = 0usize;
    // 写回失败的凭证暂存：每轮 fetch 前重试，直至写入成功或被 CLI 更新取代
    let mut pending_write: Option<Credentials> = None;

    // 托盘超量提醒：预解码告警图标；仅状态翻转时切换，避免每轮重建原生图标
    let warn_icon = warn_tray_icon();
    let default_icon = app.default_window_icon().map(|i| i.to_owned());
    let mut tray_warn = false;

    loop {
        let result = fetch_once(&client, &mut pending_write).await;
        let event = match result {
            Ok(payload) => {
                fail_count = 0;
                // 托盘超量提醒：任一行 ≥80% 切橙色角标，回落恢复默认图标
                let warn = max_used_pct(&payload) >= TRAY_WARN_PCT;
                if warn != tray_warn {
                    if let Some(tray) = app.tray_by_id("quotax-tray") {
                        let icon = if warn {
                            warn_icon.as_ref()
                        } else {
                            default_icon.as_ref()
                        };
                        if let Some(img) = icon {
                            match tray.set_icon(Some(img.clone())) {
                                Ok(()) => tray_warn = warn,
                                Err(e) => eprintln!("[quotax] 托盘图标切换失败: {e}"),
                            }
                        }
                    }
                }
                // 成功日志：输出 usage JSON（验收观测用）
                if let Ok(json) = serde_json::to_string(&payload) {
                    println!("[quotax] usage ok: {json}");
                }
                *stale_cache.lock().unwrap() = Some(payload.clone());
                UpdateEvent::Ok { payload }
            }
            Err(message) => {
                let stale = stale_cache.lock().unwrap().clone();
                let backoff_msg = BACKOFF
                    .get(fail_count.min(BACKOFF.len() - 1))
                    .map(|d| format!("，{}s 后重试", d.as_secs()))
                    .unwrap_or_default();
                log::warn_or_print(&format!("[quotax] 刷新失败: {message}{backoff_msg}"));
                fail_count += 1;
                UpdateEvent::Error { message, stale }
            }
        };
        if let Err(e) = app.emit("usage-update", &event) {
            eprintln!("[quotax] emit 失败: {e}");
        }

        let wait = if fail_count == 0 {
            // 每轮读取最新配置作为间隔（默认 60s，钳制 30–600，免重启生效）
            Duration::from_secs(crate::load_settings().poll_interval_secs_clamped())
        } else {
            BACKOFF[fail_count.saturating_sub(1).min(BACKOFF.len() - 1)]
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = notify.notified() => {
                // 手动刷新被唤醒，立即进入下一轮抓取
            }
        }
    }
}

/// 极简日志（无 log 依赖时直接打印）。
mod log {
    #[allow(dead_code)]
    pub fn warn_or_print(msg: &str) {
        println!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warn_icon_decodes() {
        let icon = warn_tray_icon().expect("icon-warn.ico should decode");
        assert_eq!((icon.width(), icon.height()), (48, 48));
        assert_eq!(icon.rgba().len(), 48 * 48 * 4);
    }

    #[test]
    fn max_used_pct_across_rows() {
        let row = |used: u64, limit: u64| crate::usage::UsageRow {
            name: String::new(),
            short: String::new(),
            window_minutes: 0,
            used,
            limit,
            reset_at: String::new(),
        };
        let payload = UsagePayload {
            summary: Some(row(50, 100)),
            limits: vec![row(90, 100), row(1, 0)],
            extra_usage: None,
            parallel_limit: None,
            membership: None,
            fetched_at: 0,
        };
        assert_eq!(max_used_pct(&payload), 90.0);
        assert!(max_used_pct(&payload) >= TRAY_WARN_PCT);
        let low = UsagePayload {
            summary: Some(row(79, 100)),
            limits: vec![],
            extra_usage: None,
            parallel_limit: None,
            membership: None,
            fetched_at: 0,
        };
        assert!(max_used_pct(&low) < TRAY_WARN_PCT);
    }
}
