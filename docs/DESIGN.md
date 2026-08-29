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
- 若 refresh_token 旋转导致 401，重新读文件重试一次（缓解与 CLI 并发刷新的竞争）；
- **内置设备码登录为兜底授权路径**（RFC 8628，2026-08-29 实施，见 `src-tauri/src/auth.rs` 与附录 B.8）：复用优先，凭证可用时绝不出现登录 UI；仅凭证文件缺失或 refresh 被服务端拒绝（HTTP 400/401）时进入「需要登录」状态，卡片展示登录视图（用户码 + 授权页 + 倒计时），成功后凭证按 CLI 格式原子写入同一文件。

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
│  usage.rs    GET /usages → 强类型结构        │
│  credentials.rs 读凭证 / 到期刷新 / 原子写回 │
└─────────────────────────────────────────────┘
                  │ HTTPS
        api.kimi.com /coding/v1/usages
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

## 附录 B：实施核查与改进清单

> 核查时间：2026-08-28 · 对照本文档逐项核对实际代码（`src-tauri/src/*`、`ui/*`、`tauri.conf.json`），`cargo check` 通过。

### B.1 核查结论

设计主体已全部落地：凭证复用/刷新/原子写回、`/usages` 宽松解析与字段映射、60s 轮询 + 退避 + 401 重试、横条/卡片 UI 与阈值变色、托盘菜单、位置持久化与多显示器回退均与设计一致。首轮核查（2026-08-28）发现的差距项已于 2026-08-29 全部完成（见 B.2/B.4）。

### B.2 差距与改进项（按优先级）

| 优先级 | 项目 | 设计出处 | 现状 |
| --- | --- | --- | --- |
| P0 | 托盘图标在用量 ≥ 80% 时叠加橙色提示点 | 4.3 | ✅ 已完成（2026-08-29）：`icons/icon-warn.ico`（白圈橙点角标，16/24/32/48/64/256 六尺寸），poller 每次成功抓取后计算 `max(used/limit)`，≥ 80% 经 `tray_by_id("quotax-tray")` 切换，回落恢复默认；仅状态翻转时切换 |
| P1 | 轮询间隔可配置（默认 60s，范围 30s–10min） | 6 | ✅ 已完成（2026-08-29）：`Settings.poll_interval_secs`（默认 60，钳制 30–600），poller 每轮读取最新值，免重启生效；退避逻辑不变 |
| P1 | 「失焦自动收起」提供配置开关 | 4.2 | ✅ 已完成（2026-08-29）：`Settings.collapse_on_blur`（默认 true）经 `get_settings` 暴露，前端 `blur` 监听按配置决定是否收起 |
| P2 | 展开卡片内容超出处理 | 4.2 | ✅ 已完成（2026-08-29）：`#rows` 设 `max-height: 144px` + `overflow-y: auto`（4px 细滚动条），`limits[]` 行数多时行区滚动，不再被窗口底部裁剪 |
| P2 | 文档勘误：第 5 节架构图接口路径 | 5 | 已修正为 `/usages`（首轮核查时修订） |

### B.3 改进实施约定

- 托盘橙点：Rust 侧在 `usage-update` 载荷计算 `max(used/limit)`，≥ 80% 时 `TrayIcon::set_icon` 切换为带橙点的预生成图标（`src-tauri/icons/` 增加 `icon-warn.ico`），恢复 < 80% 时切回默认；
- 新增配置项统一落入 `settings.json`（`poll_interval_secs`、`collapse_on_blur`），poller 每轮读取最新值，无需重启生效；
- 改动后同步更新本文档对应章节与 README。

### B.4 改进实施记录（2026-08-29）

- **托盘超量提醒（P0）**：`icon-warn.ico` 由原始 `icon.ico` 各尺寸条目叠加右下角角标（白色描边 + `#fb923c` 橙点）生成；Rust 侧经 `ico` crate 在启动时解码（取 ≤48px 最大尺寸适配托盘 DPI），`include_bytes!` 内嵌。poller 仅在 `warn` 状态翻转时调用 `set_icon`，避免每轮重建原生图标；抓取失败时保持现有图标（与 UI 展示旧数据的语义一致）。
- **轮询间隔可配置（P1）**：`Settings` 新增 `poll_interval_secs`，`poll_interval_secs_clamped()` 统一钳制（30–600）；poller 循环内每轮 `load_settings()` 读取最新值，`POLL_INTERVAL` 常量移除。
- **失焦收起开关（P1）**：`Settings` 新增 `collapse_on_blur`，随 `get_settings` 命令返回；`ui/app.js` 初始化时读入，`blur` 事件按配置执行。
- **卡片溢出（P2）**：`ui/style.css` 的 `#rows` 限高 144px（按窗口 340px 高度、含错误行与 Extra Usage 行的最坏情况预算）并允许纵向滚动。
- 验证：`cargo check` 通过；`cargo test`（`warn_icon_decodes`、`max_used_pct_across_rows`）通过。

### B.5 改进实施记录（2026-08-29 · 第二批）

- **透明窗口禁用 backdrop-filter（P2）**：WebView2 透明窗口下 `backdrop-filter` 按元素矩形包围盒渲染、无视 `border-radius`，导致横条/卡片四角出现方形硬角阴影。修复：删除 `.bar` / `.card` 的 `backdrop-filter: blur(...)` 声明（`tauri.conf.json` 的 `"shadow": false` 保持不变）；补偿性提高背景不透明度保证可读性（`--bg: rgba(18,18,26,0.92)`、`--bg-card: rgba(24,24,34,0.96)`）；软化阴影（bar: `0 6px 20px rgba(0,0,0,0.3)`，hover `0 8px 24px rgba(0,0,0,0.38)`；card: `0 10px 32px rgba(0,0,0,0.35)`）。结论：透明窗口下毛玻璃效果与圆角不可兼得，改用高不透明度深色底 + 柔和 box-shadow。
- **托盘重复创建根因（P1）**：`tauri.conf.json` 的 `"app.trayIcon"` 配置块会让 Tauri 自动创建一个**无菜单**的托盘图标，与 `main.rs` 中 `TrayIconBuilder::with_id("quotax-tray")` 创建的带菜单图标并存，表现为托盘出现两个图标。修复：删除配置中的 `trayIcon` 块，托盘完全由代码创建（菜单、左键聚焦、超量角标均挂在代码创建的实例上）。
- **凭证写回重试机制（P1）**：kimi CLI 运行时持有凭证文件句柄，poller 刷新 token 后 `write_atomic` 的 rename 可能被 Windows 拒绝（os error 5），原实现只记日志即丢弃，新 refresh_token 未持久化（旋转后的旧 token 仅剩重用宽限期，存在失效风险）。修复：poller 主循环维护 `pending_write: Option<Credentials>`；`refresh_guarded` 写回失败时暂存新凭证；每轮 fetch 前经 `retry_pending_write` 重试——文件 token 与 pending 一致视为已写入、文件 `expires_at` 更新说明 CLI 已自行刷新（丢弃 pending）、否则重试原子写回；日志仅在状态变化时打印（首次失败、最终成功/被取代），避免每轮刷屏。

### B.6 改进实施记录（2026-08-29 · 第三批）

- **阴影方形角根因修正与修复（P0）**：上一批（B.5）将方形角归因于 `backdrop-filter` 只命中了部分问题，残留阴影实为 **CSS box-shadow 被窗口硬边界矩形裁剪**——横条紧贴窗口左上角 (0,0)、卡片 300px 宽贴近 320px 窗口右缘/底部，阴影 blur 区越出窗口即被直边裁断（与 DWM 无关：`shadow: false` 经 tao `with_undecorated_shadow(false)` 已生效）。修复：窗口加大至 **372×372**（`tauri.conf.json` + `main.rs` 的 `WINDOW_W/H` 常量同步），`html,body` 增加 **36px 透明边距**容纳阴影（最大 blur 32px + 余量），可视内容尺寸不变。验证：实机截屏确认四角圆角干净、阴影柔和无裁切。
- 经验记录：调试截图时 GDI `CopyFromScreen` / `BitBlt`（含 CAPTUREBLT）均抓不到 WebView2 DirectComposition 合成的透明窗口内容，需以用户实机截图或 Windows.Graphics.Capture 为准。

### B.7 改进实施记录（2026-08-29 · 第四批）

- **内置 OAuth 登录提示词（P1，已于 B.8 实施）**：逆向 kimi CLI 0.36.0 确认其支持 OAuth Device Flow（RFC 8628），并对线上端点实测：`POST {oauth_host}/api/oauth/device_authorization`（form 仅 `client_id`）→ 200（`device_code`/`user_code`/`verification_uri_complete`/`expires_in=1800`/`interval=5`）；`POST /api/oauth/token` + `grant_type=urn:ietf:params:oauth:grant-type:device_code` 未授权时返回标准 `authorization_pending`，成功响应结构与 refresh 完全一致。无需本地回调服务器，可作为「凭证文件缺失/refresh_token 失效」时的应用内兜底授权路径。完整实现方案与验收标准见 `docs/PROMPT-builtin-oauth-login.md`。
- **macOS 适配可行性核查（P2）**：通读四个源码文件——全部为纯 Tauri 抽象层调用，无 Win32 直调、无 `cfg(target_os)` 平台分支（`windows_subsystem` 属性在非 Windows 为空操作）；依赖（tauri 2 / reqwest-rustls / tokio / serde / directories / ico）均官方支持 macOS；`src-tauri/icons/icon.icns` 已存在。本机 `cargo check --target aarch64-apple-darwin` 因 `ring`（rustls 依赖）的 C/汇编编译需要 macOS 交叉工具链而失败——属本机工具链限制，非代码问题；macOS 构建必须在 Mac 或 CI 完成。待真机确认项：托盘彩色图标在菜单栏深浅色下的观感（非 template image）、`skipTaskbar` 在 macOS 的 Dock 表现、透明无边框窗口的阴影观感（WebView2 特有的 B.5/B.6 问题在 WKWebView 不存在）。
- **打包落地（P1）**：`tauri.conf.json` `bundle.active=true`、`targets=["nsis","dmg"]`、图标列表补齐（png×3 + icns + ico）、`category=Utility`；新增 `.github/workflows/release.yml`（matrix：windows-latest → NSIS exe，macos-latest → DMG/aarch64；`workflow_dispatch` 与 `v*` tag 触发；artifacts 上传）。Windows 包本机 `npx tauri build` 产出；macOS 包仅 CI 可构建。

### B.8 改进实施记录（2026-08-29 · 第五批）

- **内置 OAuth 设备码登录落地（P1）**：按 `docs/PROMPT-builtin-oauth-login.md` 方案实施，复用优先原则不变——凭证可用时绝不出现登录 UI；仅凭证文件缺失（`CredError::NotFound`）或 refresh 被服务端拒绝（HTTP 400/401，`invalid_grant`）时 `usage-update` 载荷置 `needs_login: true`（新增字段，前端向后兼容），网络错误/5xx 照旧走 ⚠ 退避。
  - 新增 `src-tauri/src/auth.rs`：`start_login`（申请设备码 → 存唯一会话 → 后台 tokio 任务轮询 token → 成功按 CLI 格式 `write_atomic` 写入共用凭证文件 → emit `login-update success` → 经 `RefreshSignal` 立即抓取一次，不等下一轮 60s）、`cancel_login`、`open_auth_url`；轮询按服务端 `interval` 循环，`slow_down` 间隔 +5s（RFC 8628），`expired_token` / `access_denied` / 连续 3 次传输失败 / 超过 `expires_in` 终止并 emit 对应状态；`watch` channel 取消，会话单实例（重入先取消旧会话，按 `device_code` 防误删）。打开授权页由 `open_url` 按 `cfg` 分派（cmd `start` / `open` / `xdg-open`），无新依赖。
  - `credentials.rs` 提取 `parse_token_response` / `now_secs` 供 refresh 与设备码流程共用（行为不变）；`poller.rs` 失败类型改为 `FetchFailure { message, needs_login }`，refresh 链路保留「先读文件复用 CLI 结果 / 旧 token 兜底」逻辑，仅在最终被拒时携带 needs_login。
  - 前端 `ui/`：卡片新增 `#login-view`（入口按钮 / 8 位用户码 + 授权链接 + 倒计时 + 状态行），`needs_login` 时替代原「请先 /login」纯文本；登录会话进行中抑制失焦自动收起（用户需在浏览器授权页对照用户码）；托盘新增「登录账号」项（默认禁用），poller 按 `needs_login` 翻转 enabled，点击聚焦主窗。
  - 验证：`cargo check` / `cargo test` 通过（8 个测试，新增设备码响应解析、token 轮询错误分类、interval 调整、needs_login 判定等 6 个）；`tauri dev` 双路径实测——有凭证时正常出数（`usage ok`，无登录 UI、写回重试机制不受影响）；`KIMI_CODE_HOME` 指向空目录模拟凭证缺失，「需要登录」状态稳定进入（未检测到登录 → needs_login → 托盘项翻转执行无异常）。真实授权闭环（浏览器输入用户码换 token）与 CLI 共存场景（手测 B/C/D）待用户真机确认。

### B.9 改进实施记录（2026-08-29 · 第六批）

- **拖拽无法到达屏幕顶修复（P1）**：根因有两层——① `data-tauri-drag-region` 走 Windows 系统移动循环（HTCAPTION），系统将窗口钳制在显示器边界内，横条贴屏幕顶需要窗口 y=-36（36px 透明边距出屏）被拦；② 位置恢复判定要求窗口完全在屏内，贴顶位置（y<0）重启后回退默认右下角。修复：改用**自定义拖拽**（[ui/app.js](../ui/app.js)：`pointerdown` 记录 `outerPosition()` 起点 → `pointermove` 按 devicePixelRatio 换算 delta → rAF 合并 `setPosition`，pointer capture 保证拖出元素仍收事件，无系统钳制，全平台一致）；恢复判定放宽为**窗口与任一显示器有交集即可**（完全离开所有显示器才回退，[main.rs](../src-tauri/src/main.rs)）；capabilities 新增 `core:window:allow-outer-position` / `core:window:allow-set-position`。验证：Win32 `SetWindowPos` 将窗口置于 y=-55 → `Moved` 事件保存 → 重启后精确恢复 (1514, -55)，证明 set_position 支持负坐标且交集判定放行（物理拖拽手感需真机确认；沙箱环境无法注入鼠标输入驱动系统移动循环）。
- **拖拽抖动修复（P1，紧随上项）**：自定义拖拽首版用 `clientX/clientY` 计算位移——但 client 坐标相对**窗口视口**，窗口每移动一步，光标的 clientX 就反向变化一步，`nx = wx + (clientX - sx)*dpr` 形成正反馈回路（窗口前跳 → clientX 回缩 → 命令位置回拉 → 再前跳……），表现为拖动时疯狂抖动。修复：改用 **`screenX/screenY`（屏幕坐标）** 计算位移——屏幕坐标与窗口自身位置无关，彻底消除反馈回路；屏幕逻辑像素差 × devicePixelRatio = 物理像素位移，与 `outerPosition()`（物理像素）同量纲。多显示器混合 DPI 场景下 dpr 随窗口所在屏切换，可能存在亚像素级累计误差，对小组件可接受。
- **应用图标替换**：新主图标（用户提供的 1254×1254 QuotaX-icon.png）经 `npx tauri icon` 全套重生成（icon.ico/icns/png、Square*/StoreLogo、android/ios mipmaps）；`icon-warn.ico` 基于新主图标重制，并新增可复现生成工具 [examples/gen_icon_warn.rs](../src-tauri/examples/gen_icon_warn.rs)（纯 ico crate 实现：box-filter 平均池化缩放 + 1px 抗锯齿橙点角标，`cargo run --example gen_icon_warn` 一键重生成，替代此前一次性脚本），`cargo test` 8 项全绿（含 `warn_icon_decodes` 解码新文件）。
