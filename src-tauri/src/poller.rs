//! 60s 轮询 + 指数退避 + 401 重读凭证/刷新重试，经 Tauri Event 推送前端。

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use tauri::Emitter;

use crate::credentials::{self, Credentials, CredError};
use crate::usage::{self, RawUsageResponse, UsagePayload};

pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// 失败退避：1m → 2m → 5m（上限 5m），成功后重置
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

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
/// 1. 读凭证（每次重新读文件，天然兼容 CLI 侧刷新/旋转）；
/// 2. token 临期 → refresh（refresh 失败且可能是 refresh_token 被 CLI 旋转时重读文件再试一次）；
/// 3. 401 → 重读文件（CLI 刚刷过 token）重试一次，仍失败再 refresh + 重试。
pub async fn fetch_once(client: &reqwest::Client) -> Result<UsagePayload, String> {
    let creds = credentials::read().map_err(|e| e.to_string())?;

    let mut token = creds.access_token.clone();
    if credentials::is_stale(&creds) {
        match refresh_guarded(client, &creds).await {
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
            let fresh = refresh_guarded(client, &reread).await?;
            get_usage(client, &fresh.access_token)
                .await
                .map(|raw| usage::to_payload(&raw))
                .map_err(|(_, m)| m)
        }
    }
}

/// 刷新并原子写回；若 refresh 端点拒绝（refresh_token 已被 CLI 旋转），
/// 重读文件一次：CLI 已刷新过则直接复用其结果。
async fn refresh_guarded(
    client: &reqwest::Client,
    creds: &Credentials,
) -> Result<Credentials, String> {
    match credentials::refresh(client, &creds.refresh_token).await {
        Ok(fresh) => {
            if let Err(e) = credentials::write_atomic(&credentials::credentials_file(), &fresh) {
                log::warn_or_print(&format!("[quotax] 写回凭证失败: {e}"));
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

/// 主轮询循环：立即执行一次，之后按间隔/退避；notify 触发立即轮询（手动刷新）。
pub async fn run(app: AppHandle, notify: std::sync::Arc<tokio::sync::Notify>) {
    let client = reqwest::Client::builder()
        .build()
        .expect("failed to build http client");
    let stale_cache: Mutex<Option<UsagePayload>> = Mutex::new(None);
    let mut fail_count = 0usize;

    loop {
        let result = fetch_once(&client).await;
        let event = match result {
            Ok(payload) => {
                fail_count = 0;
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
            POLL_INTERVAL
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
