# QuotaX

Kimi Code（Kimi Coding Plan）额度监测 Windows 桌面小组件。设计与接口规格见 [docs/DESIGN.md](docs/DESIGN.md)。

## 运行（开发模式）

```powershell
npm install          # 安装 @tauri-apps/cli（本地依赖）
npx tauri dev        # 启动（首次编译较慢）
```

- 横条钉在桌面（置顶、可拖拽、位置记忆），悬停 200ms 展开详情卡片，移出 300ms 收起（窗口失焦自动收起，可在配置中关闭）；
- 托盘菜单：立即刷新 / 登录账号（需要登录时可用） / 置顶开关 / 退出；左键托盘图标聚焦小组件；任一限额用量 ≥ 80% 时托盘图标叠加橙色角标，回落后恢复；
- 数据自动刷新（间隔可配置，默认 60s），失败退避 1m→2m→5m，401 自动重读凭证/刷新 token。

## 配置

运行时配置位于 `%APPDATA%\QuotaX\config\settings.json`（随窗口位置等自动维护，可手动编辑）：

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `poll_interval_secs` | `60` | 轮询间隔（秒），钳制在 30–600，修改后下一轮生效（无需重启） |
| `collapse_on_blur` | `true` | 窗口失焦时自动收起展开卡片 |
| `always_on_top` | `true` | 窗口置顶（托盘菜单/卡片开关同步） |
| `x` / `y` | — | 窗口位置（拖动后自动记忆） |

## 打包（安装包）

```powershell
npx tauri build     # Windows：产出 NSIS 安装包
```

产物位于 `src-tauri/target/release/bundle/nsis/*.exe`。

macOS（DMG, Apple Silicon）**不能在 Windows 上交叉构建**（ring C 编译、签名、DMG 依赖 macOS SDK），由 CI 完成：`.github/workflows/release.yml`（手动 Run workflow 或推送 `v*` tag），同时产出 Windows 与 macOS 两个包。应用代码本身无平台专属 API，macOS 运行行为（托盘图标样式、Dock 表现）以真机为准。

## 数据来源

直接复用本机 Kimi Code CLI 的 OAuth 凭证（`%USERPROFILE%\.kimi-code\credentials\kimi-code.json`，支持 `KIMI_CODE_HOME` 覆盖），不产生文件损坏（tmp→rename 原子写回）。接口实测结论见 DESIGN.md 附录 A：

- 额度：`GET https://api.kimi.com/coding/v1/usages`（`KIMI_CODE_BASE_URL` 可覆盖）
- 刷新：`POST https://auth.kimi.com/api/oauth/token`（`KIMI_CODE_OAUTH_HOST` 可覆盖）

### 内置登录（兜底授权）

凭证可用时绝不打扰；仅当凭证文件缺失或 refresh token 失效（HTTP 400/401）时，展开卡片会出现登录视图——点击「使用 Kimi 账号登录」发起设备码流程（RFC 8628）：自动打开系统浏览器授权页，输入显示的用户码完成授权，成功后凭证按 CLI 格式原子写入同一文件（CLI 与 QuotaX 均可直接使用），卡片自动切回数据视图。网络错误/服务端 5xx 不进入登录态，仍按 ⚠ 退避重试。

## 注意事项

- 在 TRAE 沙箱终端中运行 dev 时，WebView2 数据目录默认落在 `%LOCALAPPDATA%` 会被拦截，需先设置：
  `$env:WEBVIEW2_USER_DATA_FOLDER = 'D:\Project\QuotaX\.webview2'`；普通终端无需此设置。
- 沙箱或 kimi CLI 持有文件句柄时，凭证写回可能被拒（os error 5）；应用会暂存新凭证并在每轮轮询前自动重试，直至写入成功或检测到 CLI 已自行刷新（读侧不受影响），普通环境不受限。
- 若 API 改版/凭证异常导致持续失败，横条显示 ⚠，悬停可见错误详情；凭证缺失/失效时可直接在卡片内重新登录，或仍在 CLI 中执行 `/login`。

## 结构

```
src-tauri/          Rust 后端（tauri 2 + reqwest + tokio）
  src/main.rs       窗口/托盘/命令/位置持久化
  src/auth.rs       内置 OAuth 设备码登录（needs_login 兜底授权）
  src/credentials.rs 凭证读取、OAuth 刷新、token 响应解析、原子写回
  src/usage.rs      /usages 强类型解析与前端字段映射
  src/poller.rs     轮询（间隔可配置）、退避、401 重试、needs_login 判定、事件推送、托盘超量提醒
  examples/gen_icon_warn.rs  超量提醒托盘图标（icon-warn.ico）生成工具
ui/                 原生 HTML/CSS/JS 前端（无框架）
preview/index.html  界面样式基准
docs/DESIGN.md      设计文档 + 接口探测记录（附录 A）+ 实施核查与改进记录（附录 B）
docs/PROMPT-builtin-oauth-login.md  内置 OAuth 登录（设备码流程）实现提示词
.github/workflows/release.yml  CI 打包（Windows NSIS + macOS DMG）
```
