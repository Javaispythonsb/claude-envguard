# 严谨性与安全门禁回归测试（覆盖 TUN 收敛 / 硬性门禁拦截 warn / 规则置顶非 Mihomo 分支 / 电脑端 DNS 泄漏校验）
#
# 只用临时目录与打桩 cmdlet，不接触真实网络、网卡、进程或用户配置。
# 在 Windows PowerShell 5.1 下运行：
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-rigor-integration.ps1

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$core = Join-Path $repo 'windows\claude-switch.ps1'
$bodyguard = Join-Path $repo 'windows\claude-bodyguard.ps1'
foreach ($p in @($core, $bodyguard)) {
    if (-not (Test-Path -LiteralPath $p)) { throw "找不到脚本: $p" }
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-rigor-it-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($name in @('USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:USERPROFILE = Join-Path $root 'user'
$env:APPDATA = Join-Path $root 'appdata'
$env:LOCALAPPDATA = Join-Path $root 'localappdata'
$env:TEMP = Join-Path $root 'temp'
$env:TMP = $env:TEMP
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

    # 从保镖脚本提取 Get-TunStatus，验证两处实现语义完全一致
    $bgAst = [Management.Automation.Language.Parser]::ParseFile($bodyguard, [ref]$null, [ref]$null)
    $bgTunFn = $bgAst.Find({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Get-TunStatus' }, $true)
    if (-not $bgTunFn) { throw '找不到保镖 Get-TunStatus' }
    $bodyguardTun = [scriptblock]::Create($bgTunFn.Body.Extent.Text.TrimStart('{').TrimEnd('}'))

    # 1. TUN 判定收敛：Tailscale 常驻在线时不能掩盖 Clash / sing-box / WireGuard TUN 掉线
    $script:MockIps = @()
    $script:MockAdapters = @()
    $script:MockProcs = @()
    $script:MockServices = @()
    function Get-NetIPAddress {
        [CmdletBinding()]
        param([string]$AddressFamily)
        $script:MockIps
    }
    function Get-NetAdapter {
        [CmdletBinding()]
        param()
        $script:MockAdapters
    }
    function Get-Process {
        [CmdletBinding()]
        param([string[]]$Name)
        @($script:MockProcs | Where-Object { $Name -contains $_.Name })
    }
    function Get-Service {
        [CmdletBinding()]
        param([string]$Name)
        $script:MockServices
    }

    $tailscaleOnly = @([pscustomobject]@{ Name = 'Tailscale'; InterfaceDescription = 'Tailscale Tunnel'; Status = 'Up'; ifIndex = 21 })
    $script:MockAdapters = $tailscaleOnly
    $script:MockIps = @()

    foreach ($mode in @('clash-verge', 'flclash', 'sing-box', 'wireguard')) {
        $script:PROXY_MODE = $mode
        $cTun = Get-TunStatus
        $bTun = & $bodyguardTun
        Assert "仅 Tailscale 在线时 $mode 核心判 TUN 离线" (-not $cTun.up) "实际=$($cTun.detail)"
        Assert "仅 Tailscale 在线时 $mode 保镖判 TUN 离线" (-not $bTun.up) "实际=$($bTun.detail)"
    }

    $script:PROXY_MODE = 'clash-verge'
    $script:MockIps = @([pscustomobject]@{ IPAddress = '198.19.0.1'; AddressState = 'Preferred' })
    Assert '198.19.x.x (198.18.0.0/15 后半段) 识别为 Fake-IP TUN 在线' ((Get-TunStatus).up -and (& $bodyguardTun).up)
    $script:MockIps = @([pscustomobject]@{ IPAddress = '198.18.0.1'; AddressState = 'Tentative' })
    Assert '非 Preferred 的残留地址不算 TUN 在线' ((-not (Get-TunStatus).up) -and (-not (& $bodyguardTun).up))
    $script:MockIps = @()
    $script:MockAdapters = $tailscaleOnly + @([pscustomobject]@{ Name = 'Mihomo'; InterfaceDescription = 'Meta Tunnel'; Status = 'Up'; ifIndex = 15 })
    Assert 'Mihomo 网卡在线时 clash-verge 判在线' ((Get-TunStatus).up -and (& $bodyguardTun).up)

    $script:PROXY_MODE = 'auto'
    $script:MockAdapters = $tailscaleOnly
    $script:MockProcs = @([pscustomobject]@{ Name = 'verge-mihomo' })
    Assert 'auto 模式下检测到 Mihomo 进程时拒绝把 Tailscale 当 TUN' ((-not (Get-TunStatus).up) -and (-not (& $bodyguardTun).up))
    $script:MockProcs = @()
    $script:PROXY_MODE = 'other'
    Assert 'other 模式保留通用虚拟网卡回退' ((Get-TunStatus).up -and (& $bodyguardTun).up)

    # 2. 启动门禁：6 项硬性防漏检查出现 warn/skip 必须阻断；8 项可选检查(含时区/地区) warn/skip 放行、fail 阻断
    $allChecks = @('代理内核','TUN网卡','出口IP','IP一致','时区','地区','IPv6','DNS','WebRTC','保镖','规则置顶','客户端语言','ASN属性','生态可达性')
    $script:FakeDetailLines = @()
    $realCheckDetail = ${function:Invoke-CheckDetail}
    function Invoke-CheckDetail { $script:FakeDetailLines }

    $script:FakeDetailLines = @($allChecks | ForEach-Object { "$_=ok" })
    $script:exitCode = 0
    $out = (@(Invoke-Check) -join "`n")
    Assert '14 项全 ok 时门禁放行' ($script:exitCode -eq 0 -and $out -eq 'OK:环境检查通过') "输出=$out"

    foreach ($hard in @('代理内核', 'TUN网卡', '出口IP', 'IPv6', '保镖', '规则置顶')) {
        $script:FakeDetailLines = @($allChecks | ForEach-Object { if ($_ -eq $hard) { "$_=warn:测试降级" } else { "$_=ok" } })
        $script:exitCode = 0
        $out = (@(Invoke-Check) -join "`n")
        Assert "硬性项[$hard]为 warn 时门禁阻断" ($script:exitCode -eq 1 -and $out -eq "FAIL:$hard") "输出=$out"
    }

    $script:FakeDetailLines = @($allChecks | ForEach-Object {
        if ($_ -in @('WebRTC', '客户端语言', 'ASN属性')) { "$_=warn:可选提示" }
        elseif ($_ -in @('时区', '地区', 'DNS')) { "$_=skip:用户已禁用" }
        else { "$_=ok" }
    })
    $script:exitCode = 0
    $out = (@(Invoke-Check) -join "`n")
    Assert '时区/地区关闭(skip)且 6 项硬性防漏全 ok 时门禁放行' ($script:exitCode -eq 0 -and $out -eq 'OK:环境检查通过') "输出=$out"

    $script:FakeDetailLines = @($allChecks | ForEach-Object { if ($_ -eq '时区') { '时区=fail:China Standard Time' } else { "$_=ok" } })
    $script:exitCode = 0
    $out = (@(Invoke-Check) -join "`n")
    Assert '时区开关打开且判 fail 时门禁仍阻断' ($script:exitCode -eq 1 -and $out -eq 'FAIL:时区') "输出=$out"

    # 同时核验 uistate.ready / uistate.needs_fix 与 Invoke-Check 门禁完全一致
    $lastCheckFile = Join-Path $BASE 'last-check.txt'
    [System.IO.File]::WriteAllLines($lastCheckFile, [string[]]@($allChecks | ForEach-Object { if ($_ -eq '保镖') { "$_=warn:旧策略" } else { "$_=ok" } }), (New-Object System.Text.UTF8Encoding($true)))
    $uiBlocked = (Invoke-UiState $false) | ConvertFrom-Json
    Assert '硬性项 warn 时 uistate.ready 为 false 且 needs_fix 为 true' ($uiBlocked.ready -eq $false -and $uiBlocked.needs_fix -eq $true)
    [System.IO.File]::WriteAllLines($lastCheckFile, [string[]]@($allChecks | ForEach-Object { if ($_ -in @('时区', '地区')) { "$_=skip:用户已禁用" } elseif ($_ -in @('客户端语言', 'ASN属性')) { "$_=warn:提示信息" } else { "$_=ok" } }), (New-Object System.Text.UTF8Encoding($true)))
    $uiReady = (Invoke-UiState $false) | ConvertFrom-Json
    Assert '时区与地区 skip 且仅含纯提示 warn 时 ready=true 且 needs_fix=false' ($uiReady.ready -eq $true -and $uiReady.needs_fix -eq $false)
    [System.IO.File]::WriteAllLines($lastCheckFile, [string[]]@($allChecks | ForEach-Object { if ($_ -eq 'DNS') { 'DNS=warn:非关键失败已忽略(经国内解析器)' } else { "$_=ok" } }), (New-Object System.Text.UTF8Encoding($true)))
    $uiSoftFail = (Invoke-UiState $false) | ConvertFrom-Json
    Assert '可修复可选项被降级为 warn 时 ready=true 但 needs_fix 仍为 true' ($uiSoftFail.ready -eq $true -and $uiSoftFail.needs_fix -eq $true)

    Set-Item -Path function:Invoke-CheckDetail -Value $realCheckDetail

    # 3. 规则置顶：direct 模式必须 fail；非 Mihomo 客户端用 Fake-IP / WireGuard 全局路由判定，不再误读旧 Verge 配置
    $script:MockDnsMap = @{}
    $script:MockRoutes = @()
    function Resolve-DnsName {
        [CmdletBinding()]
        param([string]$Name, [string]$Type)
        if ($script:MockDnsMap.ContainsKey($Name)) {
            return [pscustomobject]@{ IPAddress = $script:MockDnsMap[$Name] }
        }
        return $null
    }
    function Get-NetRoute {
        [CmdletBinding()]
        param([string]$DestinationPrefix)
        $script:MockRoutes
    }

    $script:PROXY_MODE = 'clash-verge'
    $realClashMode = ${function:Get-ClashMode}
    function Get-ClashMode { 'direct' }
    Assert 'Clash direct 模式判为 fail(流量直连)' ((Get-RulesStatus) -match '^规则置顶=fail:直连模式')
    Set-Item -Path function:Get-ClashMode -Value $realClashMode

    $script:PROXY_MODE = 'sing-box'
    $script:MockDnsMap = @{ 'claude.ai' = '198.18.0.10'; 'api.anthropic.com' = '198.19.0.11' }
    Assert 'sing-box Fake-IP 接管 Claude 域名时规则置顶=ok' ((Get-RulesStatus) -match '^规则置顶=ok:Fake-IP 已接管')
    $script:MockDnsMap = @{ 'claude.ai' = '160.79.104.10'; 'api.anthropic.com' = '160.79.104.11' }
    Assert 'sing-box 未开 Fake-IP 时规则置顶=warn(由硬性门禁拦截)' ((Get-RulesStatus) -match '^规则置顶=warn:')

    $script:PROXY_MODE = 'wireguard'
    $script:MockAdapters = @([pscustomobject]@{ Name = 'wg0'; InterfaceDescription = 'WireGuard Tunnel'; Status = 'Up'; ifIndex = 33 })
    $script:MockRoutes = @([pscustomobject]@{ InterfaceIndex = 33 })
    $wgStatus = Get-RulesStatus
    Assert 'WireGuard 存在 0.0.0.0/0 全局路由时规则置顶=ok' ($wgStatus -eq '规则置顶=ok:WireGuard 全局路由已接管') "实际=[$wgStatus]"
    $script:MockRoutes = @()
    Assert 'WireGuard 无全局默认路由时规则置顶=warn' ((Get-RulesStatus) -match '^规则置顶=warn:WireGuard')

    # 4. 电脑端 DNS 检查：核验解析器归属，拦截 Fake-IP 掩盖下的国内上游泄漏，路径失效零出网
    Assert 'Fake-IP 但上游解析器在 CN 时判 fail' ((Test-DnsResolverVerdict '198.18.0.2' $true @([pscustomobject]@{ ip = '223.5.5.5'; country = 'CN' }) $true).state -eq 'fail')
    Assert 'Fake-IP 且上游解析器全在海外时判 ok' ((Test-DnsResolverVerdict '198.19.0.5' $true @([pscustomobject]@{ ip = '8.8.8.8'; country = 'US' }) $true).state -eq 'ok')
    Assert '实 IP 解析且解析器在海外时判 ok(兼容 WireGuard 非 Fake-IP)' ((Test-DnsResolverVerdict '160.79.104.10' $false @([pscustomobject]@{ ip = '1.1.1.1'; country = 'US' }) $true).state -eq 'ok')
    Assert '检测服务不可达时 Fake-IP 降为 warn 而非盲判 ok' ((Test-DnsResolverVerdict '198.18.0.2' $true @() $false).state -eq 'warn')
    Assert '检测服务不可达且非 Fake-IP 时判 fail' ((Test-DnsResolverVerdict '160.79.104.10' $false @() $false).state -eq 'fail')

    $script:probeCount = 0
    function Test-ProxyPathAlive { $false }
    function Invoke-ProbeText { $script:probeCount++; return '' }
    $script:MockDnsMap = @{ 'claude.ai' = '198.18.0.2' }
    $dnsWhenDown = Test-DnsCheck
    Assert '代理路径失效时 DNS 检查不发网络探测且判 fail' ($dnsWhenDown.state -eq 'fail' -and $script:probeCount -eq 0) "结果=$($dnsWhenDown.detail)"
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
