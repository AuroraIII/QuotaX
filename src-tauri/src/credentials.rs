//! 凭证读取、OAuth refresh 与原子写回（与 Kimi Code CLI 共用凭证文件）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 与 CLI 二进制一致的 OAuth client_id（逆向确认）。
pub const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
pub const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
pub const TOKEN_PATH: &str = "/api/oauth/token";

/// 提前刷新余量（秒）：到期前 60s 即视为需要刷新。
pub const EXPIRY_MARGIN_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".into()
}

pub fn oauth_host() -> String {
    std::env::var("KIMI_CODE_OAUTH_HOST")
        .or_else(|_| std::env::var("KIMI_OAUTH_HOST"))
        .unwrap_or_else(|_| DEFAULT_OAUTH_HOST.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// 凭证文件路径：KIMI_CODE_HOME 可覆盖 ~/.kimi-code（与 CLI 的 defaultKimiHome 一致）。
pub fn credentials_file() -> PathBuf {
    let base = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().to_path_buf())
                .expect("cannot resolve home dir")
                .join(".kimi-code")
        });
    base.join("credentials").join("kimi-code.json")
}

#[derive(Debug)]
pub enum CredError {
    NotFound(PathBuf),
    Io(std::io::Error),
    Parse(String),
    Refresh { status: u16, body: String },
    Transport(String),
}

impl std::fmt::Display for CredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredError::NotFound(p) => write!(
                f,
                "未检测到 Kimi Code 登录（缺少 {}），请先在 CLI 中执行 /login",
                p.display()
            ),
            CredError::Io(e) => write!(f, "凭证文件 IO 失败: {e}"),
            CredError::Parse(e) => write!(f, "解析凭证失败: {e}"),
            CredError::Refresh { status, body } => {
                write!(f, "刷新 token 失败 (HTTP {status}): {body}")
            }
            CredError::Transport(e) => write!(f, "网络错误: {e}"),
        }
    }
}

pub fn read() -> Result<Credentials, CredError> {
    let path = credentials_file();
    if !path.exists() {
        return Err(CredError::NotFound(path));
    }
    let text = std::fs::read_to_string(&path).map_err(CredError::Io)?;
    serde_json::from_str(&text).map_err(|e| CredError::Parse(e.to_string()))
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn is_stale(creds: &Credentials) -> bool {
    creds.expires_at - now_secs() < EXPIRY_MARGIN_SECS
}

/// 调用 OAuth refresh 端点（POST {oauth_host}/api/oauth/token，form 编码）。
pub async fn refresh(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<Credentials, CredError> {
    let url = format!("{}{}", oauth_host(), TOKEN_PATH);
    let resp = client
        .post(&url)
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| CredError::Transport(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| CredError::Transport(e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(CredError::Refresh {
            status,
            body: body.chars().take(300).collect(),
        });
    }
    parse_token_response(&body, Some(refresh_token))
}

/// 解析 token 端点成功响应（refresh 与设备码流程共用，结构完全一致）。
/// `prev_refresh_token`：响应缺 refresh_token 字段时的兜底（refresh 场景传旧值）；
/// `expires_at` 缺省时按 now + expires_in 本地补齐。
pub fn parse_token_response(
    body: &str,
    prev_refresh_token: Option<&str>,
) -> Result<Credentials, CredError> {
    // 兼容 expires_at / expires_in 两种字段
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<i64>,
        #[serde(default)]
        expires_at: Option<i64>,
        #[serde(default)]
        scope: Option<String>,
    }
    let r: TokenResp =
        serde_json::from_str(body).map_err(|e| CredError::Parse(e.to_string()))?;
    let expires_in = r.expires_in.unwrap_or(900);
    Ok(Credentials {
        access_token: r.access_token,
        refresh_token: r
            .refresh_token
            .or_else(|| prev_refresh_token.map(str::to_string))
            .unwrap_or_default(),
        expires_at: r.expires_at.unwrap_or(now_secs() + expires_in),
        expires_in,
        token_type: "Bearer".into(),
        scope: r.scope.or(Some("kimi-code".into())),
    })
}

/// tmp → rename 原子写回（与 CLI 行为一致），避免与 CLI 并发写互相损坏。
pub fn write_atomic(path: &Path, creds: &Credentials) -> Result<(), CredError> {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(creds)
        .map_err(|e| CredError::Parse(e.to_string()))?;
    std::fs::write(&tmp, body).map_err(CredError::Io)?;
    std::fs::rename(&tmp, path).map_err(CredError::Io)?;
    Ok(())
}
