<div align="center">

# Claude 环境卫士 · EnvGuard (v2.0)

**Windows 启动前环境审计门禁 · 运行期进程级急停 · 浏览器侧防漏自检**

[![License: AGPL-3.0-only](https://img.shields.io/badge/License-AGPL--3.0--only-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%2F11-0078D6.svg)](desktop/)
[![Desktop](https://img.shields.io/badge/Desktop-Tauri%20v2%20%2B%20PowerShell-24C8DB.svg)](desktop/)
[![Web/PWA](https://img.shields.io/badge/Web%20%2F%20PWA-Browser%20Self--Check-39FF14.svg)](desktop/)

</div>

---

## 项目来源与许可证声明

本项目基于开源项目 [YOURSJEYO/claude-switcher](https://github.com/YOURSJEYO/claude-switcher)（v1.9.0 及 `main@cf474a2` Windows 子集）改造衍生，遵循 **[AGPL-3.0-only](LICENSE)** 许可证发布。完整修改记录可通过 Git 提交历史、`desktop/`、`windows/` 与 `tests/` 查看。

- **用途声明**：本项目用于在合规支持地区审计本机网络与系统环境一致性、降低代理异常掉线导致的裸连风险。请遵守 Anthropic 服务条款；本地环境检查只作用于本机配置与进程，不改变服务端记录，也不构成账号不被限制的保证。
- **上游官方免费声明保留**：上游 `claude-switcher` 官方版本免费提供，任何人不得冒充原作者或官方渠道收费销售，也不得删除版权与 `AGPL-3.0-only` 许可证声明或隐瞒修改内容。

---

## 项目定位

相比原版多账号切换器，**EnvGuard v2.0** 剥离了多账号切换与十余 GB 级目录快照拷贝负担，聚焦做一件事：**单账号日常使用下的启动前环境审计门禁 + 运行期掉线进程急停控制台**，并配套一套共用主题的**浏览器侧（桌面/手机 Web & PWA）信号自检页**。

> **安全边界诚实说明（详见 [SECURITY.md](SECURITY.md)）**：
> - **启动前门禁**：14 项审计未完成、存在任一 `fail`、或 6 项硬性防漏红线中任一项非 `ok`（含 `warn`/`skip`）时，启动入口锁定拒绝拉起。
> - **运行期保镖**：常驻计划任务以 **10 秒周期**巡检「代理内核进程 × 专属 TUN 隧道」双因子，掉线后终止受保护进程。这是**进程级急停**而非防火墙级包过滤，巡检周期内存在固有的时间窗口（TOCTOU）。

---

## 功能总览

### 一、PC 桌面端（Tauri v2 原生单文件 / 安装包）

1. **14 项并行环境审计引擎（6 项硬性防漏红线 + 8 项可选检查）**
   - **6 项硬性防漏红线（必须全部为 `[ OK ]`，出现 `warn`/`skip`/`fail` 均阻断启动）**：
     `01 代理内核` · `02 TUN 网卡`（Clash Verge / FlClash 模式只认 `198.18.0.0/15` 或 Mihomo/Clash 网卡，排除 Tailscale 等常驻虚拟网卡误报）· `03 出口国家与 IP`（隧道存活门控 + 瞬时抖动重试）· `04 IPv6 旁路防护` · `05 保镖进程急停`（实查运行态、进程心跳与 72h/电池旧策略告警）· `06 置顶规则 / 隧道接管`（支持 Clash Verge 命名管道与 HTTP 双通道直读内核 `/rules`，拦截 `direct` 直连模式，兼容非 Mihomo 客户端 Fake-IP 与 WireGuard 全局路由核验）。
   - **8 项可选环境检查（支持独立开关与失败降级为警告）**：
     `07 时区对齐` · `08 区域格式 Locale`（这两项可在「护盾与设置」中按需开关：开启时严格检查并随一键修复自动同步；关闭时如实标为 `[SKIP]` 且一键修复/重锁地区不会改动本机系统时钟与日期格式）· `09 IP 稳定性 (48h)` · `10 DNS 隧道与解析器归属`（核验 `198.18.0.0/15` 并通过 `ipleak.net` 实查上游 DNS 解析器国家，拦截 Fake-IP 掩盖下的国内解析器泄漏）· `11 WebRTC STUN 交叉探针`（国内外 4 节点并发探测）· `12 客户端语言` · `13 ASN 网络属性` · `14 Claude 生态可达性`。
2. **受检安全启动 Claude 桌面版 / CLI**
   - 点击启动时强制复核完整审计与代理隧道在线状态；启动 CLI 前额外拦截 `ANTHROPIC_API_KEY`、`ANTHROPIC_AUTH_TOKEN` 等环境变量覆盖。
3. **无 72h / 电池限制的常驻保镖**
   - 注册计划任务时显式解除 Windows 默认 72 小时超时强杀与笔记本电池断电停止限制，每 10 秒写入 `bodyguard-heartbeat.txt` 心跳；断线时同时终止当前用户会话下的 Claude 桌面版、原生 `claude.exe` 及 `node`/`bun` 运行的 `@anthropic-ai/claude-code`。
4. **系统托盘常驻与单实例防多开**
   - 关闭主窗口自动隐藏驻留至系统托盘（不退出监控台、不停后台保镖）；左键托盘图标或重复运行程序会自动唤醒已有主窗口；右键托盘提供「安全启动 Claude / CLI」「重新检查环境」「退出护盾」「退出控制台」快捷菜单。
5. **收工退出护盾（按快照精准还原）**
   - 5 秒内二次点击确认；先关闭 Claude 桌面版与所有 Claude Code CLI 会话（任一未退出则拒绝撤防），再暂停保镖、按 `ipv6-state.json` 逐块恢复各网卡原始 IPv6 绑定状态，并按 `home-tz.txt` 还原原系统时区（缺失旧快照时明确告警提示）。
6. **6 项实用工具 + 双极客主题**
   - 内置**一次性隔离浏览器 (`open-safe`)**、**换节点重锁地区 (`set-region`)**、**Clash 置顶规则透视 (`rules-status`)**、**硬件与虚拟化指纹 (`fingerprint`)**、**导出桌面脱敏诊断包 (`export-diagnostics`)**、**检查上游更新 (`upstream-status`)**。
   - 支持 `终端黑客绿 (Terminal)` 与 `赛博霓虹 (Cyber)` 双主题一键切换。

### 二、浏览器侧自检页（Web / PWA）

可直接在桌面端顶部切换到内置视图，也可将 `desktop/dist/` 静态部署至 Cloudflare Pages / GitHub Pages 供手机或任意浏览器零安装打开：
- **5 项防漏信号**：`01 出口 IP 与归属国` · `02 浏览器时区与 UTC 偏移` · `03 WebRTC STUN 候选（含 IPv6 与 raddr）` · `04 IPv6 双栈暴露` · `05 DNS 泄漏检测（查验实际解析器归属国）`。
- **1 项硬件参考摘要**：`06 WebGL / Canvas 硬件摘要`（仅作参考展示，不混入泄漏风险计数）。

---

## 快速开始

### 1. 运行桌面端安装包或免安装版

构建产物位于 `desktop/src-tauri/target/release/`：

- 免安装可执行文件：`desktop\src-tauri\target\release\claude-envguard.exe`
- NSIS 安装包：`desktop\src-tauri\target\release\bundle\nsis\Claude 环境卫士_2.0.0_x64-setup.exe`
- 中文 MSI 安装包：`desktop\src-tauri\target\release\bundle\msi\Claude 环境卫士_2.0.0_x64_zh-CN.msi`

详细开发、打包与跟进上游合并说明见 **[desktop/README.md](desktop/README.md)**；底层 PowerShell 命令与旧版说明见 **[windows/README.md](windows/README.md)**。

### 2. 运行自动化隔离回归测试

全部测试运行于临时沙箱目录与打桩进程，不触碰本机真实配置、计划任务或网络设置：

```powershell
# 1) Windows PowerShell 核心引擎集成测试（11 个测试集）
powershell -NoProfile -ExecutionPolicy Bypass -File tests\run-windows-tests.ps1

# 2) 浏览器侧自检逻辑单测（Node.js）
pnpm --dir desktop test

# 3) Tauri Rust 命令桥白名单与托盘映射单测
cargo test --manifest-path desktop\src-tauri\Cargo.toml
```
