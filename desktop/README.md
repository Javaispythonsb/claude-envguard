# Claude 环境卫士 · EnvGuard (v2.1)

双端一体工程（基于 Vite + Tauri v2 + PowerShell 安全引擎）：

- **PC 桌面端（Tauri 原生可执行文件）**：14 项启动前审计门禁（6 项硬性防漏红线必须全部为 `ok`，含无警告要求）+ **受检安全启动 Claude / CLI** + 一键修复 + **运行期进程级急停保镖**（10s 巡检，无 72h/电池限制；存在巡检周期窗口，非防火墙级阻断）+ **系统托盘常驻与单实例防多开** + 实用工具 + 收工护盾还原。
  - 关闭窗口自动隐藏驻留至系统托盘（不停止后台保镖），左键托盘或再次启动可执行文件会唤醒已有窗口；右键托盘菜单可直接启动 Claude / CLI、重新检查、触发退出护盾或退出控制台。
  - **时区与区域格式交由开关控制**：在「护盾与设置」中开启时严格核验并随「一键修复」自动同步（收工点「退出护盾」按快照还原）；关闭时如实显示 `[SKIP]`，且「一键修复 / 重锁地区」保留本机系统时钟与日常日期数字格式不作修改。
  - 「退出护盾」需 5 秒内二次确认：先关闭 Claude 桌面版与所有 Claude Code CLI（含 npm/bun 版），任一未退出则不撤防。
- **浏览器侧自检页（静态 Web / PWA）**：评估当前设备浏览器的 **5 项防漏信号**（出口 IP 归属、浏览器时区与 UTC 偏移、WebRTC 候选含 IPv6、IPv6 双栈暴露、经 ipleak.net 查验实际 DNS 解析器归属）与 **1 项硬件渲染摘要**（WebGL/Canvas，仅作参考不计入泄漏计数）；不替代桌面端 14 项进程与系统级审计。
- **待机形态只有贴边光条**：胶囊拖到屏幕上、左、右边缘会吸附，鼠标离开约 1.2 秒后收成 9px 光条，鼠标移到光条上即展开；不贴边时停在原处并半透明。拖动由 Rust 侧移动窗口本身，系统关闭「拖动时显示窗口内容」时也不会出现黑色线框。旧版本保存的「陶瓷盾球」设置会自动按光条处理。
- **GitHub 一键热更新**：推送至 `main` 分支后，GitHub Actions 自动跑通三端测试并发布带 `SHA256SUMS-release.txt` 与 `BUILD_INFO.json` 的 `latest` Release；桌面端在「工具 → 检查更新」中可直接拉取最新构建、核验 SHA-256、保留本机配置后原地热替换可执行文件与脚本并自动重启，更新过程显示分阶段进度条。检查结果只显示版本号与中文更新说明，不显示提交哈希、提交标题或仓库地址。

### 发版约定

- 面向用户的版本要同时修改四处版本号：`VERSION`、`desktop/package.json`、`desktop/src-tauri/tauri.conf.json`、`desktop/src-tauri/Cargo.toml`（CI 会检查四者一致）。本地版本取自 `tauri.conf.json`，远端比对取自 `VERSION`，两边不一致会让客户端误判更新状态。
- 更新说明写在仓库根目录 `RELEASE_NOTES.json` 的 `notes` 字段，用中文写给用户看，`version` 必须与本次版本号相同；不相同时 CI 会改用固定文案「常规维护与稳定性改进」。
- 不改版本号直接推送也会发布一次「维护更新」，客户端用构建时间区分新旧。

---

## 1. 运行与安装

`pnpm tauri build` 的产物：

```text
desktop\src-tauri\target\release\claude-envguard.exe                                  # 免安装，直接双击运行
desktop\src-tauri\target\release\bundle\nsis\Claude 环境卫士_2.1.1_x64-setup.exe     # NSIS 安装包
desktop\src-tauri\target\release\bundle\msi\Claude 环境卫士_2.1.1_x64_zh-CN.msi      # MSI 安装包
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

# Rust 命令桥白名单、窗口停靠几何与贴边收起状态机单测
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
