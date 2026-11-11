# Clash Verge 命名管道控制接口的隔离集成测试
#
# 在测试进程里起假的 Mihomo 管道服务（随机哈希后缀，不会与真实管道重名），配合临时 Clash 配置目录；
# 不接触真实 Clash Verge、Claude、账号或网络。在 Windows PowerShell 5.1 下运行：
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-clash-pipe-integration.ps1
#
# 覆盖：配置原名优先 / 服务模式改名后按哈希后缀匹配 / 候选不唯一或不存在时回退 HTTP /
# 管道上的 HTTP 请求与解析、鉴权失败、非 JSON、超时 / 「规则置顶」经管道得出 ok 与 fail。

$ErrorActionPreference = 'Stop'

$core = Join-Path (Split-Path -Parent $PSScriptRoot) 'windows\claude-switch.ps1'
if (-not (Test-Path -LiteralPath $core)) { throw "找不到核心脚本: $core" }

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-pipe-it-' + [guid]::NewGuid().ToString('N'))
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

# 假管道服务：按顺序为每个连接回放一段原始 HTTP 响应（$null 表示收下请求但不回应），并记录收到的请求
$serverScript = {
    param($PipeName, $Responses, $Requests)
    foreach ($response in $Responses) {
        $server = New-Object System.IO.Pipes.NamedPipeServerStream($PipeName, [System.IO.Pipes.PipeDirection]::InOut, 1,
            [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous)
        try {
            $server.WaitForConnection()
            $buffer = New-Object byte[] 4096
            $request = ''
            while ($request -notmatch "`r`n`r`n") {
                $n = $server.Read($buffer, 0, $buffer.Length)
                if ($n -le 0) { break }
                $request += [System.Text.Encoding]::UTF8.GetString($buffer, 0, $n)
            }
            [void]$Requests.Add($request)
            if ($null -ne $response) {
                $bytes = [System.Text.Encoding]::UTF8.GetBytes([string]$response)
                $server.Write($bytes, 0, $bytes.Length)
                $server.Flush()
                $server.WaitForPipeDrain()
            } else {
                Start-Sleep -Seconds 3
            }
        } finally {
            $server.Dispose()
        }
    }
}

function Start-FakePipe([string]$Name, [object[]]$Responses) {
    $requests = [System.Collections.ArrayList]::Synchronized((New-Object System.Collections.ArrayList))
    $ps = [powershell]::Create()
    [void]$ps.AddScript($serverScript).AddArgument($Name).AddArgument($Responses).AddArgument($requests)
    $handle = $ps.BeginInvoke()
    for ($i = 0; $i -lt 100 -and -not (@(Get-NamedPipeNames) -contains $Name); $i++) { Start-Sleep -Milliseconds 50 }
    return @{ Name = $Name; Shell = $ps; Handle = $handle; Requests = $requests }
}

function Stop-FakePipe($Fake) {
    for ($i = 0; $i -lt 10 -and -not $Fake.Handle.IsCompleted; $i++) {
        try {
            $drain = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Fake.Name, [System.IO.Pipes.PipeDirection]::InOut)
            $drain.Connect(300)
            $bytes = [System.Text.Encoding]::ASCII.GetBytes("GET /drain HTTP/1.0`r`n`r`n")
            $drain.Write($bytes, 0, $bytes.Length)
            $drain.Dispose()
        } catch {}
        Start-Sleep -Milliseconds 100
    }
    $Fake.Shell.Dispose()
}

function New-HttpResponse([int]$Status, [string]$Body) {
    $length = [System.Text.Encoding]::UTF8.GetByteCount($Body)
    return "HTTP/1.0 $Status X`r`nContent-Type: application/json`r`nContent-Length: $length`r`n`r`n$Body"
}

function Write-Yaml([string]$Path, [string[]]$Lines) {
    [System.IO.File]::WriteAllText($Path, ($Lines -join "`n"), (New-Object System.Text.UTF8Encoding($false)))
}

$fakes = @()
$scriptErrorPreference = $ErrorActionPreference
try {
    . $core *> $null
    $ErrorActionPreference = 'Stop'

    $PROXY_MODE = 'clash-verge'
    $CLASH_DIR = Join-Path $root 'clash'
    New-Item -ItemType Directory -Path $CLASH_DIR -Force | Out-Null
    $hash = [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')
    $configuredPipe = "verge-mihomo-sidecar-release-$hash"
    $servicePipe = "verge-mihomo-production-$hash"
    Write-Yaml (Join-Path $CLASH_DIR 'clash-verge.yaml') @(
        'mode: rule',
        "external-controller: ''",
        "external-controller-pipe: \\.\pipe\$configuredPipe",
        'secret: pipe-test-secret'
    )
    Write-Yaml (Join-Path $CLASH_DIR 'config.yaml') @('external-controller: 127.0.0.1:59999')

    # 1. 没有任何管道时回退到 HTTP 配置
    $api = Get-ClashApi
    Assert '无管道时回退 HTTP 控制接口' ($api -and -not $api.pipe -and $api.base -eq 'http://127.0.0.1:59999' -and $api.source -eq 'config.yaml') "实际=$($api.base) $($api.source)"

    # 2. 服务模式改名：按相同哈希后缀找到唯一的 verge-mihomo-* 管道，带上配置里的 secret
    $fake = Start-FakePipe $servicePipe @((New-HttpResponse 200 '{"mode":"rule","port":0}'))
    $fakes += $fake
    $api = Get-ClashApi
    Assert '按哈希后缀匹配服务模式管道' ($api -and $api.pipe -eq $servicePipe -and $api.source -eq 'clash-verge.yaml(pipe)' -and $api.base -eq "pipe:$servicePipe") "实际=$($api.pipe) $($api.source)"
    Assert '管道接口沿用配置的 secret' ($api.headers['Authorization'] -eq 'Bearer pipe-test-secret')
    $configs = Invoke-ClashApi '/configs' 5
    Assert '经管道读取 /configs' ($configs.ok -and $configs.data.mode -eq 'rule' -and $configs.endpoint -eq "pipe:$servicePipe") "原因=$($configs.reason)"
    $sent = [string]($fake.Requests | Select-Object -First 1)
    Assert '管道请求为 HTTP/1.0 GET 且带鉴权头' ($sent.StartsWith("GET /configs HTTP/1.0`r`n") -and $sent.Contains("Authorization: Bearer pipe-test-secret`r`n")) "请求=$sent"

    Stop-FakePipe $fake

    # 3. 配置原名与服务模式管道同时存在时优先原名；原名不在且同后缀候选不唯一时不猜测
    $fakeService = Start-FakePipe $servicePipe @((New-HttpResponse 200 '{}'))
    $fakeConfigured = Start-FakePipe $configuredPipe @((New-HttpResponse 200 '{}'))
    $fakes += $fakeService, $fakeConfigured
    Assert '配置原名存在时优先使用' ((Get-ClashApi).pipe -eq $configuredPipe)
    Stop-FakePipe $fakeConfigured
    $otherPipe = "verge-mihomo-other-$hash"
    $fakeOther = Start-FakePipe $otherPipe @((New-HttpResponse 200 '{}'))
    $fakes += $fakeOther
    $api = Get-ClashApi
    Assert '同后缀候选不唯一时回退 HTTP' ($api -and -not $api.pipe -and $api.source -eq 'config.yaml') "实际=$($api.pipe) $($api.source)"
    Stop-FakePipe $fakeOther
    Stop-FakePipe $fakeService

    # 4. 不接受可疑的配置管道名
    Write-Yaml (Join-Path $CLASH_DIR 'clash-verge.yaml') @("external-controller-pipe: \\.\pipe\..\evil-$hash")
    Assert '可疑的管道名不被采用' ($null -eq (Get-ClashPipeApi))
    Write-Yaml (Join-Path $CLASH_DIR 'clash-verge.yaml') @(
        'mode: rule',
        "external-controller: ''",
        "external-controller-pipe: \\.\pipe\$configuredPipe",
        'secret: pipe-test-secret'
    )

    # 5. 「规则置顶」经管道验证：前 16 条齐全为 ok，缺失为 fail
    $pinned = @($CLAUDE_DOMAINS | ForEach-Object { @{ type = 'DomainSuffix'; payload = $_; proxy = 'Proxy' } }) +
        @($CLAUDE_PORTS | ForEach-Object { @{ type = 'DstPort'; payload = $_; proxy = 'Proxy' } }) +
        @(@{ type = 'Match'; payload = ''; proxy = 'DIRECT' })
    $rulesBody = @{ rules = $pinned } | ConvertTo-Json -Depth 4 -Compress
    $fake = Start-FakePipe $servicePipe @((New-HttpResponse 200 '{"mode":"rule"}'), (New-HttpResponse 200 $rulesBody))
    $fakes += $fake
    $status = Get-RulesStatus
    Assert '管道返回置顶规则时规则置顶=ok' ($status -eq '规则置顶=ok') "实际=$status"
    Stop-FakePipe $fake
    $partialBody = @{ rules = @($pinned | Select-Object -Skip 5) } | ConvertTo-Json -Depth 4 -Compress
    $fake = Start-FakePipe $servicePipe @((New-HttpResponse 200 '{"mode":"rule"}'), (New-HttpResponse 200 $partialBody))
    $fakes += $fake
    $status = Get-RulesStatus
    Assert '内核规则缺失时规则置顶=fail' ($status -eq '规则置顶=fail:内核未加载置顶规则') "实际=$status"
    Stop-FakePipe $fake

    # 6. 异常响应分类：鉴权失败、非 JSON、无响应超时
    $fake = Start-FakePipe $servicePipe @((New-HttpResponse 401 '{"message":"Unauthorized"}'), "HTTP/1.0 200 OK`r`nContent-Length: 9`r`n`r`nnot json!", $null)
    $fakes += $fake
    Assert '401 归类为鉴权失败' ((Invoke-ClashApi '/rules' 5).reason -eq 'auth-failed')
    Assert '非 JSON 响应归类为异常' ((Invoke-ClashApi '/rules' 5).reason -eq 'error')
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $slow = Invoke-ClashApi '/rules' 1
    Assert '无响应时按超时返回' ($slow.reason -eq 'timeout' -and $clock.Elapsed.TotalSeconds -lt 2.5) "原因=$($slow.reason) 用时=$($clock.Elapsed.TotalSeconds)"
    Stop-FakePipe $fake
    Assert '管道不可用有对应提示' ((Get-ClashApiHint 'pipe-unavailable') -match '控制管道不可用')
} finally {
    $ErrorActionPreference = $scriptErrorPreference
    foreach ($f in $fakes) { try { if (-not $f.Handle.IsCompleted) { Stop-FakePipe $f } } catch {} }
    foreach ($name in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name]) }
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures -gt 0) {
    "FAILED: $failures 项"
    exit 1
}
'OK: 全部通过'
