# 任务：为 QuotaX 实现内置 OAuth 登录（设备码流程）

> 用途：本文件是一份自包含的实现提示词，可直接交给编码代理执行。执行前无需额外背景，所需接口结论均已实测写入下文。

## 0. 项目背景（执行者必读）

QuotaX 是 Kimi Code（Kimi Coding Plan）额度监测桌面小组件，Tauri 2（Rust 后端 + `ui/` 原生 HTML/CSS/JS 前端，无框架），当前仅面向 Windows。仓库根：`D:\Project\QuotaX`。

关键文件：

- `src-tauri/src/credentials.rs` — 凭证读取、OAuth refresh、tmp→rename 原子写回。常量：`CLIENT_ID = "17e5f671-d194-4dfb-9706-5516cb48c098"`、`DEFAULT_OAUTH_HOST = "https://auth.kimi.com"`、`TOKEN_PATH = "/api/oauth/token"`；`oauth_host()` 支持 `KIMI_CODE_OAUTH_HOST`/`KIMI_OAUTH_HOST` 覆盖；`credentials_file()` 定位 `~/.kimi-code/credentials/kimi-code.json`（`KIMI_CODE_HOME` 可覆盖）；`Credentials` 结构体与 `write_atomic()` 已存在，直接复用。
- `src-tauri/src/poller.rs` — 60s 轮询 `/usages`、失败退避、401 重读凭证重试、refresh 失败时写回重试（`pending_write`）。每轮重读凭证文件。
- `src-tauri/src/main.rs` — 窗口/托盘/命令（`refresh_now`、`set_always_on_top`、`get_settings`）/`Settings` 持久化。
- `src-tauri/src/usage.rs` — `/usages` 解析。
- `ui/index.html` / `ui/style.css` / `ui/app.js` — 横条 + 悬停展开卡片 UI，经 `usage-update` 事件渲染；错误态显示 ⚠ + 错误文本。

现状：授权**唯一**来源是复用本机 Kimi CLI 的凭证文件。文件缺失时 UI 显示「未检测到 Kimi Code 登录，请先在 CLI 中执行 /login」（`CredError::NotFound`），无任何应用内登录手段。

## 1. 目标行为

**复用优先，缺失/失效才登录**：

1. 启动与每轮轮询逻辑保持现状——优先读凭证文件、到期自动 refresh。凭证可用时**绝不**主动出现登录界面；
2. 仅以下两种情况进入「需要登录」状态：
   - 凭证文件缺失（`CredError::NotFound`）；
   - refresh 被服务端拒绝（HTTP 400/401，如 `invalid_grant`，说明 refresh_token 已被吊销或过期）。网络错误/5xx 维持现有 ⚠ 退避逻辑，不进入登录态；
3. 「需要登录」状态下，展开卡片显示登录视图：用户点「使用 Kimi 账号登录」→ 展示 8 位用户码 + 授权链接 + 「打开授权页」按钮（调系统浏览器）+ 有效期倒计时 + 等待状态；授权成功后自动回到数据视图；用户可取消；
4. 登录成功后把 token 按现有格式原子写入 `credentials_file()`（与 CLI 共用，CLI 也能直接用这份凭证）；poller 下一轮自动接管（它本来就每轮重读文件），无需重启；
5. 托盘菜单加「登录账号」项，仅在「需要登录」状态时可用（其余时间禁用或隐藏，取实现简单的）。

## 2. 接口规格（2026-08-29 对 kimi CLI 0.36.0 与线上端点实测，可直接信任）

**第一步：申请设备码**

```
POST {oauth_host}/api/oauth/device_authorization
Content-Type: application/x-www-form-urlencoded

client_id=17e5f671-d194-4dfb-9706-5516cb48c098
```

实测 200 响应：

```json
{
  "device_code": "JctDBN8i2uzW8oLJOvSvtTWps0TAQGK77huTQNe_",
  "user_code": "7BZD-DZYB",
  "verification_uri": "https://www.kimi.com/code/authorize_device",
  "verification_uri_complete": "https://www.kimi.com/code/authorize_device?user_code=7BZD-DZYB",
  "expires_in": 1800,
  "interval": 5
}
```

**第二步：轮询换 token**

```
POST {oauth_host}/api/oauth/token
Content-Type: application/x-www-form-urlencoded

client_id=17e5f671-...&grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code=<device_code>
```

- 未授权：`{"error":"authorization_pending","error_description":"Authorization is pending"}`（已实测）→ 按 `interval` 秒继续轮询；
- `slow_down` → 间隔 +5s（RFC 8628 标准行为）；
- `expired_token` / `access_denied` → 终止，UI 分别提示「已过期，请重新发起」/「已取消授权」；
- 成功 200：JSON 结构与现有 refresh 响应**完全一致**（`access_token`/`refresh_token`/`token_type`/`expires_in`/`scope`），`expires_at = now + expires_in` 本地补齐——复用 `credentials.rs` 里 refresh 响应的解析思路即可。

约束：单实例同时只允许一个登录会话；设备码有效期 1800s；轮询用 tokio 任务，可取消。

## 3. 实现方案（推荐，可微调但保持行为不变）

### 3.1 新增 `src-tauri/src/auth.rs`（约 150 行）

- `#[derive(Serialize)] pub struct LoginSession { user_code, verification_uri, verification_uri_complete, expires_in, interval }`
- `#[tauri::command] pub async fn start_login(app: AppHandle, state: State<'_, LoginState>) -> Result<LoginSession, String>`：POST device_authorization → 存 `device_code` + `CancellationToken`（或 `watch` channel）→ spawn tokio 轮询任务 → 立即返回 `LoginSession` 给前端展示；
- 轮询任务：按 interval 循环 POST token 端点 → 成功则构造 `Credentials` 并 `write_atomic` 到 `credentials_file()`，emit `login-update {status:"success"}`；`authorization_pending` 继续；`slow_down` 间隔+5s；`expired_token`/`access_denied`/ transport 连续失败 → emit 对应状态后退出；超过 `expires_in` 自行 emit `expired`；
- `#[tauri::command] pub fn cancel_login(state)`：触发取消，轮询任务 emit `login-update {status:"cancelled"}` 后退出；
- `LoginState`：`Mutex<Option<...>>` 管理唯一会话，`start_login` 重入时先取消旧会话再开新会话；
- 打开浏览器：不加新依赖，写个小函数 `open_url(url: &str)`：`#[cfg(target_os="windows")] Command::new("cmd").args(["/C","start","",url])`；`#[cfg(target_os="macos")] Command::new("open").arg(url)`。由 `start_login` 成功拿到链接后**后端直接调一次**（前端同时显示链接兜底）；
- 命令在 `main.rs` 的 `invoke_handler` 注册，`LoginState` 用 `app.manage()` 托管。

### 3.2 poller 接入「需要登录」状态

- `poller.rs` 中 refresh 返回 `CredError::Refresh{status: 400|401, ..}` 时，在 `usage-update` 载荷里置 `needs_login: true`（结构加一个 serde 默认字段，前端向后兼容）；`NotFound` 同样置 `needs_login: true`；其余错误照旧；
- 登录成功事件（`login-update success`）后，主窗体直接触发一次现有 `refresh_now` 等价路径，立刻出数据，不等下一轮 60s。

### 3.3 前端（`ui/`）

- `index.html`：卡片内加 `#login-view` 区块（默认隐藏）：用户码大号等宽显示、授权链接文本、「打开授权页」+「取消」按钮、倒计时、状态行；与现有深色卡片风格一致（`style.css` 追加，不改现有选择器）；
- `app.js`：`usage-update` 载荷 `needs_login === true` 时显示登录视图（替代原「请先 /login」纯文本）；`start_login` 返回后填充用户码/链接并开倒计时；监听 `login-update` 更新状态行（等待授权/成功/已过期/已取消/失败原因）；
- 「打开授权页」按钮：`invoke('open_auth_url')`（或直接复用 start_login 已打开的浏览器，按钮作为重开手段——实现任选一，别两个都做）。

### 3.4 托盘

- `main.rs` 托盘菜单加 `MenuItem`「登录账号」，默认禁用；poller 进入/离开 `needs_login` 时经 `AppHandle` 翻转 enabled（参照现有 `always_on_top` CheckMenuItem 的更新方式）。

## 4. 验收标准

1. `cargo check`、`cargo test` 全绿；新增纯函数（device_authorization 响应解析、token 轮询错误分类、interval 调整）配单元测试；
2. 手测 A（复用优先）：有效凭证存在时启动，全程无登录 UI，数据正常；
3. 手测 B（缺失登录）：把 `kimi-code.json` 临时改名 → 启动 → 卡片出现登录视图 → 走完真实授权（浏览器打开授权页输入用户码）→ 凭证文件生成、内容字段与 CLI 格式一致 → 卡片自动切回数据视图；
4. 手测 C（取消/过期）：发起登录后点取消 → 状态正确、无残留轮询任务；设备码过期提示正确；
5. 手测 D（与 CLI 共存）：登录写凭证后跑 `kimi` CLI 能正常用该凭证；CLI 后续自行 refresh 时 QuotaX 现有 401 重读逻辑不受影响；
6. 文档同步：README.md「数据来源」一节补充内置登录说明；`docs/DESIGN.md` 2.1 节标注「内置设备码登录为兜底授权路径」，附录 B 追加一条实施记录（日期、改动点、验证结果）。

## 5. 约束

- **最小改动**：不动现有凭证复用/refresh/原子写回/轮询退避逻辑的正确性；新功能全部增量添加；
- 不引入新第三方 Rust 依赖（reqwest/tokio/serde 已够用）；不引入前端框架/构建步骤；
- 代码风格与现状一致：中文注释、中文用户可见错误文案、错误经 `String`/`CredError` 上抛；
- 接口均为未公开端点：base/host/paths 一律走现有常量与 `oauth_host()`，禁止散落硬编码；
- 平台无关性：auth 链路不得使用 Windows-only API（`open_url` 已按 cfg 处理），为后续 macOS 移植留路；
- 凭证文件并发写风险沿用既有策略（原子写 + poller 每轮重读），不在本任务内扩展。
