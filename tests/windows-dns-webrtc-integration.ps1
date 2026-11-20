# DNS 与 WebRTC 检测/修复链路及隔离门禁回归测试
#
# 只用临时沙箱目录、打桩 cmdlet 与本地 node 执行 Script.js，不改动本机任何配置、Clash 文件、注册表或网络。
# powershell -NoProfile -ExecutionPolicy Bypass -File tests\windows-dns-webrtc-integration.ps1

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$core = Join-Path $repo 'windows\claude-switch.ps1'
if (-not (Test-Path -LiteralPath $core)) { throw "找不到核心脚本: $core" }

$root = Join-Path ([System.IO.Path]::GetTempPath()) ('cs-dnswebrtc-it-' + [guid]::NewGuid().ToString('N'))
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

    # 1. Open-SafeUrl 双因子门禁：TUN 离线时即使客户端进程在线也必须拒绝
    $script:mockProxyAlive = $true
    $script:mockTunUp = $false
    function Test-ProxyAlive { $script:mockProxyAlive }
    function Test-TunUp { $script:mockTunUp }

    $PROXY_MODE = 'clash-verge'
    $script:exitCode = 0
    $out = (@(Open-SafeUrl 'https://claude.ai') -join "`n")
    Assert 'Open-SafeUrl 在进程在线但 TUN 离线时拒绝打开' ($script:exitCode -eq 1 -and $out -match '^FAIL:代理路径未就绪') "out=$out"

    $PROXY_MODE = 'other'
    $script:exitCode = 0
    $out = (@(Open-SafeUrl 'https://claude.ai') -join "`n")
    Assert 'Open-SafeUrl 在 other 模式且 TUN 离线时拒绝打开' ($script:exitCode -eq 1 -and $out -match '^FAIL:代理路径未就绪') "out=$out"

    # 2. Resolve-ClashProxyGroup：跳过 GLOBAL/DIRECT 并持久化到 envguard-config.json，离线回退 YAML
    $PROXY_MODE = 'clash-verge'
    $PROXY_GROUP = 'GLOBAL'
    $script:mockApi = @{}
    function Invoke-ClashApi([string]$Path, [int]$TimeoutSec = 8) {
        if ($script:mockApi.ContainsKey($Path)) { return $script:mockApi[$Path] }
        return @{ ok = $false; reason = 'no-endpoint'; data = $null; endpoint = ''; source = '' }
    }

    $script:mockApi = @{
        '/proxies' = @{
            ok = $true
            data = [pscustomobject]@{
                proxies = [pscustomobject]@{
                    GLOBAL = [pscustomobject]@{ type = 'Selector'; now = 'DIRECT' }
                    Auto   = [pscustomobject]@{ type = 'URLTest';  now = 'US-01' }
                    Proxy  = [pscustomobject]@{ type = 'Selector'; now = 'US-01' }
                }
            }
        }
    }
    $resolved = Resolve-ClashProxyGroup -Persist
    $savedCfg = Get-Content -LiteralPath $ENVGUARD_CONFIG -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert 'Resolve-ClashProxyGroup 自动跳过 GLOBAL/DIRECT 选用真实 Selector 组并持久化' ($resolved -eq 'Proxy' -and $PROXY_GROUP -eq 'Proxy' -and $savedCfg.proxy_group -eq 'Proxy') "resolved=$resolved"

    $script:mockApi = @{}
    $PROXY_GROUP = 'GLOBAL'
    [System.IO.File]::WriteAllText((Join-Path $CLASH_DIR 'clash-verge.yaml'), @(
        'mode: rule',
        'proxy-groups:',
        '  - type: select',
        '    name: ReversedSelectGroup'
    ) -join "`n", (New-Object System.Text.UTF8Encoding($false)))
    $resolvedYaml = Resolve-ClashProxyGroup
    Assert 'API 不可用时从 clash-verge.yaml 解析非 GLOBAL 代理组' ($resolvedYaml -eq 'ReversedSelectGroup') "resolved=$resolvedYaml"

    # 3. Test-PinnedRules 与 Get-RulesStatus 拒绝 DIRECT 与 GLOBAL->DIRECT
    $directRules = @($CLAUDE_DOMAINS | ForEach-Object { [pscustomobject]@{ type = 'DomainSuffix'; payload = $_; proxy = 'DIRECT' } }) +
        @($CLAUDE_PORTS | ForEach-Object { [pscustomobject]@{ type = 'DstPort'; payload = $_; proxy = 'Proxy' } })
    Assert 'Test-PinnedRules 拒绝域名指向 DIRECT 的规则' (-not (Test-PinnedRules $directRules))

    $rejectPorts = @($CLAUDE_DOMAINS | ForEach-Object { [pscustomobject]@{ type = 'DomainSuffix'; payload = $_; proxy = 'Proxy' } }) +
        @($CLAUDE_PORTS | ForEach-Object { [pscustomobject]@{ type = 'DstPort'; payload = $_; proxy = 'REJECT' } })
    Assert 'Test-PinnedRules 允许 STUN 端口使用 REJECT 阻断' (Test-PinnedRules $rejectPorts)

    $globalPinned = @($CLAUDE_DOMAINS | ForEach-Object { [pscustomobject]@{ type = 'DomainSuffix'; payload = $_; proxy = 'GLOBAL' } }) +
        @($CLAUDE_PORTS | ForEach-Object { [pscustomobject]@{ type = 'DstPort'; payload = $_; proxy = 'GLOBAL' } })
    $script:mockApi = @{
        '/configs'        = @{ ok = $true; data = [pscustomobject]@{ mode = 'rule' } }
        '/rules'          = @{ ok = $true; data = [pscustomobject]@{ rules = $globalPinned } }
        '/proxies/GLOBAL' = @{ ok = $true; data = [pscustomobject]@{ now = 'DIRECT' } }
    }
    $gDirectStatus = Get-RulesStatus
    Assert '前 16 条指向 GLOBAL 且 GLOBAL 当前为 DIRECT 时规则置顶判 fail' ($gDirectStatus -match '^规则置顶=fail:.*GLOBAL') "status=$gDirectStatus"

    $script:mockApi['/proxies/GLOBAL'] = @{ ok = $true; data = [pscustomobject]@{ now = 'Proxy' } }
    $gProxyStatus = Get-RulesStatus
    Assert '前 16 条指向 GLOBAL 且 GLOBAL 选到代理组时判 ok' ($gProxyStatus -eq '规则置顶=ok') "status=$gProxyStatus"

    # 4. Write-ClashScriptRules + Node 真实执行生成的 Script.js main(config)
    $writeRes = Write-ClashScriptRules 'MyProxy'
    $scriptJsPath = Join-Path $CLASH_DIR 'profiles\Script.js'
    Assert 'Write-ClashScriptRules 成功生成 Script.js' ($writeRes.ok -and $writeRes.changed -and (Test-Path -LiteralPath $scriptJsPath))

    $nodeCheck = @'
const fs = require('fs');
const assert = require('assert');
const code = fs.readFileSync(process.env.TEST_SCRIPT_JS, 'utf8');
const fn = new Function(code + '; return main;');
const main = fn();

const cfgA = main({
  ipv6: true,
  rules: ['MATCH,DIRECT'],
  dns: {
    'nameserver-policy': {
      '*': '223.5.5.5',
      '*.claude.ai': '114.114.114.114',
      'geosite:cn': '223.5.5.5'
    },
    fallback: ['114.114.114.114'],
    'fake-ip-filter': ['+.claude.ai', '+.ipleak.net', '+.miwifi.com', '+.bilibili.com', '+.l.google.com', '+.cloudflare.com', 'localhost', 123]
  },
  tun: { enable: true }
});

assert.equal(cfgA.rules.length, 20);
assert.equal(cfgA.rules[0], 'DOMAIN-SUFFIX,claude.ai,MyProxy');
assert.equal(cfgA.rules[15], 'DST-PORT,19302,MyProxy');
assert.equal(cfgA.rules[16], 'IP-CIDR,8.8.8.8/32,MyProxy,no-resolve');
assert.equal(cfgA.rules[17], 'IP-CIDR,1.1.1.1/32,MyProxy,no-resolve');
assert.equal(cfgA.rules[18], 'DOMAIN-SUFFIX,ipleak.net,MyProxy');
assert.equal(cfgA.rules[19], 'MATCH,DIRECT');
assert.equal(cfgA.ipv6, false);
assert.equal(cfgA.dns.enable, true);
assert.equal(cfgA.dns.ipv6, false);
assert.equal(cfgA.dns['enhanced-mode'], 'fake-ip');
assert.equal(cfgA.dns['fake-ip-range'], '198.18.0.1/16');
assert.equal(cfgA.dns['respect-rules'], true);
assert.deepEqual(cfgA.dns['proxy-server-nameserver'], ['223.5.5.5', '119.29.29.29']);
assert.deepEqual(cfgA.dns.nameserver, ['tls://8.8.8.8', 'tls://1.1.1.1']);
assert.deepEqual(cfgA.dns.fallback, []);
assert.equal(cfgA.dns['nameserver-policy']['*'], undefined);
assert.equal(cfgA.dns['nameserver-policy']['*.claude.ai'], undefined);
assert.equal(cfgA.dns['nameserver-policy']['geosite:cn'], '223.5.5.5');
assert.deepEqual(cfgA.dns['fake-ip-filter'], ['localhost', 123]);
assert.equal(cfgA.tun['strict-route'], true);
assert.deepEqual(cfgA.tun['dns-hijack'], ['any:53', 'tcp://any:53']);

const cfgB = main({
  dns: {
    'default-nameserver': ['1.2.4.8'],
    'fake-ip-filter-mode': 'whitelist',
    'fake-ip-filter': ['+.claude.ai']
  }
});
assert.deepEqual(cfgB.dns['proxy-server-nameserver'], ['1.2.4.8']);
assert.deepEqual(cfgB.dns['fake-ip-filter'], ['+.claude.ai']);
console.log('NODE_SCRIPT_JS_OK');
'@
    $env:TEST_SCRIPT_JS = $scriptJsPath
    try {
        $nodeOut = (& node -e $nodeCheck 2>&1) -join "`n"
        Assert 'Script.js 经 Node 实跑验证 DNS/规则/白名单/TUN 全部语义' ($LASTEXITCODE -eq 0 -and $nodeOut -match 'NODE_SCRIPT_JS_OK') "nodeOut=$nodeOut"
    } finally {
        [Environment]::SetEnvironmentVariable('TEST_SCRIPT_JS', $null)
    }

    # 5. Test-DnsCheck：CNAME 首条跳过与 SMHNR 未关闭提示
    $script:mockProxyAlive = $true
    $script:mockTunUp = $true
    $script:mockSmhnrDisabled = $false
    $script:ProbePrefetch = $null
    function Test-SmhnrDisabled { $script:mockSmhnrDisabled }
    function Resolve-DnsName {
        param([string]$Name, [string]$Type)
        @(
            [pscustomobject]@{ Name = $Name; Type = 'CNAME'; IPAddress = $null },
            [pscustomobject]@{ Name = $Name; Type = 'A';     IPAddress = '198.18.0.5' }
        )
    }
    function Invoke-ProbeText {
        param([string]$Url, [int]$Attempts = 3, [int]$TimeoutSec = 8, [scriptblock]$Valid = { param($b) [bool]$b })
        if ($Url -match 'ipleak\.net/dnsdetection') { return '{"ip":{"8.8.8.8":1}}' }
        if ($Url -match 'ipleak\.net/json/8\.8\.8\.8') { return '{"country_code":"US"}' }
        return ''
    }
    $dnsSmhnrWarn = Test-DnsCheck
    Assert 'Resolve-DnsName 首条 CNAME 时仍取到 Fake-IP 且 SMHNR 未关降为 warn' ($dnsSmhnrWarn.state -eq 'warn' -and $dnsSmhnrWarn.detail -match 'SMHNR') "detail=$($dnsSmhnrWarn.detail)"
    $script:mockSmhnrDisabled = $true
    $dnsAllOk = Test-DnsCheck
    Assert 'SMHNR 已关闭且海外解析器时 DNS 检查通过' ($dnsAllOk.state -eq 'ok') "detail=$($dnsAllOk.detail)"

    # 6. ConvertFrom-StunResponse 与 Test-WebRTC
    $txnGood = [byte[]](1..12)
    $txnBad = [byte[]](2..13)
    $cookie = [byte[]](0x21, 0x12, 0xA4, 0x42)
    $ipBytes = ([System.Net.IPAddress]::Parse('203.0.113.7')).GetAddressBytes()
    $xored = [byte[]]@(for ($i = 0; $i -lt 4; $i++) { $ipBytes[$i] -bxor $cookie[$i] })
    $attr = [byte[]](0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00) + $xored
    $pktGood = [byte[]](0x01, 0x01, 0x00, [byte]$attr.Length) + $cookie + $txnGood + $attr
    $pktBadHdr = $pktGood.Clone(); $pktBadHdr[0] = 0x00
    $pktBadCookie = $pktGood.Clone(); $pktBadCookie[4] = 0x99

    Assert 'STUN 校验匹配 Transaction ID 通过' ((ConvertFrom-StunResponse $pktGood $txnGood) -eq '203.0.113.7')
    Assert 'STUN 拒绝不匹配的 Transaction ID' ($null -eq (ConvertFrom-StunResponse $pktGood $txnBad))
    Assert 'STUN 拒绝非法报文头与错误 Magic Cookie' (($null -eq (ConvertFrom-StunResponse $pktBadHdr $txnGood)) -and ($null -eq (ConvertFrom-StunResponse $pktBadCookie $txnGood)))

    $script:stunCalled = 0
    function Get-StunPublicIPs { param($Servers, $TimeoutMs = 2000); $script:stunCalled++; return $script:mockStunIps }
    function Invoke-ProbeText {
        param([string]$Url, [int]$Attempts = 3, [int]$TimeoutSec = 8, [scriptblock]$Valid = { param($b) [bool]$b })
        if ($Url -match 'ipinfo\.io/114\.114\.114\.114/json') { return '{"ip":"114.114.114.114","country":"CN"}' }
        if ($Url -match 'ipinfo\.io/198\.51\.100\.88/json') { return '{"ip":"198.51.100.88","country":"US"}' }
        return ''
    }

    $script:mockProxyAlive = $false; $script:mockTunUp = $false; $script:stunCalled = 0
    $rtcDown = Test-WebRTC '198.51.100.1'
    Assert '代理路径失效时 WebRTC 零发包判 fail' ($rtcDown.code -eq 'fail' -and $script:stunCalled -eq 0) "code=$($rtcDown.code)"

    $script:mockProxyAlive = $true; $script:mockTunUp = $true
    $script:mockStunIps = @('114.114.114.114')
    $rtcCn = Test-WebRTC '198.51.100.1'
    Assert 'WebRTC 暴露国内 UDP IP 时判 fail' ($rtcCn.code -eq 'fail' -and $rtcCn.detail -match '国内 UDP 出口') "detail=$($rtcCn.detail)"

    $script:mockStunIps = @('198.51.100.88')
    $rtcSplit = Test-WebRTC '198.51.100.1'
    Assert 'WebRTC 暴露同海外但不同于 HTTP 的 UDP IP 时判 warn' ($rtcSplit.code -eq 'warn' -and $rtcSplit.detail -match 'UDP 出口与 HTTP 出口不同') "detail=$($rtcSplit.detail)"

    $script:mockStunIps = @('198.51.100.1')
    $rtcOk = Test-WebRTC '198.51.100.1'
    Assert 'WebRTC 仅暴露当前代理出口时判 ok' ($rtcOk.code -eq 'ok') "detail=$($rtcOk.detail)"

    # 7. Invoke-Fix：global 模式同样写 Script.js；UAC 取消必须设置 exitCode=1 且不得误报完成
    Remove-Item -LiteralPath $scriptJsPath -Force -ErrorAction SilentlyContinue
    $PROXY_MODE = 'clash-verge'
    $COUNTRY_TARGET = 'US'
    $TZ_TARGET = 'Eastern Standard Time'
    $LOCALE_TARGET = 'en-US'
    $script:CheckConfig = @{ disabled = @('时区', '地区'); allowOptionalFailures = $false; logging = $true }
    $script:mockApi = @{
        '/proxies' = @{
            ok = $true
            data = [pscustomobject]@{
                proxies = [pscustomobject]@{
                    Proxy = [pscustomobject]@{ type = 'Selector'; now = 'US-01' }
                }
            }
        }
        '/configs' = @{ ok = $true; data = [pscustomobject]@{ mode = 'global' } }
    }
    function Get-CheckConfig { $script:CheckConfig }
    function Restart-ClashVerge { $true }
    function Wait-Tun { param([int]$seconds) $true }
    function Start-Sleep { }
    function Get-Egress { @{ ip = '198.51.100.1'; loc = 'US'; skipped = $false } }
    function Get-NetAdapterBinding { param([string]$ComponentID) @() }
    function Get-BodyguardHealth { @{ state = 'ok'; detail = '在线'; installed = $true; running = $true; legacyPolicy = $false } }
    function Save-HomeSmhnrState { }
    $script:mockSmhnrDisabled = $false
    function Start-Process {
        [CmdletBinding()]
        param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, [string]$Verb)
        if ($Verb -eq 'RunAs') { return [pscustomobject]@{ ExitCode = 1 } }
        return $null
    }

    $script:exitCode = 0
    $fixOut = (@(Invoke-Fix) -join "`n")
    Assert 'Invoke-Fix 在 global 模式下仍生成 Script.js 防漏块' (Test-Path -LiteralPath $scriptJsPath)
    Assert 'Invoke-Fix 在 UAC 失败时置 exitCode=1 并报 FAIL 且不误打勾' (
        $script:exitCode -eq 1 -and
        $fixOut -match '(?m)^FAIL:修复未完全生效\(.*管理员授权未完成' -and
        $fixOut -notmatch 'FIX:时区/IPv6/DNS防漏/保镖 ✓'
    ) "exit=$script:exitCode out=$fixOut"
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
