# EnvGuard 新增能力的隔离集成测试
#
# 只用临时目录 + 打桩的进程/计划任务/启动命令，不接触真实 Claude、账号、计划任务、系统设置或网络。
# 在 Windows PowerShell 5.1 下运行：
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-envguard-integration.ps1
#
# 覆盖：检查配置不接受时区/地区放宽字段 / 时区与地区按系统判定 / 受检启动（桌面版不注入 TZ，CLI 注入）/
# 退出护盾对桌面版与 npm/bun CLI 的 Fail-Closed / 保镖健康度 / 安装器代理组名防注入 /
# 以 Tauri 相同方式（无窗口子进程 + UTF-8 标准输入）调用核心脚本：保存设置、uistate、运行时配置桥。

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$core = Join-Path $repo 'windows\claude-switch.ps1'
$install = Join-Path $repo 'windows\install.ps1'
foreach ($path in @($core, $install)) {
    if (-not (Test-Path -LiteralPath $path)) { throw "找不到脚本: $path" }
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-envguard-it-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($name in @('USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'TZ', 'ANTHROPIC_API_KEY', 'ENVGUARD_APP_VERSION')) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:USERPROFILE = Join-Path $root 'user'
$env:APPDATA = Join-Path $root 'appdata'
$env:LOCALAPPDATA = Join-Path $root 'localappdata'
$env:TEMP = Join-Path $root 'temp'
$env:TMP = $env:TEMP
[Environment]::SetEnvironmentVariable('TZ', $null)
[Environment]::SetEnvironmentVariable('ANTHROPIC_API_KEY', $null)
[Environment]::SetEnvironmentVariable('ENVGUARD_APP_VERSION', $null)
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

function Invoke-CoreProcess([string]$Script, [string[]]$Arguments, [string]$StdinText, [string]$UserProfile) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'powershell.exe'
    $psi.Arguments = '-NoProfile -ExecutionPolicy Bypass -File "' + $Script + '" ' + ($Arguments -join ' ')
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['USERPROFILE'] = $UserProfile
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrTask = $proc.StandardError.ReadToEndAsync()
    if ($StdinText) {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($StdinText)
        $proc.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    }
    $proc.StandardInput.Close()
    $stdout = New-Object System.IO.MemoryStream
    $proc.StandardOutput.BaseStream.CopyTo($stdout)
    $proc.WaitForExit()
    return [pscustomobject]@{
        Exit = $proc.ExitCode
        Out  = [System.Text.Encoding]::UTF8.GetString($stdout.ToArray())
        Err  = $stderrTask.Result
    }
}

function Read-UiState($Result) {
    $line = @(($Result.Out -split "`r?`n") | Where-Object { $_.TrimStart().StartsWith('{') }) | Select-Object -Last 1
    if (-not $line) { return $null }
    return ($line | ConvertFrom-Json)
}

$scriptErrorPreference = $ErrorActionPreference
try {
    . $core *> $null
    $ErrorActionPreference = 'Stop'

    # 1. 检查配置只接受上游字段：不能借配置把时区/地区这类硬性检查判为通过
    foreach ($json in @(
        '{"disabled":[],"allowOptionalFailures":false,"logging":true,"tzMode":"process"}',
        '{"disabled":[],"allowOptionalFailures":false,"logging":true,"relaxLocale":true}'
    )) {
        $rejected = $false
        try { Set-CheckConfigFromJson $json } catch { $rejected = ($_.Exception.Message -match '未知字段') }
        Assert "拒绝已移除的放宽字段 $json" $rejected
    }
    Assert '被拒绝的配置不落盘' (-not (Test-Path -LiteralPath $CHECK_CONFIG))
    $defaultKeys = (@((Get-CheckConfig).Keys) | Sort-Object) -join ','
    Assert '默认检查配置只含上游字段' ($defaultKeys -eq 'allowOptionalFailures,disabled,logging') "实际=$defaultKeys"

    # 2. 时区与地区只按系统设置判定；即使检查配置里混入旧的放宽字段也不能判为通过
    $detailAst = ([Management.Automation.Language.Parser]::ParseFile($core, [ref]$null, [ref]$null)).Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-CheckDetail'
    }, $true)
    function Get-CheckStatement([string]$Condition) {
        $statement = $detailAst.Body.EndBlock.Statements | Where-Object {
            $_ -is [Management.Automation.Language.IfStatementAst] -and $_.Clauses[0].Item1.Extent.Text -eq $Condition
        }
        if (-not $statement) { throw "找不到生产检查语句: $Condition" }
        return [scriptblock]::Create($statement.Extent.Text)
    }
    $tzCheck = Get-CheckStatement '$tz -eq $TZ_TARGET'
    $localeCheck = Get-CheckStatement '$cult -eq $LOCALE_TARGET'
    $script:CheckConfig = @{ disabled = @(); allowOptionalFailures = $true; logging = $true; tzMode = 'process'; relaxLocale = $true }
    $TZ_TARGET = 'Pacific Standard Time'
    $LOCALE_TARGET = 'en-US'
    $tz = 'China Standard Time'
    Assert '系统时区不符即失败' ((@(& $tzCheck) -join '') -eq '时区=fail:China Standard Time')
    $tz = 'Pacific Standard Time'
    Assert '系统时区一致才通过' ((@(& $tzCheck) -join '') -eq '时区=ok')
    $cult = 'zh-CN'
    Assert '区域格式不符即失败' ((@(& $localeCheck) -join '') -eq '地区=fail:zh-CN(期望en-US)')
    $cult = 'en-US'
    Assert '区域格式一致才通过' ((@(& $localeCheck) -join '') -eq '地区=ok:en-US')
    $script:CheckConfig = $null

    # 3. 受检启动：桌面版不注入也不声称注入 TZ；CLI 注入目标 IANA 时区；门禁不过不启动
    $COUNTRY_TARGET = 'US'
    $IANA_TZ_OVERRIDE = ''
    $script:checkPasses = $true
    $script:proxyUp = $true
    $script:desktopLaunches = @()
    $script:started = @()
    $script:elevatedExit = 0
    function Invoke-Check {
        if ($script:checkPasses) { 'OK:环境检查通过' } else { $script:exitCode = 1; 'FAIL:时区' }
    }
    function Test-ProxyAlive { $script:proxyUp }
    function Test-TunUp { $script:proxyUp }
    function Start-ClaudeApp { $script:desktopLaunches += ,[string]$env:TZ; $true }
    function Get-Command {
        [CmdletBinding()]
        param([string]$Name)
        if ($Name -eq 'claude') { [pscustomobject]@{ Source = 'C:\Tools\claude.exe' } }
    }
    function Start-Process {
        [CmdletBinding()]
        param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, [switch]$NoNewWindow, [string]$Verb)
        $call = @{ FilePath = [string]$FilePath; Args = @($ArgumentList); Verb = $Verb; Script = '' }
        if ((@($ArgumentList) -join ' ') -match '-File "([^"]+)"') { $call.Script = [System.IO.File]::ReadAllText($Matches[1]) }
        $script:started += ,$call
        if ($PassThru) { return [pscustomobject]@{ ExitCode = $script:elevatedExit } }
    }

    $script:exitCode = 0
    $out = (@(Invoke-SafeLaunch 'desktop') -join "`n")
    Assert '桌面版通过门禁后启动' ($script:exitCode -eq 0 -and $out -match '^OK:' -and $script:desktopLaunches.Count -eq 1) "输出=$out"
    Assert '桌面版启动时不注入 TZ' ($script:desktopLaunches.Count -eq 1 -and $script:desktopLaunches[0] -eq '')
    Assert '桌面版输出不声称注入 TZ' ($out -notmatch 'TZ') "输出=$out"
    Assert '启动后不残留 TZ 环境变量' ([string]::IsNullOrEmpty($env:TZ))

    $script:exitCode = 0
    $script:checkPasses = $false
    $script:desktopLaunches = @()
    $out = (@(Invoke-SafeLaunch 'desktop') -join "`n")
    Assert '检查未通过时阻断桌面启动' ($script:exitCode -eq 1 -and $out -match '(?m)^FAIL:' -and $script:desktopLaunches.Count -eq 0) "输出=$out"
    $script:checkPasses = $true

    $script:exitCode = 0
    $script:proxyUp = $false
    $out = (@(Invoke-SafeLaunch 'desktop') -join "`n")
    Assert '代理路径离线时阻断启动' ($script:exitCode -eq 1 -and $out -match '代理路径未就绪' -and $script:desktopLaunches.Count -eq 0) "输出=$out"
    $script:proxyUp = $true

    $script:exitCode = 0
    $script:started = @()
    $out = (@(Invoke-SafeLaunch 'cli') -join "`n")
    $cliCall = @($script:started | Where-Object { $_.FilePath -eq 'powershell.exe' }) | Select-Object -First 1
    $cliCommand = if ($cliCall) { [string]$cliCall.Args[-1] } else { '' }
    Assert 'CLI 在独立终端启动且不注入冗余 TZ' ($script:exitCode -eq 0 -and (-not $cliCommand.Contains('$env:TZ')) -and $cliCommand.Contains("& 'C:\Tools\claude.exe'")) "命令=$cliCommand"
    Assert 'CLI 输出不声称注入 TZ' ($out -match '^OK:' -and $out -notmatch 'TZ=') "输出=$out"

    $script:exitCode = 0
    $script:started = @()
    $env:ANTHROPIC_API_KEY = 'sk-isolated-test'
    $out = (@(Invoke-SafeLaunch 'cli') -join "`n")
    [Environment]::SetEnvironmentVariable('ANTHROPIC_API_KEY', $null)
    Assert '认证环境变量覆盖时拒绝启动 CLI' ($script:exitCode -eq 1 -and $out -match 'ANTHROPIC_API_KEY' -and $script:started.Count -eq 0) "输出=$out"

    $script:exitCode = 0
    $out = (@(Invoke-SafeLaunch 'switch') -join "`n")
    Assert '非法启动目标被拒绝' ($script:exitCode -eq 1 -and $out -match '^FAIL:') "输出=$out"

    # 4. 退出护盾：桌面版与本会话的 npm/bun CLI 必须全部退出才撤防；其他会话与无关 node 进程不受影响
    $script:procs = @(
        [pscustomobject]@{ Id = 4101; Name = 'node'; Session = 1; CommandLine = '"C:\nodejs\node.exe" "C:\npm\node_modules\@anthropic-ai\claude-code\cli.js"' },
        [pscustomobject]@{ Id = 4102; Name = 'bun'; Session = 1; CommandLine = 'bun C:/work/node_modules/@anthropic-ai/claude-code/cli.js' },
        [pscustomobject]@{ Id = 4103; Name = 'node'; Session = 1; CommandLine = 'node vite.js' },
        [pscustomobject]@{ Id = 4104; Name = 'node'; Session = 2; CommandLine = 'node C:\other\@anthropic-ai\claude-code\cli.js' }
    )
    $script:stubbornIds = @()
    $script:stopped = @()
    $script:taskStops = 0
    $script:desktopStopOk = $true
    function Stop-ClaudeApp { $script:desktopStopOk }
    function Get-Process {
        [CmdletBinding()]
        param([string[]]$Name, [int]$Id)
        if ($PSBoundParameters.ContainsKey('Id')) { return [pscustomobject]@{ Id = $Id; SessionId = 1; ProcessName = 'powershell' } }
        @($script:procs | Where-Object { $Name -contains $_.Name } | ForEach-Object {
            [pscustomobject]@{ Id = $_.Id; SessionId = $_.Session; ProcessName = $_.Name }
        })
    }
    function Get-CimInstance {
        [CmdletBinding()]
        param([string]$ClassName, [string]$Filter)
        @($script:procs | ForEach-Object { [pscustomobject]@{ ProcessId = $_.Id; Name = "$($_.Name).exe"; CommandLine = $_.CommandLine } })
    }
    function Stop-Process {
        [CmdletBinding()]
        param([int]$Id, [switch]$Force)
        $script:stopped += $Id
        if ($script:stubbornIds -notcontains $Id) { $script:procs = @($script:procs | Where-Object { $_.Id -ne $Id }) }
    }
    function Stop-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName)
        $script:taskStops++
    }
    function Start-Sleep { }
    function Get-TimeZone { [pscustomobject]@{ Id = 'Pacific Standard Time' } }
    [System.IO.File]::WriteAllText((Join-Path $BASE 'home-tz.txt'), 'China Standard Time', (New-Object System.Text.UTF8Encoding($false)))

    $script:stubbornIds = @(4101)
    $script:exitCode = 0
    $script:started = @()
    $out = (@(Invoke-ShieldRestore) -join "`n")
    Assert 'CLI 未退出时拒绝撤防' ($script:exitCode -eq 1 -and $out -match 'Claude Code CLI 未能退出') "输出=$out"
    Assert '拒绝撤防时不停保镖也不提权' ($script:taskStops -eq 0 -and $script:started.Count -eq 0)

    $script:stubbornIds = @()
    $script:exitCode = 0
    $script:started = @()
    $out = (@(Invoke-ShieldRestore) -join "`n")
    $elevated = @($script:started | Where-Object { $_.Verb -eq 'RunAs' })
    Assert '本会话 CLI 全部退出后完成撤防(无 IPv6 快照时给出 WARN 并整体启用)' ($script:exitCode -eq 0 -and $out -match '(?m)^WARN:未找到护盾开启前的网卡 IPv6 记录' -and $out -match '(?m)^OK:') "输出=$out"
    Assert '只结束本会话的 Claude Code CLI' (($script:stopped -notcontains 4103) -and ($script:stopped -notcontains 4104) -and ((@($script:procs | ForEach-Object { $_.Id }) -join ',') -eq '4103,4104'))
    Assert '提权脚本恢复 IPv6 并还原原时区' ($elevated.Count -eq 1 -and $elevated[0].Script -match 'Enable-NetAdapterBinding' -and $elevated[0].Script.Contains("Set-TimeZone -Id 'China Standard Time'"))
    Assert '撤防时停止保镖任务' ($script:taskStops -ge 1)

    # 4b. 按快照逐块恢复各网卡 IPv6 + 无 home-tz.txt 时明确告警且不乱改时区
    $script:MockBindings = @(
        [pscustomobject]@{ Name = '以太网'; Enabled = $true },
        [pscustomobject]@{ Name = 'Tailscale'; Enabled = $false }
    )
    function Get-NetAdapterBinding {
        [CmdletBinding()]
        param([string]$ComponentID)
        $script:MockBindings
    }
    $v6StatePath = Join-Path $BASE 'ipv6-state.json'
    Remove-Item -LiteralPath $v6StatePath -Force -ErrorAction SilentlyContinue
    Save-HomeIpv6State
    Assert 'Save-HomeIpv6State 生成网卡状态快照' (Test-Path -LiteralPath $v6StatePath)
    $script:MockBindings = @(
        [pscustomobject]@{ Name = '以太网'; Enabled = $false },
        [pscustomobject]@{ Name = 'Tailscale'; Enabled = $false }
    )
    $firstSnapHash = (Get-FileHash -LiteralPath $v6StatePath).Hash
    Save-HomeIpv6State
    Assert '全关闭状态下不覆盖已有 IPv6 快照' ((Get-FileHash -LiteralPath $v6StatePath).Hash -eq $firstSnapHash)
    Remove-Item -LiteralPath (Join-Path $BASE 'home-tz.txt') -Force -ErrorAction SilentlyContinue
    $script:exitCode = 0
    $script:started = @()
    $out = (@(Invoke-ShieldRestore) -join "`n")
    $elevated = @($script:started | Where-Object { $_.Verb -eq 'RunAs' })
    Assert '有 IPv6 快照但无原时区记录时给出时区 WARN 且成功撤防' ($script:exitCode -eq 0 -and $out -match '(?m)^WARN:未找到原时区记录' -and $out -match '(?m)^OK:.*恢复 1 块网卡.*其余 1 块保持原关闭') "输出=$out"
    Assert '按快照仅恢复原启用网卡并保持原关闭网卡,且不调用 Set-TimeZone' ($elevated.Count -eq 1 -and $elevated[0].Script.Contains("Enable-NetAdapterBinding -Name '以太网'") -and $elevated[0].Script.Contains("Disable-NetAdapterBinding -Name 'Tailscale'") -and (-not $elevated[0].Script.Contains('Set-TimeZone'))) "脚本=$($elevated[0].Script)"
    Assert '成功按快照恢复后清理 ipv6-state.json 以便下次护盾重新采样' (-not (Test-Path -LiteralPath $v6StatePath))

    $script:desktopStopOk = $false
    $script:exitCode = 0
    $script:started = @()
    $script:taskStops = 0
    $out = (@(Invoke-ShieldRestore) -join "`n")
    Assert '桌面版未退出时拒绝撤防' ($script:exitCode -eq 1 -and $out -match '桌面版未能退出' -and $script:started.Count -eq 0 -and $script:taskStops -eq 0) "输出=$out"
    $script:desktopStopOk = $true

    # 5. 保镖健康度：识别计划任务默认的 72h/电池限制；以心跳或运行态判断在线
    $script:task = $null
    function Get-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName)
        $script:task
    }
    $legacySettings = [pscustomobject]@{ ExecutionTimeLimit = 'PT72H'; StopIfGoingOnBatteries = $true; DisallowStartIfOnBatteries = $true }
    $fixedSettings = [pscustomobject]@{ ExecutionTimeLimit = 'PT0S'; StopIfGoingOnBatteries = $false; DisallowStartIfOnBatteries = $false }
    $heartbeat = Join-Path $BASE 'bodyguard-heartbeat.txt'
    $health = Get-BodyguardHealth
    Assert '未注册保镖判为失败' ($health.state -eq 'fail' -and -not $health.installed)
    $script:task = [pscustomobject]@{ State = 'Running'; Settings = $legacySettings }
    $health = Get-BodyguardHealth
    Assert '默认 72h/电池策略判为警告' ($health.state -eq 'warn' -and $health.legacyPolicy)
    $script:task = [pscustomobject]@{ State = 'Running'; Settings = $fixedSettings }
    Assert '无限制且运行中判为通过' ((Get-BodyguardHealth).state -eq 'ok')
    $script:task = [pscustomobject]@{ State = 'Ready'; Settings = $fixedSettings }
    Assert '已注册但未运行判为失败' ((Get-BodyguardHealth).state -eq 'fail')
    [System.IO.File]::WriteAllText($heartbeat, "$PID $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())", [System.Text.Encoding]::ASCII)
    Assert '新鲜心跳视为在线' ((Get-BodyguardHealth).state -eq 'ok')
    [System.IO.File]::WriteAllText($heartbeat, "$PID $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - 120)", [System.Text.Encoding]::ASCII)
    Assert '过期心跳不算在线' ((Get-BodyguardHealth).state -eq 'fail')

    # 6. 安装器写入代理组名：恶意组名不能闭合字面量或注入语句
    $installAst = [Management.Automation.Language.Parser]::ParseFile($install, [ref]$null, [ref]$null)
    $escapeStatement = $installAst.Find({ param($node)
        $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$grpEsc'
    }, $true)
    $writeStatement = $installAst.Find({ param($node)
        $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$core' -and $node.Right.Extent.Text -match 'PROXY_GROUP'
    }, $true)
    if (-not $escapeStatement -or -not $writeStatement) { throw '找不到安装器的代理组名写入语句' }
    $writeGroup = [scriptblock]::Create("param(`$grp, `$core)`n" + $escapeStatement.Extent.Text + "`n" + $writeStatement.Extent.Text + "`n`$core")
    $template = '$PROXY_GROUP = "GLOBAL"                # Clash 代理组名'
    foreach ($grp in @('a";$global:EG_INJECTED="1";"b', 'a`$(Set-Variable EG_INJECTED 1 -Scope Global)', 'x$env:USERNAME')) {
        $line = [string](& $writeGroup $grp $template)
        $parseErrors = $null
        $lineAst = [Management.Automation.Language.Parser]::ParseInput($line, [ref]$null, [ref]$parseErrors)
        $dynamic = @($lineAst.FindAll({ param($node)
            $node -is [Management.Automation.Language.ExpandableStringExpressionAst] -or
            $node -is [Management.Automation.Language.SubExpressionAst] -or
            $node -is [Management.Automation.Language.CommandAst]
        }, $true))
        $single = ($parseErrors.Count -eq 0 -and @($lineAst.EndBlock.Statements).Count -eq 1 -and $dynamic.Count -eq 0)
        if ($single) { & { Invoke-Expression $line } }
        Assert "代理组名写入为单一常量赋值: $grp" ($single -and $null -eq $global:EG_INJECTED) "结果=$line"
    }

    # 7. 与 Tauri 相同的调用方式：无窗口子进程、UTF-8 标准输入、按 UTF-8 解码标准输出
    $profileA = Join-Path $root 'user-a'
    New-Item -ItemType Directory -Path $profileA -Force | Out-Null
    $payload = '{"disabled":["IP一致","客户端语言","ASN属性","生态可达性"],"allowOptionalFailures":false,"logging":true}'
    $r = Invoke-CoreProcess $core @('set-check-config', '--stdin') $payload $profileA
    $savedPath = Join-Path $profileA '.claude-switcher\checks.json'
    $savedDisabled = if (Test-Path -LiteralPath $savedPath) { (@((Get-Content -LiteralPath $savedPath -Raw -Encoding UTF8 | ConvertFrom-Json).disabled) -join ',') } else { '' }
    Assert '无窗口子进程经 stdin 保存中文检查项' ($r.Exit -eq 0 -and $r.Out -match 'OK:检查设置已保存' -and $savedDisabled -eq 'IP一致,客户端语言,ASN属性,生态可达性') "退出码=$($r.Exit) 已保存=$savedDisabled 错误=$($r.Err)"
    $r = Invoke-CoreProcess $core @('get-check-config') '' $profileA
    $readBack = (@(($r.Out | ConvertFrom-Json).disabled) -join ',')
    Assert 'get-check-config 读回同一组中文检查项' ($r.Exit -eq 0 -and $readBack -eq $savedDisabled) "输出=$($r.Out)"

    $profileB = Join-Path $root 'user-b'
    $baseB = Join-Path $profileB '.claude-switcher'
    New-Item -ItemType Directory -Path $baseB -Force | Out-Null
    $names = @('代理内核', 'TUN网卡', '出口IP', 'IP一致', '时区', '地区', 'IPv6', 'DNS', 'WebRTC', '保镖', '规则置顶', '客户端语言', 'ASN属性', '生态可达性')
    [System.IO.File]::WriteAllLines((Join-Path $baseB 'last-check.txt'), [string[]]@($names | ForEach-Object { "$_=ok:测试" }), (New-Object System.Text.UTF8Encoding($true)))
    $r = Invoke-CoreProcess $core @('uistate') '' $profileB
    $state = Read-UiState $r
    $stateNames = if ($state) { (@($state.checks | ForEach-Object { $_.name }) -join ',') } else { '' }
    Assert 'uistate 返回完整 14 项且名称无乱码' ($r.Exit -eq 0 -and $stateNames -eq ($names -join ',')) "输出=$($r.Out) 错误=$($r.Err)"
    $configFields = if ($state) { (@($state.check_config.PSObject.Properties.Name) | Sort-Object) -join ',' } else { '' }
    Assert 'uistate 的检查配置只含上游字段' ($configFields -eq 'allowOptionalFailures,disabled,logging') "实际=$configFields"
    Assert '只读 uistate 不创建账号目录' (-not (Test-Path -LiteralPath (Join-Path $baseB 'accounts')))

    $profileC = Join-Path $root 'user-c'
    $baseC = Join-Path $profileC '.claude-switcher'
    $bundleDir = Join-Path $root 'bundle\resources'
    New-Item -ItemType Directory -Path $baseC, $bundleDir -Force | Out-Null
    $bundled = Join-Path $bundleDir 'claude-switch.ps1'
    Copy-Item -LiteralPath $core -Destination $bundled
    $installedCore = Join-Path $baseC 'claude-switch.ps1'
    $installedText = @(
        '$PROXY_MODE = "flclash"            # 代理客户端',
        '$COUNTRY_TARGET = "JP"                 # 目标出口国家',
        '$TZ_TARGET = "Tokyo Standard Time"   # Windows 时区ID',
        '$LOCALE_TARGET = "ja-JP"               # 地区格式目标',
        '$PROXY_GROUP = "Proxy"                # Clash 代理组名'
    ) -join "`n"
    [System.IO.File]::WriteAllText($installedCore, $installedText, (New-Object System.Text.UTF8Encoding($true)))
    $installedHash = (Get-FileHash -LiteralPath $installedCore).Hash
    $state = Read-UiState (Invoke-CoreProcess $bundled @('uistate') '' $profileC)
    Assert '打包脚本只读继承已安装脚本的锁定配置' ($state -and $state.proxy_mode -eq 'flclash' -and $state.country_target -eq 'JP' -and $state.tz_target -eq 'Tokyo Standard Time' -and $state.iana_tz_target -eq 'Asia/Tokyo' -and $state.locale_target -eq 'ja-JP')
    $r = Invoke-CoreProcess $bundled @('set-client', 'clash-verge') '' $profileC
    $runtimePath = Join-Path $baseC 'envguard-config.json'
    $runtime = if (Test-Path -LiteralPath $runtimePath) { Get-Content -LiteralPath $runtimePath -Raw -Encoding UTF8 | ConvertFrom-Json } else { $null }
    $modeFile = Join-Path $baseC 'proxy-mode.txt'
    $modeText = if (Test-Path -LiteralPath $modeFile) { (Get-Content -LiteralPath $modeFile -Raw).Trim() } else { '' }
    Assert '打包脚本切换客户端写入运行时配置与 proxy-mode.txt' ($r.Exit -eq 0 -and $runtime -and $runtime.proxy_mode -eq 'clash-verge' -and $modeText -eq 'clash-verge') "输出=$($r.Out) 错误=$($r.Err)"
    Assert '打包脚本不改写已安装脚本' ((Get-FileHash -LiteralPath $installedCore).Hash -eq $installedHash)
    $state = Read-UiState (Invoke-CoreProcess $bundled @('uistate') '' $profileC)
    Assert '运行时配置覆盖已安装配置' ($state -and $state.proxy_mode -eq 'clash-verge' -and $state.country_target -eq 'JP')
    $badRuntime = [ordered]@{ proxy_mode = 'clash-verge'; country_target = 'usa'; iana_tz_target = '../../evil'; proxy_group = 'x";calc;"' }
    [System.IO.File]::WriteAllText($runtimePath, ($badRuntime | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
    $state = Read-UiState (Invoke-CoreProcess $bundled @('uistate') '' $profileC)
    Assert '运行时配置中的非法国家与时区被忽略' ($state -and $state.country_target -eq 'JP' -and $state.iana_tz_target -eq 'Asia/Tokyo')

    # 诊断导出：版本号取 EnvGuard 自身，检查结果与日志里的 IP 不进诊断包
    $profileD = Join-Path $root 'user-d'
    $baseD = Join-Path $profileD '.claude-switcher'
    New-Item -ItemType Directory -Path (Join-Path $baseD 'logs') -Force | Out-Null
    [System.IO.File]::WriteAllLines((Join-Path $baseD 'last-check.txt'), [string[]]@('出口IP=ok:203.0.113.9', '时区=ok'), (New-Object System.Text.UTF8Encoding($true)))
    [System.IO.File]::WriteAllText((Join-Path $baseD 'logs\client.log'), "2026-11-11T00:00:00Z version=2.0.0 command=uistate exit=0 duration_ms=12`n2026-11-11T00:00:01Z version=2.0.0 command=uistate exit=0 duration_ms=12 出口IP=203.0.113.9`n", (New-Object System.Text.UTF8Encoding($false)))
    $zip = Join-Path $root 'diagnostics.zip'
    $env:ENVGUARD_APP_VERSION = '2.0.0'
    $r = Invoke-CoreProcess $core @('export-diagnostics', "`"$zip`"") '' $profileD
    [Environment]::SetEnvironmentVariable('ENVGUARD_APP_VERSION', $null)
    $unzipped = Join-Path $root 'diagnostics'
    if (Test-Path -LiteralPath $zip) { Expand-Archive -LiteralPath $zip -DestinationPath $unzipped -Force }
    $packed = if (Test-Path -LiteralPath $unzipped) { (Get-ChildItem -LiteralPath $unzipped -File | ForEach-Object { [System.IO.File]::ReadAllText($_.FullName) }) -join "`n" } else { '' }
    $envJson = if (Test-Path -LiteralPath (Join-Path $unzipped 'environment.json')) { Get-Content -LiteralPath (Join-Path $unzipped 'environment.json') -Raw | ConvertFrom-Json } else { $null }
    Assert '诊断包导出成功且版本取 EnvGuard 自身' ($r.Exit -eq 0 -and $envJson -and $envJson.app_version -eq '2.0.0') "输出=$($r.Out) 错误=$($r.Err)"
    Assert '诊断包不含检查结果与日志里的 IP' ($packed -and $packed -notmatch '203\.0\.113\.9' -and $packed -match 'command=uistate')

    # 版本号与上游更新检查（订阅打桩）
    $env:ENVGUARD_APP_VERSION = '2.0.0'
    Assert 'EnvGuard 调用时版本取应用自身' ((Get-AppVersion) -eq '2.0.0')
    $env:ENVGUARD_APP_VERSION = 'x;Remove-Item'
    Assert '非法版本号不被采用' ((Get-AppVersion) -eq 'unknown')
    [Environment]::SetEnvironmentVariable('ENVGUARD_APP_VERSION', $null)
    function New-AtomFeed([string[]]$Shas) {
        $entries = foreach ($sha in $Shas) {
            $title = if ($sha.StartsWith('a')) { 'fix(windows): sample fix' } else { 'docs: sample note' }
            "<entry><id>tag:github.com,2008:Grit::Commit/$sha</id><updated>2026-11-12T01:00:00Z</updated><title>$title</title></entry>"
        }
        return '<?xml version="1.0" encoding="UTF-8"?><feed xmlns="http://www.w3.org/2005/Atom">' + ($entries -join '') + '</feed>'
    }
    $script:feed = ''
    $script:feedFails = $false
    function Invoke-WebRequest {
        [CmdletBinding()]
        param($Uri, $UserAgent, [switch]$UseBasicParsing, $TimeoutSec)
        if ($script:feedFails) { throw 'offline' }
        [pscustomobject]@{ Content = $script:feed }
    }
    $script:feed = New-AtomFeed @($UPSTREAM_MERGED, ('b' * 40))
    Assert '上游没有新提交时报告已同步' ((@(Invoke-UpstreamStatus))[0] -match '^OK:.*暂无新提交')
    $script:feed = New-AtomFeed @(('a' * 40), ('c' * 40), $UPSTREAM_MERGED)
    $out = @(Invoke-UpstreamStatus)
    Assert '列出未合并的上游提交并统计 Windows 相关' ($out[0] -match '^WARN:上游 main 有 2 个新提交.*其中 1 个' -and ($out -join "`n") -match 'aaaaaaa fix\(windows\)') "输出=$($out -join ' | ')"
    $script:feed = New-AtomFeed @(('d' * 40), ('e' * 40))
    Assert '订阅里找不到已合并提交时只报下限' ((@(Invoke-UpstreamStatus))[0] -match '至少有 2 个新提交')
    $script:feedFails = $true
    $script:exitCode = 0
    $out = @(Invoke-UpstreamStatus)
    Assert '读不到订阅时返回 FAIL' ($out[0] -match '^FAIL:' -and $script:exitCode -eq 1)
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
