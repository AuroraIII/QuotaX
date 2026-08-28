# QuotaX 设计文档

> Kimi Coding Plan 额度监测桌面小组件（Windows）
> 版本：v0 草案 · 2026-08-28

## 1. 项目概述

QuotaX 是一个常驻桌面的迷你小组件，用于实时监测 Kimi Code（Kimi Coding Plan）会员额度使用情况：

- 平时以一条**迷你横条**钉在桌面（置顶、半透明、可拖动），只显示最关键信息：周额度百分比 + 迷你进度条；
- **悬停/点击展开**为详情卡片，展示全部限额条目、重置倒计时、Extra Usage 余额；
- 后台每 60s 自动刷新数据，失败自动退避重试；
- 附带系统托盘图标（立即刷新 / 置顶开关 / 退出）。

监测对象（与 Kimi Code 官方文档一致）：

- **周额度**：每 7 天自动刷新，不结转；
- **5 小时滚动速率窗口**：短时间请求过多触发限流，窗口滚过自动恢复；
- **Extra Usage（加油包）**：订阅额度用完后按量付费的余额（若已开通）。

## 2. 数据来源

> ⚠️ 以下接口为 **Kimi Code CLI 内部使用的未公开接口**（从本机 CLI 二进制逆向确认），未来可能变更。代码中 base URL 与路径均为常量，便于修改。

### 2.1 凭证

复用本机 Kimi Code CLI 的 OAuth 凭证，无需用户二次登录：

```
%USERPROFILE%\.kimi-code\credentials\kimi-code.json
```

字段（已实测确认）：

```json
{
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": 1787919731,
  "expires_in": 900,
  "token_type": "Bearer",
  "scope": "kimi-code"
}
```

- access_token **15 分钟过期**，组件必须实现 refresh 流程；
- refresh：`POST {oauth_host}/api/oauth/token`（form 编码的标准 OAuth refresh_token grant；`oauth_host` 默认 `https://auth.kimi.com`，实测见附录 A）；
- 刷新成功后写回凭证文件，采用 **tmp → rename 原子写**（与 CLI 行为一致），避免与正在运行的 CLI 互相损坏文件；
- 若 refresh_token 旋转导致 401，重新读文件重试一次（缓解与 CLI 并发刷新的竞争）。

### 2.2 额度接口

```
GET {base}/usages
Authorization: Bearer <access_token>
```

- base：`https://api.kimi.com/coding/v1`（环境变量 `KIMI_CODE_BASE_URL` 可覆盖）。路径为 **`/usages`（复数）**，实测与 CLI 0.36.0 二进制逆向一致；原假设 `/oauth/usage` 已证伪（详见附录 A）。
- 真实响应结构与下文草案不同：顶层为 `usage` / `limits[]`（`window` + `detail`）/ `boosterWallet`，数值均为字符串；CLI 内部将其转换为 `{summary, limits, extra_usage}`。完整样例与字段含义见附录 A。

> 以下为设计期从二进制解析的草案结构（kap-server 本地路由的 wire format，与远端 `/usages` 不同，仅留作参考）：

```json
{
  "summary": { "name": "...", "window": "...", "used": 0, "limit": 0, "reset_at": "..." } | null,
  "limits":  [ { "name": "...", "window": "...", "used": 0, "limit": 0, "reset_at": "..." } ],
  "extra_usage": {
    "balance_cents": 0,
    "total_cents": 0,
    "monthly_charge_limit_enabled": false,
    "monthly_charge_limit_cents": 0,
    "monthly_used_cents": 0,
    "currency": "..."
  } | null
}
```

### 2.3 备选方案

若接口变动导致不可用，降级方案：定期执行 `kimi` CLI 相关命令/读取会话状态并解析输出。接口探测结论将记录在本文件附录。

## 3. 技术栈

| 层 | 选型 | 理由 |
| --- | --- | --- |
| 应用框架 | **Tauri 2**（Rust + WebView2） | 体积小（exe ~10MB）、常驻内存低（~20-40MB），适合常驻小组件；远轻于 Electron |
| 前端 | **原生 HTML/CSS/JS** | 无框架、无构建步骤，Tauri 直接托管静态文件，保持项目极简 |
| HTTP | `reqwest`（rustls）+ `tokio` | 异步轮询 |
| 序列化 | `serde` / `serde_json` | — |
| 路径 | `directories` | 定位 `%USERPROFILE%\.kimi-code` |

本机环境（已验证）：

- Rust 1.97.1（`x86_64-pc-windows-msvc`）✅
- VS 2022 Build Tools + MSVC 14.44 + Windows SDK 10.0.26100 ✅
- WebView2 Runtime 151 ✅
- Node 22 / npm 10（备用，可用于安装 `@tauri-apps/cli`）✅
- 待装：`tauri-cli`（脚手架时用 `npm i -g @tauri-apps/cli` 或 `cargo install tauri-cli`）

## 4. 界面设计

### 4.1 收起态（默认）

尺寸约 `210 × 42px`，无边框、圆角（12px）、半透明深色（~85% 不透明度）、置顶、可拖动。
**两行式**：同时显示「周额度」与「5 小时窗口」两个最关键的限额：

```
┌────────────────────────────────┐
│ Ⓚ  周  ▰▰▰▰▰▰▱▱▱▱  62%       │
│    5h  ▰▰▱▱▱▱▱▱▱▱  18%       │
└────────────────────────────────┘
```

- 左侧：Kimi "K" 标识点；
- 两行结构相同：短标签（`周` / `5h`）+ 迷你进度条 + 百分比；
- 每行独立阈值变色（与官方 Console 80% 提醒一致）：
  - `< 80%` 绿色 `#4ade80`
  - `≥ 80%` 橙色 `#fb923c`
  - `≥ 95%` 红色 `#f87171`
- 数据加载失败：进度条变灰 + `⚠` 图标，hover 显示错误详情。

### 4.2 展开态（悬停 200ms 展开，移出 300ms 收起）

自横条向下展开为约 `300 × 240px` 卡片，圆角 12px，与横条同风格：

```
┌──────────────────────────────────────┐
│ Kimi Code 额度              12:30 刷新 │
│ ──────────────────────────────────── │
│ 总额度（summary）      3 天 5 小时后重置│
│ ▰▰▰▰▰▰▱▱▱▱  58%        580 / 1000    │
│                                      │
│ 周额度（7 天）         3 天 5 小时后重置│
│ ▰▰▰▰▰▰▱▱▱▱  62%        620 / 1000    │
│                                      │
│ 5 小时窗口                  2h 14m 后恢复│
│ ▰▰▱▱▱▱▱▱▱▱  18%          18 / 100    │
│                                      │
│ Extra Usage 余额            ¥ 25.00   │
│ ──────────────────────────────────── │
│ ⟳ 立即刷新                  置顶 ☑     │
└──────────────────────────────────────┘
```

- **总额度行**：取自接口 `summary` 字段，为 null 时整行不显示（字段含义以接口实测为准）；
- 每条 `limits[]` 一行：名称、进度条、百分比 + `used / limit` 绝对数值、重置倒计时（由 `reset_at` 本地每秒 tick 计算）；
- Extra Usage 行：`balance_cents / 100` + `currency`，未开通则不显示；
- 底部工具行：手动刷新按钮、置顶开关；
- 窗口失焦自动收起（可在配置中关闭）。

### 4.3 系统托盘

托盘图标（用量超 80% 时图标叠加橙色点），右键菜单：

- 立即刷新
- 置顶 开/关
- 退出

### 4.4 窗口行为

- 拖动：按住横条空白处拖动，位置持久化到本地配置（下次启动恢复）；
- 多显示器：跟随保存的坐标，坐标失效（拔掉显示器）时回退到主屏右下角；
- 默认不出现在任务栏与 Alt-Tab。

## 5. 架构与模块

```
┌─────────────────────────────────────────────┐
│ WebView2 前端 (ui/index.html + css + js)     │
│  渲染横条/卡片 ← listen("usage-update")      │
└─────────────────▲───────────────────────────┘
                  │ Tauri Event / Command
┌─────────────────┴───────────────────────────┐
│ Rust 后端                                    │
│  poller.rs   tokio 定时器，60s 轮询，失败退避 │
│  usage.rs    GET /oauth/usage → 强类型结构   │
│  credentials.rs 读凭证 / 到期刷新 / 原子写回 │
└─────────────────────────────────────────────┘
                  │ HTTPS
        api.kimi.com /coding/v1/oauth/usage
```

计划目录结构：

```
QuotaX/
├── docs/DESIGN.md          # 本文档
├── preview/index.html      # 界面静态预览（mock 数据）
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json     # 无边框透明窗口、托盘、权限
│   └── src/
│       ├── main.rs         # 入口、窗口/托盘初始化
│       ├── credentials.rs
│       ├── usage.rs
│       └── poller.rs
├── ui/
│   ├── index.html
│   ├── style.css
│   └── app.js
└── README.md               # 构建/运行说明
```

## 6. 刷新与异常策略

- 轮询间隔默认 60s（可配置 30s–10min）；
- 失败（网络错误 / 5xx）：指数退避 1m → 2m → 5m，上限 5m，UI 显示 `⚠` + 上次成功数据（置灰标注时间）；
- 401：重新读凭证文件并重试一次（可能 CLI 刚刷新过 token），仍失败则执行 refresh 流程；
- 凭证文件不存在：UI 显示「未检测到 Kimi Code 登录，请先在 CLI 中 /login」。

## 7. 风险

| 风险 | 说明 | 缓解 |
| --- | --- | --- |
| 接口未公开 | `/oauth/usage` 可能随 CLI 版本变更 | base/路径定义为常量；接口探测结果写本文档附录；预留 CLI 降级方案 |
| 凭证并发写 | 与 CLI 同时刷新 token 可能互相覆盖 | 原子写；401 时重读文件重试 |
| 轮询频率 | 过于频繁可能触发速率限制 | 默认 60s，最短 30s；接口本身应远轻于模型请求 |
| WebView2 依赖 | 极少数系统未装 | 安装包阶段再处理（Tauri 支持嵌入引导） |

## 8. 范围声明

- 本期仅监测 **Kimi Coding Plan** 额度；暂不支持其他平台（OpenRouter 等）；
- 本期交付：`dev` 模式可运行 + 设计文档 + 界面预览；安装包打包（`cargo tauri build`）后续再做。

## 附录 A：接口探测记录

> 实测时间：2026-08-28 14:21–14:52 UTC · kimi CLI 0.36.0 · 本机真实凭证

### A.1 额度端点结论

| 候选 URL | 结果 |
| --- | --- |
| `GET https://api.kimi.com/coding/v1/oauth/usage` | **404** ❌（设计期假设证伪） |
| `GET https://auth.kimi.com/oauth/usage`（及 /coding/v1、/v1 等变体） | **404** ❌ |
| `GET https://api.kimi.com/coding/v1/usages` | **200** ✅ **可用端点** |
| `GET https://api.kimi.com/coding/v1/me`（用户信息，备用） | 200 ✅ |

- 真实路径为 **`/usages`（复数）**。来源：对 `kimi.exe` 0.36.0 二进制做字符串提取，`kimiCodeUsageUrl() = ${KIMI_CODE_BASE_URL ?? "https://api.kimi.com/coding/v1"}/usages`，请求头仅需 `Authorization: Bearer` + `Accept: application/json`，CLI 侧超时 8s。
- `/oauth/usage` 是 CLI **内嵌 kap-server 的本地路由**（wire format 为 `{summary, limits, extra_usage}`），并非远端服务，故直连 404。
- 401 实测：过期 access_token 请求 `/usages` 返回 401，可作为触发重读凭证/刷新的信号。

### A.2 响应样例（结构真实，取值已脱敏）

```json
{
  "user": {
    "userId": "cpog_xxxxxxxxxxxxxxxxxxxx",
    "region": "REGION_CN",
    "membership": { "level": "LEVEL_BASIC" },
    "businessId": ""
  },
  "usage": {
    "limit": "100", "used": "42", "remaining": "58",
    "resetTime": "2026-01-01T08:00:00.000000Z"
  },
  "limits": [
    {
      "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
      "detail": {
        "limit": "100", "used": "7", "remaining": "93",
        "resetTime": "2026-01-01T07:00:00.000000Z"
      }
    }
  ],
  "parallel": { "limit": "10" },
  "totalQuota": {},
  "authentication": { "method": "METHOD_ACCESS_TOKEN", "scope": "FEATURE_CODING" },
  "subType": "TYPE_PURCHASE",
  "domain": "DOMAIN_NEXUS"
}
```

### A.3 字段含义（对照 CLI 0.36.0 解析逻辑 `managed-usage.ts`）

| 字段 | 含义 | QuotaX 映射 |
| --- | --- | --- |
| `usage` | 主限额。CLI 语义：无 `window` 字段时按 **1 周窗口**处理（周额度） | `summary` 行；横条第一行「周」 |
| `limits[].window` | `duration × timeUnit`（`TIME_UNIT_MINUTE/HOUR/DAY/WEEK`）；样例 `300 × MINUTE` = **5 小时滚动窗口** | 横条第二行「5h」；卡片逐行展示 |
| `limits[].detail` | 该窗口的 used/limit/remaining/resetTime | 卡片行数值与倒计时 |
| `resetTime` | RFC3339 带微秒，UTC；倒计时基准 | `reset_at`（前端每秒 tick） |
| `boosterWallet` | Extra Usage 加油包余额；**未开通时字段整体缺省**（非 null）。CLI 按 `balanceCents/currency` 解析 | `extra_usage` 行，缺省不显示 |
| 数值类型 | 服务端返回 **JSON 字符串**（"100"） | 解析时 number/string 兼容 |
| `parallel.limit` | 并发请求上限 | v0 暂不展示 |
| `user.membership.level` / `subType` | 会员等级 / 账号子类型（TYPE_PURCHASE=按量购买型） | v0 暂不展示 |
| `totalQuota` / `authentication` / `domain` | 其余元数据 | 未使用 |

注：样例账号为 `TYPE_PURCHASE`（按量购买型），其主限额 `resetTime` 距抓取时刻约 1–3 小时，说明该类型主窗口并非 7 天；订阅型账号预期为周窗口。UI 以响应为准动态展示，不硬编码窗口语义。

### A.4 刷新端点实测

- 端点：`POST https://auth.kimi.com/api/oauth/token`（`Content-Type: application/x-www-form-urlencoded`）
- 参数：`client_id=17e5f671-d194-4dfb-9706-5516cb48c098`、`grant_type=refresh_token`、`refresh_token=<凭证文件中的 refresh_token>`
- 响应 **200**：

```json
{
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "token_type": "Bearer",
  "expires_in": 900,
  "scope": "kimi-code"
}
```

- **refresh_token 每次旋转**（响应返回新值）；响应**无 `expires_at`**，需本地 `now + expires_in` 计算后写回凭证文件；
- 同一旧 refresh_token 在实测窗口内（约 6 分钟）**可重复使用**（服务端存在宽限），对「组件与 CLI 并发刷新」是利好；仍按 2.1 节策略做 401 重读 + 原子写回，不依赖宽限行为；
- 刷新获得的新 access_token 请求 `/usages` → 200 ✅。

### A.5 对 v0 实现的影响

1. `usage.rs` 按 A.2 结构强类型解析（数值兼容 number/string，`boosterWallet` 缺省容错），并按 A.3 映射为前端统一结构 `{summary, limits[], extra_usage}`；
2. `credentials.rs` 刷新端点使用 `/api/oauth/token`（`KIMI_CODE_OAUTH_HOST` 可覆盖），写回时补齐 `expires_at`；
3. 本机开发沙箱曾拦截对凭证目录的写回（详见实施记录）；正常用户环境无此限制。

### A.6 并发共用凭证实测（QuotaX dev 与运行中的 kimi CLI 并存）

- dev 运行期间 kimi CLI（PID 264860）同时在跑；QuotaX 每轮重读凭证文件，某轮观测到凭证文件被 CLI 自行刷新（`expires_at` 更新为未来时间），QuotaX 随后直接复用新 token，全程无冲突；
- 手动探测阶段的多次 refresh 复用同一旧 refresh_token 均返回 200（见 A.4），未触发 token 族失效。
