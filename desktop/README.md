# Claude 环境卫士 · EnvGuard (v2.0)

双端一体工程（基于 Vite + Tauri v2 + PowerShell 安全引擎）：

- **PC 桌面端（Tauri 原生可执行文件）**：14 项环境审计 + **受检安全启动 Claude / CLI** + 一键修复 + 无 72h/电池限制的常驻保镖 + 4 项精简实用工具 + 收工退出护盾还原。
  - 时区与区域格式按系统设置判定，属于硬性项。Claude 桌面版的 Chromium 渲染进程在 Windows 上不读 `TZ` 环境变量，MSIX 版经 shell 激活也不继承它，进程级注入骗不过网页端，所以桌面版必须把系统时区设为目标；收工时用「退出护盾」还原原时区。
  - 启动 CLI 时额外向子进程注入目标 IANA 时区（`TZ`）。
  - 「退出护盾」需 5 秒内二次确认：先关闭 Claude 桌面版与所有 Claude Code CLI（含 npm/bun 版），任一未退出则不撤防。
- **移动端（静态 Web / PWA 零安装防漏自检页）**：手机连好代理后打开即测 6 项实时信号（出口 IP 归属、浏览器时区与 UTC 偏移、WebRTC 候选泄漏（含 IPv6 候选）、IPv6 双栈暴露、DNS 泄漏（经 ipleak.net 查明实际解析器归属）、WebGL/Canvas 硬件摘要）。
- **双主题切换**：内置 `终端黑客绿 (Terminal)` 与 `赛博霓虹 (Cyber)` 两套极客主题。

---

## 1. 运行与安装

`pnpm tauri build` 的产物：

```text
desktop\src-tauri\target\release\claude-envguard.exe                                  # 免安装，直接双击运行
desktop\src-tauri\target\release\bundle\nsis\Claude 环境卫士_2.0.0_x64-setup.exe     # NSIS 安装包
desktop\src-tauri\target\release\bundle\msi\Claude 环境卫士_2.0.0_x64_zh-CN.msi      # MSI 安装包
```

> 安全说明：发布版只执行随包的 `resources\claude-switch.ps1`（打包时直接取自仓库 `windows\`，不再另存副本）。它只读继承本机已有 `%USERPROFILE%\.claude-switcher\claude-switch.ps1` 的 5 项锁定配置，新增的客户端/地区设置写入 `envguard-config.json`，检查策略写入 `checks.json`，不会覆盖旧版脚本或触碰账号目录。只有调试构建（`pnpm tauri dev`）才会改用工作区 `windows\claude-switch.ps1` 或环境变量 `ENVGUARD_CORE_SCRIPT` 指定的脚本。

---

## 2. 开发与重新打包命令

在 `desktop/` 目录下执行：

```powershell
# 启动前端开发预览（浏览器打开即进入移动端/Web 6 项实时自检模式）
pnpm dev --host

# 启动 Tauri 桌面端热重载开发窗口
pnpm tauri dev

# 构建静态 Web/PWA 包（输出到 desktop/dist/，可直接托管至 Cloudflare Pages / GitHub Pages 供手机书签使用）
pnpm build

# 打包 Windows 桌面端（免安装 exe + NSIS 安装包 + 中文 MSI）
pnpm tauri build
```

> 不要用裸 `cargo build --release` 出包：缺少 Tauri CLI 注入的 `custom-protocol`，产物会退回开发模式去加载 `http://localhost:1420`，脱离开发服务器双击只有空白窗口。

---

## 3. 隔离回归测试

在仓库根目录执行，全部使用临时目录与打桩进程，不接触真实安装、账号、计划任务或系统设置：

```powershell
# PowerShell 核心脚本：上游 Windows 回归 + EnvGuard 新增能力
powershell -NoProfile -ExecutionPolicy Bypass -File tests\run-windows-tests.ps1

# 手机自检判定逻辑（Node 自带测试运行器）
pnpm --dir desktop test

# Rust 命令桥白名单单测
cargo test --manifest-path desktop\src-tauri\Cargo.toml
```

推送到 GitHub 后，`.github/workflows/windows.yml` 会在 `windows-latest` 上依次跑以上三组测试并执行 `pnpm tauri build`，把免安装 exe、NSIS 与 MSI 安装包作为构建产物上传。

---

## 4. 跟进上游

仓库的 `upstream` 分支只保存上游 [YOURSJEYO/claude-switcher](https://github.com/YOURSJEYO/claude-switcher) 中与本项目相关的文件子集（`AGENTS.md`、`LICENSE`、`README.md`、`SECURITY.md`、`windows/`、`tests/windows-*.ps1`），`main` 在它之上做 EnvGuard 改动，因此可以用 Git 三方合并跟进上游。工具页的「检查上游更新」会列出还没合并的上游提交。

在仓库根目录执行（用独立 worktree 导入上游，不动主工作区）：

```powershell
$up = "$env:TEMP\cs-upstream"
git clone https://github.com/YOURSJEYO/claude-switcher.git $up   # 之后改用: git -C $up pull
$wt = "$env:TEMP\eg-upstream"
git worktree add $wt upstream
Push-Location $wt
git --git-dir="$up\.git" --work-tree=. checkout origin/main -- AGENTS.md LICENSE README.md SECURITY.md windows ':(glob)tests/windows-*.ps1'
git add -A
git commit -m "upstream: import claude-switcher main@<短哈希> (Windows release subset)"
Pop-Location
git -C $up reset -q
git worktree remove $wt
git merge upstream
```

上游若删除了子集中的文件，导入时要在 worktree 里同步 `git rm`。合并后把 `windows\claude-switch.ps1` 中的 `$UPSTREAM_MERGED` 改成上游 main 的完整哈希，跑通全部测试再提交。
