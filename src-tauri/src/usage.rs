//! GET {base}/usages 的强类型解析（结构以 CLI 0.36.0 实测为准，见 DESIGN.md 附录 A）。
//!
//! 服务端返回的数值为字符串（如 "100"），且 boosterWallet 字段在未开通时缺省，
//! 因此数值字段使用宽松反序列化（number|string），boosterWallet 手动提取。

use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.kimi.com/coding/v1";

pub fn base_url() -> String {
    std::env::var("KIMI_CODE_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

pub fn usage_url() -> String {
    format!("{}/usages", base_url())
}

/// 数值字段宽松反序列化：接受 number 或数字字符串，null/缺失归 0。
fn deser_num<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f as u64))
            .ok_or_else(|| D::Error::custom("invalid number")),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|_| D::Error::custom(format!("invalid numeric string: {s}"))),
        serde_json::Value::Null => Ok(0),
        _ => Err(D::Error::custom("expected number or numeric string")),
    }
}

#[derive(Debug, Deserialize)]
pub struct RawUsageEntry {
    #[serde(default, deserialize_with = "deser_num")]
    pub limit: u64,
    #[serde(default, deserialize_with = "deser_num")]
    pub used: u64,
    /// RFC3339，如 "2026-08-28T15:21:24.248076Z"
    #[serde(rename = "resetTime", default)]
    pub reset_time: String,
}

#[derive(Debug, Deserialize)]
pub struct RawWindow {
    #[serde(default)]
    pub duration: u64,
    /// "TIME_UNIT_MINUTE" | "TIME_UNIT_HOUR" | "TIME_UNIT_DAY" | "TIME_UNIT_WEEK"
    #[serde(rename = "timeUnit", default)]
    pub time_unit: String,
}

#[derive(Debug, Deserialize)]
pub struct RawLimitItem {
    #[serde(default)]
    pub window: Option<RawWindow>,
    pub detail: RawUsageEntry,
}

#[derive(Debug, Deserialize)]
pub struct RawUser {
    #[serde(default)]
    pub membership: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RawUsageResponse {
    #[serde(default)]
    pub user: Option<RawUser>,
    #[serde(default)]
    pub usage: Option<RawUsageEntry>,
    #[serde(default)]
    pub limits: Vec<RawLimitItem>,
    #[serde(default, rename = "parallel")]
    pub parallel: Option<RawParallel>,
    #[serde(default, rename = "boosterWallet")]
    pub booster_wallet: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RawParallel {
    #[serde(default, deserialize_with = "deser_num")]
    pub limit: u64,
}

// ============ 推送给前端的统一结构（与 DESIGN.md 第 4 节 UI 对齐） ============

#[derive(Debug, Clone, Serialize)]
pub struct UsageRow {
    /// 展示名，如 "周额度（7 天）" / "5 小时窗口"
    pub name: String,
    /// 横条短标签，如 "周" / "5h"
    pub short: String,
    /// 窗口时长（分钟），用于前端识别 5h 窗口；周额度 = 10080
    pub window_minutes: u64,
    pub used: u64,
    pub limit: u64,
    /// RFC3339 重置时间，前端本地每秒 tick 计算倒计时
    pub reset_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtraUsage {
    pub balance_cents: i64,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagePayload {
    pub summary: Option<UsageRow>,
    pub limits: Vec<UsageRow>,
    pub extra_usage: Option<ExtraUsage>,
    pub parallel_limit: Option<u64>,
    pub membership: Option<String>,
    /// 抓取时刻（epoch 秒）
    pub fetched_at: i64,
}

fn window_to_minutes(w: &Option<RawWindow>) -> u64 {
    match w {
        Some(w) => {
            let mult = match w.time_unit.as_str() {
                "TIME_UNIT_HOUR" => 60,
                "TIME_UNIT_DAY" => 60 * 24,
                "TIME_UNIT_WEEK" => 60 * 24 * 7,
                _ => 1, // TIME_UNIT_MINUTE 及未知值
            };
            w.duration * mult
        }
        // CLI 语义：顶层 usage 无 window 时按 1 周处理
        None => 60 * 24 * 7,
    }
}

fn minutes_to_display(mins: u64) -> (String, String) {
    match mins {
        m if m == 60 * 24 * 7 => ("周额度（7 天）".to_string(), "周".to_string()),
        m if m % (60 * 24) == 0 => (format!("{} 天窗口", m / (60 * 24)), format!("{}d", m / (60 * 24))),
        m if m % 60 == 0 => (format!("{} 小时窗口", m / 60), format!("{}h", m / 60)),
        m => (format!("{} 分钟窗口", m), format!("{}m", m)),
    }
}

fn row_from_entry(entry: &RawUsageEntry, window: &Option<RawWindow>) -> UsageRow {
    let mins = window_to_minutes(window);
    let (name, short) = minutes_to_display(mins);
    UsageRow {
        name,
        short,
        window_minutes: mins,
        used: entry.used,
        limit: entry.limit,
        reset_at: entry.reset_time.clone(),
    }
}

/// boosterWallet 兼容 snake_case / camelCase 两种键（未开通时字段缺省 → None）。
fn parse_booster(v: &Option<serde_json::Value>) -> Option<ExtraUsage> {
    let obj = v.as_ref()?.as_object()?;
    let pick = |keys: &[&str]| -> Option<i64> {
        for k in keys {
            if let Some(x) = obj.get(*k) {
                if let Some(n) = x.as_i64() {
                    return Some(n);
                }
                if let Some(s) = x.as_str() {
                    if let Ok(n) = s.parse::<i64>() {
                        return Some(n);
                    }
                }
            }
        }
        None
    };
    Some(ExtraUsage {
        balance_cents: pick(&["balance_cents", "balanceCents"])?,
        currency: obj
            .get("currency")
            .and_then(|c| c.as_str())
            .unwrap_or("CNY")
            .to_string(),
    })
}

pub fn to_payload(raw: &RawUsageResponse) -> UsagePayload {
    let summary = raw.usage.as_ref().map(|e| row_from_entry(e, &None));
    let limits = raw
        .limits
        .iter()
        .map(|item| row_from_entry(&item.detail, &item.window))
        .collect();
    UsagePayload {
        summary,
        limits,
        extra_usage: parse_booster(&raw.booster_wallet),
        parallel_limit: raw.parallel.as_ref().map(|p| p.limit),
        membership: raw.user.as_ref().and_then(|u| {
            u.membership
                .as_ref()
                .and_then(|m| m.get("level"))
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        }),
        fetched_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    }
}
