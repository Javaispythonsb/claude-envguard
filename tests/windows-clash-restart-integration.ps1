# Clash Verge 重启竞态回归测试:修复重启 Clash 后必须等到新内核与新 TUN 网卡就绪
#
# 只用临时沙箱目录、打桩 cmdlet 与本地 node 执行 Script.js，不改动本机任何配置、Clash 文件、注册表或网络。
# powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-clash-restart-integration.ps1

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$core = Join-Path $repo 'windows\claude-switch.ps1'
if (-not (Test-Path -LiteralPath $core)) { throw "找不到核心脚本: $core" }

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-clashrestart-it-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($name in @('USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'ENVGUARD_STATE_DIR')) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:USERPROFILE = Join-Path $root 'user'
$env:APPDATA = Join-Path $root 'appdata'
$env:LOCALAPPDATA = Join-Path $root 'localappdata'
$env:TEMP = Join-Path $root 'temp'
$env:TMP = $env:TEMP
[Environment]::SetEnvironmentVariable('ENVGUARD_STATE_DIR', $null)
foreach ($d in @($env:USERPROFILE, $env:APPDATA, $env:LOCALAPPDATA, $env:TEMP)) {
    New-Item -ItemType Directory -Path $d -Force | Out-Null
}

function global:Get-AppxPackage {
    [CmdletBinding()]
    param([string]$Name)
    return $null
}

$failures = 0
function Assert([string]$what, [bool]$condition, [string]$detail = '') {
    if ($condition) {
        "PASS  $what"
    } else {
        "FAIL  $what $detail"
        $script:failures++
    }
}

$scriptErrorPreference = $ErrorActionPreference
try {
    . $core *> $null
    $ErrorActionPreference = 'Stop'

    $CLASH_DIR = Join-Path $root 'clash'
    New-Item -ItemType Directory -Path $CLASH_DIR -Force | Out-Null

    # 旧内核 PID=100 在第 3 次等待后才被新内核 PID=200 取代,Meta TUN 网卡第 5 次等待后才重建上线
    $script:tick = 0
    $script:stoppedGui = @()
    $script:started = @()
    function Start-Sleep { param($Seconds, $Milliseconds) $script:tick++ }
    function Find-ClashVerge { 'C:\Program Files\Clash Verge\clash-verge.exe' }
    function Get-Process {
        [CmdletBinding()]
        param([string[]]$Name, [int]$Id)
        $out = @()
        foreach ($n in @($Name)) {
            if ($n -eq 'verge-mihomo') {
                $kid = if ($script:tick -ge 3) { 200 } else { 100 }
                $out += [pscustomobject]@{ Id = $kid; ProcessName = 'verge-mihomo' }
            } elseif ($n -eq 'clash-verge') {
                $out += [pscustomobject]@{ Id = 50; ProcessName = 'clash-verge' }
            }
        }
        $out
    }
    function Stop-Process {
        [CmdletBinding()]
        param([Parameter(ValueFromPipeline = $true)]$InputObject, [int]$Id, [switch]$Force)
        process { $script:stoppedGui += $InputObject.Id }
    }
    function Start-Process {
        [CmdletBinding()]
        param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, [string]$Verb)
        $script:started += [string]$FilePath
    }
    function Get-NetAdapter {
        [CmdletBinding()]
        param()
        $list = @([pscustomobject]@{ Name = '以太网'; InterfaceDescription = 'Realtek'; Status = 'Up' })
        if ($script:tick -ge 5) { $list += [pscustomobject]@{ Name = 'Meta'; InterfaceDescription = 'Meta Tunnel'; Status = 'Up' } }
        $list
    }
    function Test-ProxyAlive { $true }

    $script:ClashRestarted = $false
    $ok = Restart-ClashVerge -KernelWaitSec 20 -TunWaitSec 20
    Assert 'Restart-ClashVerge 成功并标记本次已重启' ($ok -and $script:ClashRestarted)
    Assert '只结束 GUI 进程并重新拉起 Clash Verge' (($script:stoppedGui -join ',') -eq '50' -and $script:started.Count -eq 1) "stopped=$($script:stoppedGui -join ',') started=$($script:started.Count)"
    Assert '返回前已等到新内核(PID 变化)' ((@(Get-MihomoKernelPids) -join ',') -eq '200')
    Assert '返回前已等到重建后的 Meta TUN 网卡上线' ([bool](Get-MihomoTunNic)) "tick=$($script:tick)"

    # 内核在等待期内没换 PID(部分版本复用服务内核)时不得无限阻塞,超时后照常返回
    $script:tick = 0
    function Get-Process {
        [CmdletBinding()]
        param([string[]]$Name, [int]$Id)
        if (@($Name) -contains 'verge-mihomo') { return [pscustomobject]@{ Id = 100; ProcessName = 'verge-mihomo' } }
        if (@($Name) -contains 'clash-verge') { return [pscustomobject]@{ Id = 50; ProcessName = 'clash-verge' } }
    }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $ok2 = Restart-ClashVerge -KernelWaitSec 1 -TunWaitSec 1
    $sw.Stop()
    Assert '内核 PID 未变化时按超时返回而不是卡死' ($ok2 -and $sw.Elapsed.TotalSeconds -lt 10) "elapsed=$($sw.Elapsed.TotalSeconds)"

    function Find-ClashVerge { $null }
    Assert '找不到 Clash Verge 时返回 false' (-not (Restart-ClashVerge))

    # 桌面版经 Tauri resource_dir() 传入 \\?\C:\... 路径:必须规范为普通路径,否则 Join-Path 抛"参数 drive 的值为空"中断修复
    Assert '\\?\ 盘符路径被规范为普通路径' ((ConvertFrom-VerbatimPath '\\?\C:\Users\a\envguard\resources') -eq 'C:\Users\a\envguard\resources')
    Assert '\\?\UNC 路径被规范为 \\server\share' ((ConvertFrom-VerbatimPath '\\?\UNC\srv\share\x') -eq '\\srv\share\x')
    Assert '普通路径保持不变' ((ConvertFrom-VerbatimPath 'D:\repo\windows') -eq 'D:\repo\windows')
    Assert '规范后的目录可用于 Join-Path' ((Join-Path (ConvertFrom-VerbatimPath '\\?\C:\Users\a\resources') 'claude-bodyguard.ps1') -eq 'C:\Users\a\resources\claude-bodyguard.ps1')

    # 端到端:真的经 \\?\ 路径运行一段脚本,确认核心脚本开头的规范代码让 Join-Path 正常
    $coreText = [System.IO.File]::ReadAllText($core, [System.Text.Encoding]::UTF8)
    $from = $coreText.IndexOf('function ConvertFrom-VerbatimPath')
    $endMarker = '$SCRIPT_DIR = ConvertFrom-VerbatimPath ([string]$PSScriptRoot)'
    $to = $coreText.IndexOf($endMarker) + $endMarker.Length
    Assert '核心脚本开头包含路径规范代码' ($from -ge 0 -and $to -gt $from)
    $probeDir = Join-Path $root 'probe'
    New-Item -ItemType Directory -Path $probeDir -Force | Out-Null
    $probe = Join-Path $probeDir 'p.ps1'
    $probeBody = $coreText.Substring($from, $to - $from) + "`r`n'DIR=' + `$SCRIPT_DIR`r`n'JOIN=' + (Join-Path `$SCRIPT_DIR 'x.ps1')`r`n"
    [System.IO.File]::WriteAllText($probe, $probeBody, (New-Object System.Text.UTF8Encoding($true)))
    $viaVerbatim = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File ('\\?\' + $probe) 2>&1 | Out-String)
    Assert '经 \\?\ 路径运行时 $SCRIPT_DIR 为普通路径且 Join-Path 正常' ($viaVerbatim -match ('DIR=' + [regex]::Escape($probeDir) + '\s') -and $viaVerbatim -match ('JOIN=' + [regex]::Escape((Join-Path $probeDir 'x.ps1')))) "out=$viaVerbatim"

    # Invoke-AdminScript:只用写沙箱文件等无害命令验证,绝不执行任何系统设置命令
    $markFile = Join-Path $root 'admin-mark.txt'
    $markEsc = $markFile.Replace("'", "''")
    function Test-CurrentProcessElevated { $true }
    $startedBefore = $script:started.Count
    $adm = Invoke-AdminScript @("Set-Content -LiteralPath '$markEsc' -Value ok")
    Assert '已是管理员时直接执行,不走 RunAs' ($adm.ok -and $adm.direct -and $script:started.Count -eq $startedBefore -and (Test-Path -LiteralPath $markFile)) "detail=$($adm.detail) started=$($script:started.Count)"
    $adm = Invoke-AdminScript @("Get-Item -LiteralPath '$($root.Replace("'", "''"))\no-such-file.txt' -ErrorAction Continue")
    Assert '命令报错时把错误信息带回调用方' ($adm.ok -and $adm.detail -match '部分命令报错') "detail=$($adm.detail)"
    $adm = Invoke-AdminScript @('exit 3')
    Assert '管理员脚本非零退出码时判失败并带回退出码' ((-not $adm.ok) -and $adm.detail -match '退出码=3') "detail=$($adm.detail)"
    $leftovers = @(Get-ChildItem -LiteralPath ([System.IO.Path]::GetTempPath()) -Filter 'claude-fix-*' -ErrorAction SilentlyContinue)
    Assert '执行后清理临时脚本与结果文件' ($leftovers.Count -eq 0) "left=$($leftovers.Count)"

    function Test-CurrentProcessElevated { $false }
    $script:started = @()
    function Start-Process {
        [CmdletBinding()]
        param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, [string]$Verb)
        if ($Verb -eq 'RunAs') { throw 'The operation was canceled by the user' }
    }
    $adm = Invoke-AdminScript @("Set-Content -LiteralPath '$markEsc' -Value again")
    Assert '非管理员且 UAC 被取消时判失败并说明原因' ((-not $adm.ok) -and (-not $adm.direct) -and $adm.detail -match '管理员授权被取消') "detail=$($adm.detail)"
} finally {
    $ErrorActionPreference = $scriptErrorPreference
    foreach ($name in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name]) }
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    "FAILED: $failures 项"
    exit 1
}
'OK: 全部通过'
