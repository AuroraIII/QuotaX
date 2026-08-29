//! 内置 OAuth 设备码登录（RFC 8628）：凭证文件缺失或 refresh 被拒时的应用内兜底授权路径。
//! 复用优先原则：凭证可用时本模块不参与；登录成功后按 CLI 格式原子写入共用凭证文件，
//! poller 经 RefreshSignal 立即触发一次抓取（它本就每轮重读文件），无需重启。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

use crate::credentials::{self, Credentials};

pub const DEVICE_AUTH_PATH: &str = "/api/oauth/device_authorization";
/// token 轮询允许的连续传输失败次数（超过则终止登录会话）
const MAX_TRANSPORT_FAILURES: u32 = 3;

// ============ 返回前端的会话信息与事件 ============

/// start_login 返回：前端展示用户码 / 授权链接 / 倒计时
#[derive(Debug, Clone, Serialize)]
pub struct LoginSession {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// 设备码有效期（秒）
    pub expires_in: i64,
    /// 建议轮询间隔（秒）
    pub interval: u64,
}

/// login-update 事件载荷（tag = status）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LoginEvent {
    Success,
    Cancelled,
    /// 设备码过期（expired_token 或超过有效期）
    Expired,
    /// 用户在授权页拒绝
    Denied,
    Error { message: String },
}

// ============ 纯函数：响应解析与错误分类（配单元测试） ============

/// device_authorization 端点成功响应（实测样例见 docs/PROMPT-builtin-oauth-login.md）
#[derive(Debug, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_expires_in")]
    pub expires_in: i64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_expires_in() -> i64 {
    1800
}

fn default_interval() -> u64 {
    5
}

/// token 端点单次轮询结果分类（RFC 8628 标准错误码）
#[derive(Debug)]
pub enum TokenPoll {
    /// 200：换 token 成功，凭证已构造
    Success(Credentials),
    /// authorization_pending：继续等待
    Pending,
    /// slow_down：轮询间隔 +5s
    SlowDown,
    /// expired_token / 超过有效期
    Expired,
    /// access_denied：用户取消授权
    Denied,
    /// 其他失败（非标准错误码 / 响应不可解析）
    Failed(String),
}

pub fn classify_token_response(status: u16, body: &str) -> TokenPoll {
    if (200..300).contains(&status) {
        // 成功响应与 refresh 完全同构，复用同一解析
        return match credentials::parse_token_response(body, None) {
            Ok(creds) => TokenPoll::Success(creds),
            Err(e) => TokenPoll::Failed(e.to_string()),
        };
    }
    #[derive(Deserialize)]
    struct ErrResp {
        #[serde(default)]
        error: String,
        #[serde(default)]
        error_description: Option<String>,
    }
    let parsed: Option<ErrResp> = serde_json::from_str(body).ok();
    let error = parsed
        .as_ref()
        .map(|p| p.error.trim().to_string())
        .filter(|e| !e.is_empty());
    match error.as_deref() {
        Some("authorization_pending") => TokenPoll::Pending,
        Some("slow_down") => TokenPoll::SlowDown,
        Some("expired_token") => TokenPoll::Expired,
        Some("access_denied") => TokenPoll::Denied,
        Some(other) => {
            let desc = parsed
                .and_then(|p| p.error_description)
                .unwrap_or_default();
            TokenPoll::Failed(format!("HTTP {status}: {other} {desc}"))
        }
        None => TokenPoll::Failed(format!(
            "HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        )),
    }
}

/// 下轮轮询间隔（slow_down → +5s，RFC 8628）
pub fn next_interval(interval: u64, poll: &TokenPoll) -> u64 {
    match poll {
        TokenPoll::SlowDown => interval + 5,
        _ => interval,
    }
}

// ============ 唯一登录会话状态 ============

struct Session {
    device_code: String,
    /// 授权链接（verification_uri_complete），open_auth_url 复用
    auth_url: String,
    /// 发送 true = 请求取消
    cancel: watch::Sender<bool>,
}

/// 全局唯一登录会话（start_login 重入时先取消旧会话）
#[derive(Default)]
pub struct LoginState(Mutex<Option<Session>>);

/// 会话终结后从状态中移除（按 device_code 匹配，避免误删新会话）
fn take_session(state: &LoginState, device_code: &str) {
    let mut guard = state.0.lock().unwrap();
    if guard
        .as_ref()
        .map(|s| s.device_code == device_code)
        .unwrap_or(false)
    {
        *guard = None;
    }
}

// ============ Tauri 命令 ============

/// 发起设备码登录：申请设备码 → 存唯一会话 → 后台任务轮询 token → 打开一次授权页。
#[tauri::command]
pub async fn start_login(
    app: AppHandle,
    state: State<'_, LoginState>,
) -> Result<LoginSession, String> {
    // 单实例只允许一个登录会话：重入时先取消旧会话（旧任务 emit cancelled 后退出）
    if let Some(old) = state.0.lock().unwrap().take() {
        let _ = old.cancel.send(true);
    }

    let client = reqwest::Client::new();
    let url = format!("{}{}", credentials::oauth_host(), DEVICE_AUTH_PATH);
    let resp = client
        .post(&url)
        .form(&[("client_id", credentials::CLIENT_ID)])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("无法连接授权服务器: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取授权服务器响应失败: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "申请设备码失败 (HTTP {status}): {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    let da: DeviceAuthResponse =
        serde_json::from_str(&body).map_err(|e| format!("解析设备码响应失败: {e}"))?;
    let auth_url = da
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| da.verification_uri.clone());

    let (cancel_tx, cancel_rx) = watch::channel(false);
    *state.0.lock().unwrap() = Some(Session {
        device_code: da.device_code.clone(),
        auth_url: auth_url.clone(),
        cancel: cancel_tx,
    });

    // 后端直接打开一次系统浏览器（前端同时展示链接兜底）
    open_url(&auth_url);

    tauri::async_runtime::spawn(poll_token(
        app.clone(),
        da.device_code.clone(),
        da.expires_in,
        da.interval,
        cancel_rx,
    ));

    Ok(LoginSession {
        user_code: da.user_code,
        verification_uri: da.verification_uri,
        verification_uri_complete: auth_url,
        expires_in: da.expires_in,
        interval: da.interval,
    })
}

/// 取消当前登录会话（无会话时为幂等空操作）。
#[tauri::command]
pub fn cancel_login(state: State<'_, LoginState>) -> Result<(), String> {
    if let Some(session) = state.0.lock().unwrap().as_ref() {
        let _ = session.cancel.send(true);
    }
    Ok(())
}

/// 重新打开当前会话的授权页（无会话时报错）。
#[tauri::command]
pub fn open_auth_url(state: State<'_, LoginState>) -> Result<(), String> {
    let guard = state.0.lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return Err("当前没有进行中的登录会话".into());
    };
    open_url(&session.auth_url);
    Ok(())
}

// ============ 后台轮询任务 ============

/// 单次 token 端点请求（device_code grant），返回 (status, body)。
async fn request_token(
    client: &reqwest::Client,
    url: &str,
    device_code: &str,
) -> Result<(u16, String), String> {
    let resp = client
        .post(url)
        .form(&[
            ("client_id", credentials::CLIENT_ID),
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code",
            ),
            ("device_code", device_code),
        ])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    Ok((status, body))
}

/// 设备码 token 轮询任务：按 interval 循环请求 token 端点，成功则原子写入凭证文件
/// 并 emit success（随后经 RefreshSignal 立即抓取一次）；authorization_pending 继续、
/// slow_down 间隔 +5s、expired_token / access_denied / 连续传输失败 / 超过有效期终止。
async fn poll_token(
    app: AppHandle,
    device_code: String,
    expires_in: i64,
    mut interval: u64,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let client = reqwest::Client::new();
    let url = format!("{}{}", credentials::oauth_host(), credentials::TOKEN_PATH);
    let deadline = Instant::now() + Duration::from_secs(expires_in.max(1) as u64);
    let mut transport_failures = 0u32;
    let mut outcome: Option<LoginEvent> = None;

    while outcome.is_none() {
        if Instant::now() >= deadline {
            // 服务端未及时返回 expired_token 时的本地兜底
            outcome = Some(LoginEvent::Expired);
            break;
        }
        match request_token(&client, &url, &device_code).await {
            Ok((status, body)) => match classify_token_response(status, &body) {
                TokenPoll::Success(creds) => {
                    outcome = Some(
                        match credentials::write_atomic(&credentials::credentials_file(), &creds)
                        {
                            Ok(()) => LoginEvent::Success,
                            Err(e) => LoginEvent::Error {
                                message: format!("登录成功但写入凭证失败: {e}"),
                            },
                        },
                    );
                }
                TokenPoll::Pending => {}
                TokenPoll::SlowDown => interval = next_interval(interval, &TokenPoll::SlowDown),
                TokenPoll::Expired => outcome = Some(LoginEvent::Expired),
                TokenPoll::Denied => outcome = Some(LoginEvent::Denied),
                TokenPoll::Failed(msg) => outcome = Some(LoginEvent::Error { message: msg }),
            },
            Err(e) => {
                transport_failures += 1;
                if transport_failures >= MAX_TRANSPORT_FAILURES {
                    outcome = Some(LoginEvent::Error {
                        message: format!("网络错误: {e}"),
                    });
                    break;
                }
                // 偶发传输失败：按当前间隔下轮重试
            }
        }
        if outcome.is_some() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval.max(1))) => {}
            cancelled = cancel_rx.changed() => {
                // 显式取消（send true）或会话被替换（sender 落 drop）
                if cancelled.is_err() || *cancel_rx.borrow() {
                    outcome = Some(LoginEvent::Cancelled);
                }
            }
        }
    }

    let event = outcome.unwrap_or(LoginEvent::Cancelled);
    // 会话终结：从状态移除（cancel_login / open_auth_url 随之失效）
    {
        let st = app.state::<LoginState>();
        take_session(&st, &device_code);
    }
    if let Err(e) = app.emit("login-update", &event) {
        eprintln!("[quotax] login-update emit 失败: {e}");
    }
    // 登录成功 → 立即触发一次抓取，不等下一轮轮询
    if matches!(event, LoginEvent::Success) {
        app.state::<crate::RefreshSignal>().0.notify_one();
    }
}

// ============ 工具 ============

/// 用系统默认浏览器打开 URL（不引入 opener 依赖，cfg 分支保证平台无关）。
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    if let Err(e) = cmd.spawn() {
        eprintln!("[quotax] 打开浏览器失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_device_auth_response() {
        let body = r#"{
            "device_code": "JctDBN8i2uzW8oLJOvSvtTWps0TAQGK77huTQNe_",
            "user_code": "7BZD-DZYB",
            "verification_uri": "https://www.kimi.com/code/authorize_device",
            "verification_uri_complete": "https://www.kimi.com/code/authorize_device?user_code=7BZD-DZYB",
            "expires_in": 1800,
            "interval": 5
        }"#;
        let da: DeviceAuthResponse = serde_json::from_str(body).expect("应解析成功");
        assert_eq!(da.device_code, "JctDBN8i2uzW8oLJOvSvtTWps0TAQGK77huTQNe_");
        assert_eq!(da.user_code, "7BZD-DZYB");
        assert_eq!(da.expires_in, 1800);
        assert_eq!(da.interval, 5);
        assert!(da
            .verification_uri_complete
            .as_deref()
            .unwrap()
            .contains("user_code=7BZD-DZYB"));
    }

    #[test]
    fn device_auth_defaults() {
        // 字段缺省回退：expires_in=1800、interval=5、complete 链接缺失
        let da: DeviceAuthResponse = serde_json::from_str(
            r#"{"device_code":"d","user_code":"u","verification_uri":"https://x"}"#,
        )
        .expect("应解析成功");
        assert_eq!(da.expires_in, 1800);
        assert_eq!(da.interval, 5);
        assert!(da.verification_uri_complete.is_none());
    }

    #[test]
    fn classify_token_responses() {
        use TokenPoll::*;
        assert!(matches!(
            classify_token_response(400, r#"{"error":"authorization_pending"}"#),
            Pending
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"slow_down"}"#),
            SlowDown
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"expired_token"}"#),
            Expired
        ));
        assert!(matches!(
            classify_token_response(400, r#"{"error":"access_denied"}"#),
            Denied
        ));
        // 成功响应与 refresh 同构
        assert!(matches!(
            classify_token_response(
                200,
                r#"{"access_token":"a","refresh_token":"r","expires_in":900,"token_type":"Bearer","scope":"kimi-code"}"#
            ),
            Success(_)
        ));
        // 非标准错误码 / 非 JSON 响应
        assert!(matches!(
            classify_token_response(400, r#"{"error":"invalid_grant"}"#),
            Failed(_)
        ));
        assert!(matches!(
            classify_token_response(500, "internal error"),
            Failed(_)
        ));
    }

    #[test]
    fn interval_adjustment() {
        assert_eq!(next_interval(5, &TokenPoll::Pending), 5);
        assert_eq!(next_interval(5, &TokenPoll::SlowDown), 10);
        assert_eq!(next_interval(10, &TokenPoll::SlowDown), 15);
    }
}
