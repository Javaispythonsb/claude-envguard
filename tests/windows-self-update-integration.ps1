# EnvGuard GitHub 热更新引擎隔离集成测试
#
# 只用临时沙箱目录 + 内存打桩，不发真实网络请求、不改动本机安装态或计划任务。

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$core = Join-Path $repo 'windows\claude-switch.ps1'
if (-not (Test-Path -LiteralPath $core)) { throw "找不到脚本: $core" }

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-selfupdate-it-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($name in @('USERPROFILE', 'ENVGUARD_STATE_DIR', 'ENVGUARD_APP_VERSION', 'ENVGUARD_APP_COMMIT', 'ENVGUARD_APP_COMMIT_UNIX', 'ENVGUARD_GITHUB_REPO', 'ENVGUARD_PROGRESS', 'TEMP', 'TMP')) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}

$failures = 0
function Assert([string]$what, [bool]$condition, [string]$detail = '') {
    if ($condition) {
        "PASS $what"
    } else {
        $script:failures++
        "FAIL $what $(if ($detail) { "($detail)" })"
    }
}

try {
    $env:USERPROFILE = Join-Path $root 'user'
    $env:ENVGUARD_STATE_DIR = Join-Path $env:USERPROFILE '.claude-switcher'
    $env:TEMP = Join-Path $root 'temp'
    $env:TMP = $env:TEMP
    $env:ENVGUARD_GITHUB_REPO = 'Javaispythonsb/claude-envguard'
    foreach ($d in @($env:USERPROFILE, $env:ENVGUARD_STATE_DIR, $env:TEMP)) {
        New-Item -ItemType Directory -Path $d -Force | Out-Null
    }

    . $core

    # 1. URL 白名单：仅接受本仓库 releases/download 路径或官方 CDN，拒绝其它 GitHub 仓库或非 HTTPS
    Assert '允许本仓库 Release 下载地址' (Test-EnvGuardReleaseUrl 'https://github.com/Javaispythonsb/claude-envguard/releases/download/latest/claude-envguard-windows-x64.zip')
    Assert '允许 GitHub Release 对象 CDN' (Test-EnvGuardReleaseUrl 'https://objects.githubusercontent.com/github-production-release-asset/12345')
    Assert '拒绝其它仓库 Release 地址' (-not (Test-EnvGuardReleaseUrl 'https://github.com/other-owner/evil-repo/releases/download/latest/claude-envguard-windows-x64.zip'))
    Assert '拒绝非 HTTPS 地址' (-not (Test-EnvGuardReleaseUrl 'http://github.com/Javaispythonsb/claude-envguard/releases/download/latest/a.zip'))

    # 2. 版本与提交级比对（支持同版本号新提交滚动热更新 & 防本地超前误降级）
    $shaA = '1111111111111111111111111111111111111111'
    $shaB = '2222222222222222222222222222222222222222'
    $shaC = '3333333333333333333333333333333333333333'

    $localSame = @{ version = '2.0.0'; commit = $shaA; short_commit = '1111111'; commit_unix = [int64]1794400000 }
    $remoteA = [pscustomobject]@{ version = '2.0.0'; commit = $shaA; short_commit = '1111111'; commit_unix = 1794400000; built_at = '2026-11-12T06:00:00Z'; notes = 'init' }
    $cmp1 = Compare-EnvGuardBuild $localSame $remoteA $null
    Assert '相同提交判定为 up_to_date' ($cmp1.status -eq 'up_to_date') "status=$($cmp1.status)"

    $atomPending = @{ commit = $shaB; short_commit = '2222222'; title = 'feat: new ui' }
    $cmpPending = Compare-EnvGuardBuild $localSame $remoteA $atomPending
    Assert 'main 有更新但 Release 尚未产出时标记 ci_building' ($cmpPending.status -eq 'up_to_date' -and $cmpPending.ci_building -eq $true -and $cmpPending.ci_head_commit -eq '2222222')

    $remoteB = [pscustomobject]@{ version = '2.0.0'; commit = $shaB; short_commit = '2222222'; commit_unix = 1794403600; built_at = '2026-11-12T07:00:00Z'; notes = 'feat: hot update' }
    $cmp2 = Compare-EnvGuardBuild $localSame $remoteB $null
    Assert '同为 2.0.0 但远端提交更新时判定为 update_available' ($cmp2.status -eq 'update_available' -and $cmp2.latest_commit -eq '2222222')

    $localNewer = @{ version = '2.0.0'; commit = $shaC; short_commit = '3333333'; commit_unix = [int64]1794409999 }
    $cmpAhead = Compare-EnvGuardBuild $localNewer $remoteB $null
    Assert '本地提交晚于云端 Release 时判定为 ahead 防止倒退覆盖' ($cmpAhead.status -eq 'ahead') "status=$($cmpAhead.status)"

    # 3. 完整暂存热更新流程：SHA-256 篡改拒绝 + 正常通过 + 保留 5 项头部配置与 UTF-8 BOM + LF
    $appDir = Join-Path $root 'app'
    $appRes = Join-Path $appDir 'resources'
    New-Item -ItemType Directory -Force -Path $appRes | Out-Null

    # 准备本机已安装的自定义配置脚本
    $customCore = ([System.IO.File]::ReadAllText($core, [System.Text.Encoding]::UTF8))
    $customCore = Set-ScriptConfigValue $customCore 'PROXY_MODE' 'flclash'
    $customCore = Set-ScriptConfigValue $customCore 'COUNTRY_TARGET' 'JP'
    $customCore = Set-ScriptConfigValue $customCore 'TZ_TARGET' 'Tokyo Standard Time'
    $customCore = Set-ScriptConfigValue $customCore 'LOCALE_TARGET' 'ja-JP'
    $customCore = Set-ScriptConfigValue $customCore 'PROXY_GROUP' 'MyHomeISP'
    $installedCorePath = Join-Path $env:ENVGUARD_STATE_DIR 'claude-switch.ps1'
    Write-Utf8BomLf $installedCorePath $customCore

    # 构造模拟远端发布 zip 包（含合法 MZ 头的 600KB 假 exe + 新版脚本）
    $pkgBuild = Join-Path $root 'pkg-build'
    New-Item -ItemType Directory -Force -Path (Join-Path $pkgBuild 'resources') | Out-Null
    $fakeExeBytes = New-Object byte[] 614400
    $fakeExeBytes[0] = 0x4D # 'M'
    $fakeExeBytes[1] = 0x5A # 'Z'
    [System.IO.File]::WriteAllBytes((Join-Path $pkgBuild 'claude-envguard.exe'), $fakeExeBytes)
    Copy-Item -LiteralPath $core -Destination (Join-Path $pkgBuild 'resources\claude-switch.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $repo 'windows\claude-bodyguard.ps1') -Destination (Join-Path $pkgBuild 'resources\claude-bodyguard.ps1') -Force

    $mockZip = Join-Path $root 'claude-envguard-windows-x64.zip'
    Compress-Archive -Path "$pkgBuild\*" -DestinationPath $mockZip -Force
    $realHash = (Get-FileHash -LiteralPath $mockZip -Algorithm SHA256).Hash.ToLowerInvariant()
    $goodSums = Join-Path $root 'SHA256SUMS-good.txt'
    $badSums = Join-Path $root 'SHA256SUMS-bad.txt'
    [System.IO.File]::WriteAllText($goodSums, "$realHash  claude-envguard-windows-x64.zip`n", [System.Text.Encoding]::ASCII)
    [System.IO.File]::WriteAllText($badSums, ("0" * 64) + "  claude-envguard-windows-x64.zip`n", [System.Text.Encoding]::ASCII)

    function Get-ScheduledTask { return $null }
    $script:useBadSums = $true
    function Invoke-WebRequest {
        param([string]$Uri, [string]$UserAgent, [hashtable]$Headers, [string]$OutFile, [switch]$UseBasicParsing, [int]$TimeoutSec)
        if ($Uri -like '*claude-envguard-windows-x64.zip*') {
            Copy-Item -LiteralPath $mockZip -Destination $OutFile -Force
            return
        }
        if ($Uri -like '*SHA256SUMS-release.txt*') {
            $srcSums = if ($script:useBadSums) { $badSums } else { $goodSums }
            Copy-Item -LiteralPath $srcSums -Destination $OutFile -Force
            return
        }
        throw "Unexpected Uri: $Uri"
    }
    function Get-EnvGuardUpdateState {
        return [ordered]@{
            status          = 'update_available'
            repo            = 'Javaispythonsb/claude-envguard'
            current_version = '2.0.0'
            current_commit  = '1111111'
            latest_version  = '2.1.0'
            latest_commit   = '2222222'
            latest_full_sha = $shaB
            built_at        = '2026-11-12T08:00:00Z'
            notes           = 'release 2.1.0'
            zip_url         = 'https://github.com/Javaispythonsb/claude-envguard/releases/download/latest/claude-envguard-windows-x64.zip'
            sums_url        = 'https://github.com/Javaispythonsb/claude-envguard/releases/download/latest/SHA256SUMS-release.txt'
            page_url        = 'https://github.com/Javaispythonsb/claude-envguard/releases/tag/latest'
            message         = 'update available'
        }
    }

    # 3a) 篡改 SHA-256 必须立即拒绝且不残留 .exe.new
    $script:exitCode = 0
    $badOut = (@(Invoke-StageAppUpdate $appDir) -join "`n")
    Assert 'SHA-256 不匹配时拒绝热更新并报错' ($script:exitCode -eq 1 -and $badOut -match '^FAIL:.*SHA-256') "out=$badOut"
    Assert 'SHA-256 失败后不残留 claude-envguard.exe.new' (-not (Test-Path -LiteralPath (Join-Path $appDir 'claude-envguard.exe.new')))

    # 3b) 合法 SHA-256 成功暂存 exe.new 并保留全部 5 项用户头部配置 + UTF-8 BOM + LF
    $script:useBadSums = $false
    $script:exitCode = 0
    $okOut = (@(Invoke-StageAppUpdate $appDir) -join "`n")
    Assert 'SHA-256 校验通过后成功暂存热更新' ($script:exitCode -eq 0 -and $okOut -match '^OK:') "out=$okOut"
    Assert '已生成 claude-envguard.exe.new' (Test-Path -LiteralPath (Join-Path $appDir 'claude-envguard.exe.new'))
    Assert '已同步更新应用旁 resources\claude-switch.ps1' (Test-Path -LiteralPath (Join-Path $appRes 'claude-switch.ps1'))

    $updatedBytes = [System.IO.File]::ReadAllBytes($installedCorePath)
    $hasBom = ($updatedBytes[0] -eq 0xEF -and $updatedBytes[1] -eq 0xBB -and $updatedBytes[2] -eq 0xBF)
    $hasCrlf = ([System.Text.Encoding]::UTF8.GetString($updatedBytes)).Contains("`r`n")
    $updatedText = [System.IO.File]::ReadAllText($installedCorePath, [System.Text.Encoding]::UTF8)
    Assert '更新后的核心脚本保持 UTF-8 BOM 且无 CRLF' ($hasBom -and -not $hasCrlf)
    Assert '更新后完整保留用户 5 项锁定配置' (
        (Get-ScriptConfigValue $updatedText 'PROXY_MODE') -eq 'flclash' -and
        (Get-ScriptConfigValue $updatedText 'COUNTRY_TARGET') -eq 'JP' -and
        (Get-ScriptConfigValue $updatedText 'TZ_TARGET') -eq 'Tokyo Standard Time' -and
        (Get-ScriptConfigValue $updatedText 'LOCALE_TARGET') -eq 'ja-JP' -and
        (Get-ScriptConfigValue $updatedText 'PROXY_GROUP') -eq 'MyHomeISP'
    )
    Assert '暂存结果文案只含版本号，不含提交哈希' ($okOut -match 'v2\.1\.0' -and $okOut -notmatch '2222222')

    # 4. 进度输出：开启 ENVGUARD_PROGRESS 时流式下载逐步报告百分比，且进度行只走控制台、不进管道
    $env:ENVGUARD_PROGRESS = '1'
    $progressSink = New-Object System.IO.StringWriter
    $savedOut = [Console]::Out
    try {
        [Console]::SetOut($progressSink)
        $streamCopy = Join-Path $root 'streamed.zip'
        $piped = @(Save-EnvGuardAsset ([Uri]$mockZip).AbsoluteUri $streamCopy 30 'download' 6 75)
    } finally {
        [Console]::SetOut($savedOut)
        $env:ENVGUARD_PROGRESS = $null
    }
    $progressLines = @($progressSink.ToString() -split "`r?`n" | Where-Object { $_ -match '^PROGRESS:download:\d+$' })
    $pcts = @($progressLines | ForEach-Object { [int]($_ -replace '^PROGRESS:download:', '') })
    Assert '流式下载产物与源文件哈希一致' ((Get-FileHash -LiteralPath $streamCopy -Algorithm SHA256).Hash.ToLowerInvariant() -eq $realHash)
    Assert '进度从下载区间起点附近递增到终点 75' ($pcts.Count -ge 2 -and $pcts[-1] -eq 75 -and $pcts[0] -ge 6) "pcts=$($pcts -join ',')"
    $monotonic = $true
    for ($i = 1; $i -lt $pcts.Count; $i++) { if ($pcts[$i] -lt $pcts[$i - 1]) { $monotonic = $false } }
    Assert '进度百分比单调不减' $monotonic
    Assert '进度行不进入函数输出管道' ($piped.Count -eq 0) "piped=$($piped -join '|')"

    # 5. 发布说明：英文提交标题不展示给用户，中文说明原样保留
    Assert '英文提交标题替换为固定中文说明' ((ConvertTo-EnvGuardReleaseNote 'feat(desktop): add orb') -eq '常规维护与稳定性改进')
    Assert '中文发布说明原样保留' ((ConvertTo-EnvGuardReleaseNote '  修复悬浮球拖动  ') -eq '修复悬浮球拖动')
    $cmpMsg = Compare-EnvGuardBuild $localSame $remoteB $null
    Assert '同版本维护更新的提示不含提交哈希' ($cmpMsg.message -notmatch '2222222' -and $cmpMsg.message -match 'v2\.0\.0') "msg=$($cmpMsg.message)"
} finally {
    foreach ($k in $savedEnv.Keys) {
        [Environment]::SetEnvironmentVariable($k, $savedEnv[$k])
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    exit 1
}
