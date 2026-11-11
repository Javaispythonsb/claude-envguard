# Claude 环境卫士 · EnvGuard —— 双端（PC Tauri + 移动 Web/PWA）实施方案

> **最终敲定范围（v2 精简增强版）**
> - **产品定位**：Claude 环境审计启动门禁 + 运行期进程级急停控制台（剥离多账号切换与快照拷贝；运行期保镖为 10s 巡检进程急停，非防火墙级阻断）。
> - **视觉主题**：`① 终端黑客绿 (Terminal)` + `② 赛博霓虹 (Cyber)` 双主题一键切换。
> - **双端分工**：
>   - **PC 桌面端（Tauri v2 + PowerShell 安全引擎）**：14 项启动前环境审计（8 项硬性红线含无警告要求）+ **受检安全启动 Claude / CLI** + **按快照还原各网卡 IPv6 与原时区的护盾还原** + 一键修复 + 无 72h/电池限制的常驻保镖 + 实用工具。
>   - **浏览器侧自检页（Web / PWA，共用同一套前端主题）**：评估当前设备浏览器的 5 项防漏信号（出口 IP 归属、时区/UTC 偏移一致性、WebRTC 候选、IPv6 双栈暴露、实际 DNS 解析器归属）+ 1 项硬件渲染摘要（仅参考）。

---

## 1. 架构总览

```
                    共用同一套极客主题 UI (src/)
                 ┌───────────────┴───────────────┐
                 ▼                               ▼
     [PC 桌面端 · Tauri .exe]          [浏览器侧 · 静态 Web / PWA]
     Rust 受控命令桥 (白名单)           纯浏览器实时探测 (零安装)
                 │                     - Cloudflare Trace / IPinfo
                 ▼                     - WebRTC ICE STUN 候选比对
     windows/claude-switch.ps1         - IPv6 暴露 / ipleak DNS / WebGL
     - uistate [--fresh] (单次 JSON)
     - launch desktop|cli (8 项硬性红线门禁)
     - fix / shield-restore / set-region / open-safe / rules-status / fingerprint
                 │
     windows/claude-bodyguard.ps1 (常驻计划任务：无72h限制 · 含心跳 · 桌面+CLI双急停)
```

## 2. 本轮已在工作区脚本中落地的后端加固与新能力

> 全部修改仅位于工作区 `windows\*.ps1`，未触碰本机真实安装；隔离回归统一用 `tests\run-windows-tests.ps1` 运行。

1. **修复保镖静默失效隐患与 TUN 误判**（`install.ps1` / `claude-switch.ps1` / `claude-bodyguard.ps1`）：
   - 计划任务显式配置 `-ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -RestartCount 3`，根除运行满 72 小时被系统强杀及笔记本拔电源停保镖；
   - 保镖每轮写入 `bodyguard-heartbeat.txt`（PID + 时间戳），安检 `Get-BodyguardHealth` 实查进程存活与旧策略告警，不再只看“任务名存在”；
   - Clash Verge / FlClash 模式下 TUN 只认 `198.18.0.0/15` 或 Mihomo/Clash 网卡，避免 Tailscale 等常驻虚拟网卡掩盖掉线；
   - 保镖急停范围覆盖当前用户会话下的 Claude 桌面版、native `claude.exe` 与 `node`/`bun` 启动的 `@anthropic-ai/claude-code`。
2. **新增受检安全启动门禁**（`launch desktop` / `launch cli`）：
   - 启动前强制执行完整 `Invoke-Check`（8 项硬性检查必须全部为 `ok`，含无警告要求）与隧道在线复核，未通过立即阻断；CLI 额外拦截 `ANTHROPIC_API_KEY` 等环境变量污染。
3. **护盾还原**（`shield-restore`）：
   - 先关闭 Claude 桌面版与所有 Claude Code CLI（含 npm/bun 版），全部退出后才暂停保镖、按 `ipv6-state.json` 恢复各网卡原 IPv6 状态并按 `home-tz.txt` 还原原系统时区（缺失快照时如实告警）；任一进程未退出则拒绝撤防；界面上需 5 秒内二次确认。
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
