# 审计并行化的隔离集成测试
#
# STUN 用本机回环上的假服务；预取用不联网的假后台任务；curl 全部打桩。不接触真实 Claude、Clash、账号或网络。
# 在 Windows PowerShell 5.1 下运行：
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-audit-parallel-integration.ps1
#
# 覆盖：STUN 响应解析 / 多个 STUN 探测共用一个超时窗口并按服务器顺序去重 /
# 预取结果命中即用、只消费一次、未命中回退同步探测 / ASN 检查语句直接使用预取结果 /
# 代理路径失效或检查被禁用时不发起预取 / 回收未消费的后台任务。

$ErrorActionPreference = 'Stop'

$core = Join-Path (Split-Path -Parent $PSScriptRoot) 'windows\claude-switch.ps1'
if (-not (Test-Path -LiteralPath $core)) { throw "找不到核心脚本: $core" }

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-parallel-it-' + [guid]::NewGuid().ToString('N'))
$savedEnv = @{}
foreach ($name in @('USERPROFILE', 'APPDATA', 'LOCALAPPDATA')) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:USERPROFILE = Join-Path $root 'user'
$env:APPDATA = Join-Path $root 'appdata'
$env:LOCALAPPDATA = Join-Path $root 'localappdata'
foreach ($d in @($env:USERPROFILE, $env:APPDATA, $env:LOCALAPPDATA)) {
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

function New-StunReply([byte[]]$Request, [string]$MappedIp, [switch]$Plain) {
    $cookie = [byte[]](0x21, 0x12, 0xA4, 0x42)
    $ipBytes = ([System.Net.IPAddress]::Parse($MappedIp)).GetAddressBytes()
    if ($Plain) {
        $attribute = [byte[]](0x00, 0x01, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00) + $ipBytes
    } else {
        $xored = [byte[]]@(for ($i = 0; $i -lt 4; $i++) { $ipBytes[$i] -bxor $cookie[$i] })
        $attribute = [byte[]](0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00) + $xored
    }
    return [byte[]](0x01, 0x01, 0x00, [byte]$attribute.Length) + $cookie + [byte[]]$Request[8..19] + $attribute
}

# 假 STUN 服务：$MappedIp 为空表示只收不回（模拟不支持 UDP 的节点）
$stunServer = {
    param($Udp, $MappedIp)
    $Udp.Client.ReceiveTimeout = 5000
    $remote = New-Object System.Net.IPEndPoint([System.Net.IPAddress]::Any, 0)
    $request = $Udp.Receive([ref]$remote)
    if (-not $MappedIp) { return }
    $cookie = [byte[]](0x21, 0x12, 0xA4, 0x42)
    $ipBytes = ([System.Net.IPAddress]::Parse($MappedIp)).GetAddressBytes()
    $xored = [byte[]]@(for ($i = 0; $i -lt 4; $i++) { $ipBytes[$i] -bxor $cookie[$i] })
    $attribute = [byte[]](0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00) + $xored
    $reply = [byte[]](0x01, 0x01, 0x00, [byte]$attribute.Length) + $cookie + [byte[]]$request[8..19] + $attribute
    [void]$Udp.Send($reply, $reply.Length, $remote)
}

$stunFakes = @()
$scriptErrorPreference = $ErrorActionPreference
try {
    . $core *> $null
    $ErrorActionPreference = 'Stop'

    # 1. STUN 响应解析
    $request = [byte[]](0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42) + [byte[]](1..12)
    Assert '解析 XOR-MAPPED-ADDRESS' ((ConvertFrom-StunResponse (New-StunReply $request '203.0.113.7')) -eq '203.0.113.7')
    Assert '解析 MAPPED-ADDRESS' ((ConvertFrom-StunResponse (New-StunReply $request '198.51.100.9' -Plain)) -eq '198.51.100.9')
    $truncated = [byte[]]((New-StunReply $request '203.0.113.7')[0..27])
    Assert '截断响应不返回地址' ($null -eq (ConvertFrom-StunResponse $truncated))

    # 2. 四个 STUN 探测并发：两个不回应也只等一个超时窗口，结果按服务器顺序去重
    $servers = @()
    foreach ($mapped in @('203.0.113.7', '', '203.0.113.7', '')) {
        $udp = New-Object System.Net.Sockets.UdpClient((New-Object System.Net.IPEndPoint([System.Net.IPAddress]::Loopback, 0)))
        $shell = [powershell]::Create()
        [void]$shell.AddScript($stunServer).AddArgument($udp).AddArgument($mapped)
        $stunFakes += @{ Udp = $udp; Shell = $shell; Handle = $shell.BeginInvoke() }
        $servers += ,@('127.0.0.1', $udp.Client.LocalEndPoint.Port)
    }
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $found = @(Get-StunPublicIPs $servers 1500)
    $elapsed = $clock.Elapsed.TotalSeconds
    Assert '多个 STUN 探测共用一个超时窗口' ($elapsed -ge 1.2 -and $elapsed -lt 2.5) "用时=$elapsed"
    Assert 'STUN 结果按服务器顺序去重' (($found -join ',') -eq '203.0.113.7') "结果=$($found -join ',')"
    Assert '全部无响应时返回空' (@(Get-StunPublicIPs @(,@('127.0.0.1', $stunFakes[1].Udp.Client.LocalEndPoint.Port)) 300).Count -eq 0)

    # 3. 预取缓存：命中即用且只消费一次，未命中回退同步探测
    $script:curlCalls = @{}
    function curl.exe {
        $url = [string]$args[-1]
        if (-not $script:curlCalls.ContainsKey($url)) { $script:curlCalls[$url] = 0 }
        $script:curlCalls[$url]++
        if ($url -like 'https://ipinfo.io/*') { return '{"ip":"198.51.100.24","org":"AS64512 Example Metro"}' }
        if ($url -like 'https://www.peeringdb.com/*') { return '{"data":[{"asn":64512,"info_type":"Content"}]}' }
        return '200'
    }
    $job = New-ProbePrefetchJob '' '@{ "code|https://claude.ai/" = "403" }' @('', '') '^code\|https://claude\.ai/$'
    $script:ProbePrefetch = @{ Jobs = @($job); Results = @{} }
    Assert '预取命中时直接返回且不发请求' ((Invoke-ProbeCode -Url 'https://claude.ai/') -eq '403' -and -not $script:curlCalls.ContainsKey('https://claude.ai/'))
    Assert '预取结果只消费一次' ((Invoke-ProbeCode -Url 'https://claude.ai/') -eq '200' -and $script:curlCalls['https://claude.ai/'] -eq 1)
    Assert '没有对应预取时走同步探测' ((Invoke-ProbeCode -Url 'https://api.anthropic.com/v1/models') -eq '200' -and $script:curlCalls['https://api.anthropic.com/v1/models'] -eq 1)
    $failing = New-ProbePrefetchJob '' 'throw "boom"' @('', '') '^code\|https://example\.test/$'
    $script:ProbePrefetch = @{ Jobs = @($failing); Results = @{} }
    Assert '后台任务失败时回退同步探测' ((Invoke-ProbeCode -Url 'https://example.test/') -eq '200' -and $script:curlCalls['https://example.test/'] -eq 1)

    # 4. ASN 检查语句直接使用预取的 ipinfo → peeringdb 结果
    $detailAst = ([Management.Automation.Language.Parser]::ParseFile($core, [ref]$null, [ref]$null)).Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-CheckDetail'
    }, $true)
    $asnStatement = $detailAst.Body.EndBlock.Statements | Where-Object {
        $_ -is [Management.Automation.Language.IfStatementAst] -and $_.Clauses[0].Item1.Extent.Text -eq "Test-CheckDisabled 'ASN属性'"
    }
    $asnCheck = [scriptblock]::Create($asnStatement.Extent.Text)
    function Test-ProxyAlive { return $script:pathAlive }
    function Test-TunUp { return $script:pathAlive }
    $script:pathAlive = $true
    $script:CheckConfig = @{ disabled = @(); allowOptionalFailures = $false; logging = $true }
    $e = @{ ip = '198.51.100.24'; loc = 'US'; skipped = $false }
    $script:curlCalls = @{}
    $asnJob = New-ProbePrefetchJob '' (@'
@{
    "text|https://ipinfo.io/198.51.100.24/json" = '{"ip":"198.51.100.24","org":"AS64512 Example Metro"}'
    "text|https://www.peeringdb.com/api/net?asn=64512&fields=asn,info_type" = '{"data":[{"asn":64512,"info_type":"Cable/DSL/ISP"}]}'
}
'@) @('', '') '^text\|https://(ipinfo\.io|www\.peeringdb\.com)/'
    $script:ProbePrefetch = @{ Jobs = @($asnJob); Results = @{} }
    $asn = @(& $asnCheck) -join ''
    Assert 'ASN 检查使用预取结果且不再发请求' ($asn -eq 'ASN属性=ok:ISP(AS64512 Example Metro);具体 IP 的住宅属性未验证' -and $script:curlCalls.Count -eq 0) "结果=$asn 请求=$($script:curlCalls.Keys -join ',')"
    Stop-ProbePrefetch

    # 5. 预取只在代理路径存活且检查未禁用时发起
    $script:created = @()
    $realJobFactory = ${function:New-ProbePrefetchJob}
    function New-ProbePrefetchJob([string]$Definitions, [string]$Body, [object[]]$Arguments, [string]$KeyPattern) {
        $script:created += $KeyPattern
        return @{ Shell = $null; Handle = $null; Pattern = $KeyPattern; Done = $true }
    }
    $script:pathAlive = $false
    Start-ProbePrefetch $e
    Assert '代理路径失效时不发起预取' ($null -eq $script:ProbePrefetch -and $script:created.Count -eq 0)
    $script:pathAlive = $true
    Start-ProbePrefetch @{ ip = ''; loc = ''; skipped = $true }
    Assert '出口探测被跳过时不发起预取' ($null -eq $script:ProbePrefetch -and $script:created.Count -eq 0)
    Start-ProbePrefetch $e
    Assert '路径存活时预取 DNS、ASN 与两个生态站点' ($script:created.Count -eq 4 -and $script:created[0] -match 'ipleak' -and $script:created[1] -match 'ipinfo' -and ($script:created -join ' ') -match 'claude\\\.ai' -and ($script:created -join ' ') -match 'api\\\.anthropic')
    Stop-ProbePrefetch
    $script:created = @()
    $script:CheckConfig = @{ disabled = @('DNS', 'ASN属性', '生态可达性'); allowOptionalFailures = $false; logging = $true }
    Start-ProbePrefetch $e
    Assert '检查被禁用时不预取对应探测' ($script:created.Count -eq 0)
    Stop-ProbePrefetch
    Set-Item -Path function:New-ProbePrefetchJob -Value $realJobFactory

    # 6. 回收：未消费的后台任务被中断，不会拖住审计结束
    $slow = New-ProbePrefetchJob '' 'Start-Sleep -Seconds 30; @{}' @('', '') '^code\|'
    $script:ProbePrefetch = @{ Jobs = @($slow); Results = @{} }
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    Stop-ProbePrefetch
    Assert '回收未消费的后台任务' ($null -eq $script:ProbePrefetch -and $slow.Done -and $clock.Elapsed.TotalSeconds -lt 5) "用时=$($clock.Elapsed.TotalSeconds)"
} finally {
    $ErrorActionPreference = $scriptErrorPreference
    foreach ($f in $stunFakes) {
        try { $f.Udp.Close() } catch {}
        try { $f.Shell.Dispose() } catch {}
    }
    foreach ($name in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name]) }
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    "FAILED: $failures 项"
    exit 1
}
'OK: 全部通过'
