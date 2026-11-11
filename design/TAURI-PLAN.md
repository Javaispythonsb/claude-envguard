# Claude 环境卫士 · EnvGuard —— 双端（PC Tauri + 移动 Web/PWA）实施方案

> **最终敲定范围（v2 精简增强版）**
> - **产品定位**：Claude 环境安全 / Fail-Closed 防泄漏护盾（彻底剥离多账号切换、跨身份保留与 11.9GB 级快照拷贝包袱）。
> - **视觉主题**：`① 终端黑客绿 (Terminal)` + `② 赛博霓虹 (Cyber)` 双主题一键切换。
> - **双端分工**：
>   - **PC 桌面端（Tauri v2 + PowerShell 安全引擎）**：14 项环境审计 + **受检安全启动 Claude / CLI（CLI 额外注入目标 `TZ`）** + **收工护盾还原** + 一键修复 + 无 72h/电池限制的常驻保镖 + 4 项精简实用工具。
>   - **移动端（Web / PWA 零安装防漏自检页，共用同一套前端主题）**：手机连好代理后点开即测 6 项浏览器/网络信号（出口 IP 归属、时区/UTC 偏移一致性、WebRTC STUN 泄漏、IPv6 双栈暴露、DoH 加密 DNS、Canvas/WebGL 摘要）。

---

## 1. 架构总览

```
                    共用同一套极客主题 UI (src/)
                 ┌───────────────┴───────────────┐
                 ▼                               ▼
     [PC 桌面端 · Tauri .exe]          [移动端 · 静态 Web / PWA]
     Rust 受控命令桥 (白名单)           纯浏览器实时探测 (零安装)
                 │                     - Cloudflare Trace / IPinfo
                 ▼                     - WebRTC ICE STUN 候选比对
     windows/claude-switch.ps1         - IPv6 暴露 / DoH / WebGL
     - uistate [--fresh] (单次 JSON)
     - launch desktop|cli (Fail-Closed；仅 CLI 注入 TZ)
     - fix / shield-restore / set-region / open-safe / rules-status / fingerprint
                 │
     windows/claude-bodyguard.ps1 (常驻计划任务：无72h限制 · 含心跳 · 桌面+CLI双急停)
```

## 2. 本轮已在工作区脚本中落地的后端加固与新能力

> 全部修改仅位于工作区 `windows\*.ps1`，未触碰本机真实安装；隔离回归统一用 `tests\run-windows-tests.ps1` 运行。

1. **修复保镖静默失效隐患**（`install.ps1` / `claude-switch.ps1` / `claude-bodyguard.ps1`）：
   - 计划任务显式配置 `-ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -RestartCount 3`，根除运行满 72 小时被系统强杀及笔记本拔电源停保镖；
   - 保镖每轮写入 `bodyguard-heartbeat.txt`（PID + 时间戳），安检 `Get-BodyguardHealth` 实查进程存活与旧策略告警，不再只看“任务名存在”；
   - 保镖急停范围覆盖当前用户会话下的 Claude 桌面版、native `claude.exe` 与 `node`/`bun` 启动的 `@anthropic-ai/claude-code`。
2. **新增受检安全启动门禁**（`launch desktop` / `launch cli`）：
   - 启动前强制执行完整 `Invoke-Check` 与隧道在线复核，未通过立即物理阻断；CLI 额外拦截 `ANTHROPIC_API_KEY` 等环境变量污染；
   - 只向 CLI 子进程注入目标 IANA 时区（`TZ`）。桌面版不注入：实测 Chromium 渲染进程在 Windows 上忽略 `TZ`，MSIX 版经 shell 激活也不继承调用方环境变量，所以时区与区域格式仍按系统设置判定。
3. **护盾还原**（`shield-restore`）：
   - 原计划的 `tzMode` / `relaxLocale`「不打扰日常系统」开关已撤回：它们会把时区、地区两项硬性检查判为通过，而桌面版实际并未受到保护；
   - `shield-restore` 先关闭 Claude 桌面版与所有 Claude Code CLI（含 npm/bun 版），全部退出后才暂停保镖、恢复网卡 IPv6 与原系统时区（`home-tz.txt`），任一进程未退出则拒绝撤防；界面上需 5 秒内二次确认。
4. **新增单次聚合状态接口**（`uistate [--fresh]`）：
   - 一次调用输出完整 JSON（版本、代理模式、目标地区/IANA 时区、保镖健康度、策略配置、14 项检查结果），替代旧版每次刷新连起 4 个 PowerShell 子进程。

## 3. 目录结构规划（本机 Node v24 + Rust 1.98 已就绪）

```
desktop/                      # 新建工程目录（不动根目录旧文件）
├── index.html                # 共用入口（自动识别 Tauri 桌面 vs 移动浏览器 Web 模式）
├── src/
│   ├── styles/themes.css     # Terminal Green + Cyber 双主题令牌与响应式布局
│   ├── pc-app.js             # PC 端状态管理与 Tauri invoke 调用
│   ├── mobile-check.js       # 移动端纯浏览器 6 项防漏实时探测（复用原 docs/ 检测原理）
│   └── main.js
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/main.rs           # 严格枚举白名单调用 claude-switch.ps1，禁止任意命令拼接
└── package.json
```

## 4. 实施里程碑（大幅瘦身后更快落地）

| 阶段 | 内容 | 预估耗时 |
|---|---|---|
| **M1 工程落地与双端接通** | 初始化 `desktop/`（Vite + Tauri v2），迁入定稿双主题 UI；Rust 接通 `uistate`/`launch`/`fix`/`shield-restore`/`open-safe` 等白名单命令；移动端模式接通真实浏览器 6 项自检 | 约 1 次会话即可跑通开发态 |
| **M2 打包与细节打磨** | 构建 `.exe` / NSIS 安装包与可独立部署到 GitHub Pages/Cloudflare Pages 的移动端静态包 `dist/`，补充托盘常驻与错误提示 | 0.5–1 天 |
