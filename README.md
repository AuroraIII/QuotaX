# QuotaX

> Kimi Code（Kimi Coding Plan）额度监测桌面小组件

![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-blue)
![Tauri](https://img.shields.io/badge/Tauri-2-orange)
![Rust](https://img.shields.io/badge/Rust-stable-dea584)

一个常驻桌面的迷你横条：平时只占屏幕一角，悬停即展开详情卡片，实时显示 Kimi Code 的周额度与 5 小时窗口用量。自动复用本机 Kimi Code CLI 的登录凭证，装好即用，无需重复登录。

## ✨ 特性

- **桌面常驻**：无边框半透明横条，置顶显示、位置记忆，可自由拖拽到屏幕任意位置（包括贴顶）
- **悬停展开**：悬停 200ms 展开详情卡片，移出 300ms 自动收起
- **双指标监测**：周额度 + 5 小时窗口用量，进度条 + 剩余时间倒计时
- **自动刷新**：默认 60s 轮询（可配置 30–600s），失败自动退避重试，无需人工干预
- **托盘集成**：立即刷新 / 登录账号 / 置顶开关 / 退出；用量 ≥ 80% 时托盘图标叠加橙色角标提醒
- **凭证复用**：直接读取 Kimi Code CLI 的 OAuth 凭证，与 CLI 互不干扰；没有 CLI 也能在应用内一键登录
- **跨平台**：Windows / macOS（Apple Silicon），代码无平台专属 API

## 📥 下载与安装

| 平台 | 安装包 | 说明 |
| --- | --- | --- |
| Windows (x64) | `QuotaX_x.y.z_x64-setup.exe` | NSIS 安装包，双击安装 |
| macOS (Apple Silicon) | `QuotaX_x.y.z_aarch64.dmg` | DMG 镜像，拖入 Applications |

安装包可从 [Releases](https://github.com/AuroraIII/QuotaX/releases) 获取；最新构建也可在 [Actions](https://github.com/AuroraIII/QuotaX/actions) 的构建产物（Artifacts）中下载。

> **关于安全提示**：安装包未做代码签名，首次运行时系统可能弹出警告——
> - Windows SmartScreen：点击「更多信息 → 仍要运行」
> - macOS Gatekeeper：右键点击应用 →「打开」，或在「系统设置 → 隐私与安全性」中放行

## 🚀 使用指南

### 首次运行

- **已安装 Kimi Code CLI 并登录过**：无需任何操作，启动即显示额度数据
- **未安装 / 未登录**：展开卡片会出现登录入口，点击「使用 Kimi 账号登录」，在弹出的浏览器页面完成授权即可（授权成功后凭证自动保存，Kimi Code CLI 也可直接复用）

### 日常使用

- 拖动横条可移动位置（自动记忆，重启恢复）
- 悬停横条展开卡片：查看各额度进度、用量明细、余额与刷新时间
- 右键托盘图标：立即刷新 / 登录账号 / 置顶开关 / 退出
- 卡片内「刷新」按钮可手动刷新，「置顶」开关与托盘菜单同步

### 状态含义

| 显示 | 含义 |
| --- | --- |
| 正常进度条 | 数据正常 |
| ⚠ + 置灰数据 | 刷新失败（网络/服务端异常），显示上次成功数据并自动重试 |
| 登录入口 | 凭证缺失或已失效，点击重新登录 |

## ⚙️ 配置

配置文件位于 `%APPDATA%\QuotaX\config\settings.json`（macOS：`~/Library/Application Support/QuotaX/config/settings.json`），可手动编辑：

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `poll_interval_secs` | `60` | 轮询间隔（秒），钳制在 30–600，修改后下一轮生效（无需重启） |
| `collapse_on_blur` | `true` | 窗口失焦时自动收起展开卡片 |
| `always_on_top` | `true` | 窗口置顶（托盘菜单/卡片开关同步） |
| `x` / `y` | — | 窗口位置（拖动后自动记忆） |

## 🔐 数据与隐私

QuotaX **只与 Kimi 官方服务通信**，不经过任何第三方服务器：

- 额度查询：`GET https://api.kimi.com/coding/v1/usages`
- 凭证刷新：`POST https://auth.kimi.com/api/oauth/token`

凭证文件与 Kimi Code CLI 完全共用（`~/.kimi-code/credentials/kimi-code.json`），写入采用原子替换，不会损坏 CLI 的凭证。接口规格与实测记录详见 [docs/DESIGN.md](docs/DESIGN.md)。

## ❓ 常见问题

**与 Kimi Code CLI 同时使用会冲突吗？**
不会。两者共用同一凭证文件，QuotaX 每轮刷新前重新读取文件，即使 CLI 刚刷新过凭证也能正确识别；写回采用原子替换并带冲突重试，不会互相覆盖损坏。

**横条显示 ⚠ 怎么办？**
多为网络波动或服务端暂时异常，应用会自动退避重试（1m→2m→5m），通常无需处理。持续失败时悬停卡片可查看错误详情；若提示凭证失效，在卡片内重新登录即可。

**数据多久更新一次？**
默认 60 秒，可通过配置文件调整（30–600 秒）。托盘「立即刷新」或卡片刷新按钮可手动触发。

## 🛠️ 开发

### 环境要求

- Rust (stable) + Node.js ≥ 18
- Windows：WebView2 Runtime（Win11 自带）
- macOS：Xcode Command Line Tools

### 本地运行

```bash
npm install        # 安装 @tauri-apps/cli
npx tauri dev      # 开发模式（首次编译较慢）
```

### 本地打包

```bash
npx tauri build    # Windows：产出 NSIS 安装包
```

产物位于 `src-tauri/target/release/bundle/nsis/*.exe`。

macOS 包（DMG）需在 macOS 环境构建——Tauri 不支持从 Windows 交叉编译（ring C 编译、代码签名与 DMG 打包依赖 macOS SDK）。仓库已配置 CI 自动构建：推送 `v*` tag 或在 GitHub Actions 手动触发 [Release 工作流](.github/workflows/release.yml)，可同时产出 Windows 与 macOS 安装包。Intel Mac 支持方式见工作流内注释。

> 开发环境备注：在受限沙箱终端中运行 dev 时，若 WebView2 数据目录写入被拦截，可设置环境变量 `WEBVIEW2_USER_DATA_FOLDER` 指向项目内目录；沙箱或 CLI 持有凭证文件句柄导致写回被拒（os error 5）时，应用会暂存新凭证并在后续轮询自动重试，普通环境不受影响。

### 项目结构

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
