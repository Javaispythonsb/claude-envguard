# Claude 账号切换器 - Windows 核心脚本
# 用法: powershell -NoProfile -File claude-switch.ps1 check|checkdetail|status|switch A|B|C|D|fix|set-region|set-client|get-flclash-controller|set-flclash-controller|check-update|update-now
$ErrorActionPreference = 'Continue'
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}

# ============ 安装时可配置项(安装器自动改写) ============
$PROXY_MODE = "clash-verge"            # 代理客户端: clash-verge|flclash|sing-box|clashx|surge|wireguard|other|auto
$COUNTRY_TARGET = "US"                 # 目标出口国家(安装时按出口自动识别)
$TZ_TARGET = "Eastern Standard Time"   # Windows 时区ID
$LOCALE_TARGET = "en-US"               # 地区格式目标(安装/set-region 自动写入)
$PROXY_GROUP = "GLOBAL"                # Clash 代理组名
# =======================================================

$BASE = if ($env:ENVGUARD_STATE_DIR) { [string]$env:ENVGUARD_STATE_DIR } else { Join-Path $env:USERPROFILE ".claude-switcher" }
$ENVGUARD_CONFIG = Join-Path $BASE "envguard-config.json"
$IANA_TZ_OVERRIDE = ""

# 外部运行时配置桥:当从 Tauri 资源目录或开发目录调用本脚本(非 $BASE\claude-switch.ps1)时:
# 1) 若用户目录已有旧版 $BASE\claude-switch.ps1,只读继承其 5 项锁定配置(绝不覆盖原文件);
# 2) 若存在 $BASE\envguard-config.json,再叠加其中的最新客户端/地区锁定值。
function Initialize-RuntimeConfig {
    $selfFull = if ($PSCommandPath) { try { [IO.Path]::GetFullPath($PSCommandPath) } catch { '' } } else { '' }
    $installedCore = Join-Path $BASE 'claude-switch.ps1'
    $installedFull = try { [IO.Path]::GetFullPath($installedCore) } catch { '' }
    if ($selfFull -and $installedFull -and ($selfFull -ine $installedFull) -and (Test-Path -LiteralPath $installedCore -PathType Leaf)) {
        try {
            $legacyText = [IO.File]::ReadAllText($installedCore, [Text.Encoding]::UTF8)
            foreach ($pair in @(
                @('PROXY_MODE','PROXY_MODE'),
                @('COUNTRY_TARGET','COUNTRY_TARGET'),
                @('TZ_TARGET','TZ_TARGET'),
                @('LOCALE_TARGET','LOCALE_TARGET'),
                @('PROXY_GROUP','PROXY_GROUP')
            )) {
                $m = [regex]::Match($legacyText, '(?m)^[ \t]*\$' + $pair[0] + '[ \t]*=[ \t]*"([^"]*)"')
                if ($m.Success -and $m.Groups[1].Value) {
                    Set-Variable -Name $pair[1] -Scope Script -Value $m.Groups[1].Value
                }
            }
        } catch {}
    }
    if (Test-Path -LiteralPath $ENVGUARD_CONFIG -PathType Leaf) {
        try {
            $raw = [IO.File]::ReadAllText($ENVGUARD_CONFIG, [Text.Encoding]::UTF8)
            if ($raw.Length -le 16384) {
                $cfg = $raw | ConvertFrom-Json -ErrorAction Stop
                if ($cfg.proxy_mode -is [string] -and $cfg.proxy_mode) { $script:PROXY_MODE = $cfg.proxy_mode }
                if ($cfg.country_target -is [string] -and $cfg.country_target -match '^[A-Z]{2}$') { $script:COUNTRY_TARGET = $cfg.country_target }
                if ($cfg.tz_target -is [string] -and $cfg.tz_target) { $script:TZ_TARGET = $cfg.tz_target }
                if ($cfg.iana_tz_target -is [string] -and $cfg.iana_tz_target -match '^[A-Za-z_]+/[A-Za-z_]+(/[A-Za-z_]+)?$') { $script:IANA_TZ_OVERRIDE = $cfg.iana_tz_target }
                if ($cfg.locale_target -is [string] -and $cfg.locale_target) { $script:LOCALE_TARGET = $cfg.locale_target }
                if ($cfg.proxy_group -is [string] -and $cfg.proxy_group -and $cfg.proxy_group -notmatch '["`$,\x00-\x1f]') { $script:PROXY_GROUP = $cfg.proxy_group }
            }
        } catch {}
    }
}

function Save-RuntimeConfig([hashtable]$Patch) {
    $current = [ordered]@{
        proxy_mode     = $PROXY_MODE
        country_target = $COUNTRY_TARGET
        tz_target      = $TZ_TARGET
        iana_tz_target = $(if ($IANA_TZ_OVERRIDE) { $IANA_TZ_OVERRIDE } else { '' })
        locale_target  = $LOCALE_TARGET
        proxy_group    = $PROXY_GROUP
    }
    foreach ($k in $Patch.Keys) { $current[$k] = $Patch[$k] }
    New-Item -ItemType Directory -Force -Path $BASE -ErrorAction Stop | Out-Null
    $tmp = Join-Path $BASE ('.envguard-cfg-' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        [IO.File]::WriteAllText($tmp, (([pscustomobject]$current) | ConvertTo-Json -Depth 3), (New-Object Text.UTF8Encoding($false)))
        Move-Item -LiteralPath $tmp -Destination $ENVGUARD_CONFIG -Force -ErrorAction Stop
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
}

function Test-SelfInBase([string]$SelfPath) {
    if (-not $SelfPath) { return $false }
    try {
        return ([IO.Path]::GetFullPath($SelfPath) -ieq [IO.Path]::GetFullPath((Join-Path $BASE 'claude-switch.ps1')))
    } catch { return $false }
}

Initialize-RuntimeConfig
function Find-ClaudeMsixDataPath([switch]$PackagedOnly) {
    $msixRoot = Join-Path $env:LOCALAPPDATA "Claude-Data"
    try {
        $pkg = Get-AppxPackage -Name 'Claude' -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($pkg -and $pkg.PackageFamilyName) {
            return (Join-Path $msixRoot ([string]$pkg.PackageFamilyName))
        }
    } catch {}
    if ($PackagedOnly) { return $null }
    $candidates = @(Get-ChildItem -LiteralPath $msixRoot -Directory -Filter 'claude_*' -ErrorAction SilentlyContinue)
    if ($candidates.Count -eq 1) { return $candidates[0].FullName }
    return $null
}
function Find-ClaudeDataPath {
    # 已安装 Appx 包且其数据目录存在时优先:%APPDATA%\Claude 可能只是 MCP 工具或旧版残留建的目录,
    # 按它切换会把空目录当当前账号归档。未安装包时仍旧版优先,卸载后遗留的 Claude-Data 目录不能抢占。
    $packaged = Find-ClaudeMsixDataPath -PackagedOnly
    if ($packaged -and (Test-Path -LiteralPath $packaged)) { return $packaged }
    $legacy = Join-Path $env:APPDATA "Claude"
    if (Test-Path -LiteralPath $legacy) { return $legacy }
    if ($packaged) { return $packaged }
    $msix = Find-ClaudeMsixDataPath
    if ($msix) { return $msix }
    return $legacy
}
$DATA = Find-ClaudeDataPath
$LOG  = Join-Path $BASE "claude-switch.log"
$CLIENT_LOG = Join-Path $BASE "logs\client.log"
$CHECK_CONFIG = Join-Path $BASE "checks.json"
$script:CheckConfig = $null
$CLASH_DIR = @(
    (Join-Path $env:APPDATA "io.github.clash-verge-rev.clash-verge-rev"),
    (Join-Path $env:LOCALAPPDATA "io.github.clash-verge-rev.clash-verge-rev")
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $CLASH_DIR) { $CLASH_DIR = Join-Path $env:APPDATA "io.github.clash-verge-rev.clash-verge-rev" }
# check-update / rules-status / diagnose-proxy / upstream-status / 只读 uistate 不在检查路径上创建任何目录;
# accounts 子目录仅在旧版切换/快照命令中按需创建,EnvGuard 命令只建 $BASE
$readOnlyCommands = @('check-update','rules-status','diagnose-proxy','upstream-status')
$accountCommands = @('status','switch','snapshot-export','snapshot-import')
if ($args.Count -eq 0) {
    New-Item -ItemType Directory -Force -Path "$BASE\accounts" | Out-Null
} elseif ($args[0] -notin $readOnlyCommands -and -not ($args[0] -eq 'uistate' -and ($args.Count -lt 2 -or $args[1] -ne '--fresh'))) {
    if ($args[0] -in $accountCommands) {
        New-Item -ItemType Directory -Force -Path "$BASE\accounts" | Out-Null
    } else {
        New-Item -ItemType Directory -Force -Path $BASE | Out-Null
    }
}

function Log($m) { "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $m" | Out-File -Append -Encoding utf8 $LOG }

# 当前安装版本(安装器/更新器写入 version.txt,缺失视为 unknown)
function Get-AppVersion {
    # 由 EnvGuard 调用时以应用自身版本为准:version.txt 属于旧版切换器的安装
    if ($env:ENVGUARD_APP_VERSION -match '^[0-9][A-Za-z0-9._-]{0,39}$') { return $env:ENVGUARD_APP_VERSION }
    $vf = Join-Path $BASE "version.txt"
    if (Test-Path -LiteralPath $vf) {
        try {
            $raw = Get-Content -LiteralPath $vf -Raw -Encoding UTF8 -ErrorAction Stop
            if ($null -ne $raw) {
                $v = ([string]$raw).Trim()
                if ($v) { return $v }
            }
        } catch {}
    }
    return 'unknown'
}

function ConvertTo-CheckConfig($Value) {
    $optional = @('IP一致','DNS','WebRTC','客户端语言','ASN属性','生态可达性')
    $keys = @('disabled','allowOptionalFailures','logging')
    if ($null -eq $Value -or $Value -isnot [pscustomobject]) { throw '检查配置必须是 JSON 对象' }
    foreach ($key in @($Value.PSObject.Properties.Name)) {
        if ($keys -notcontains $key) { throw '检查配置包含未知字段' }
    }
    $disabled = @()
    if ($Value.PSObject.Properties.Name -contains 'disabled') {
        $raw = $Value.disabled
        if ($null -eq $raw -or $raw -is [string] -or $raw -isnot [System.Collections.IEnumerable]) { throw 'disabled 必须是数组' }
        $seen = @{}
        foreach ($name in $raw) {
            if ($name -isnot [string] -or $optional -notcontains $name -or $seen.ContainsKey($name)) { throw '只能关闭允许的非关键检查，且不能重复' }
            $seen[$name] = $true
            $disabled += $name
        }
    }
    $allow = $false
    $logging = $true
    foreach ($key in @('allowOptionalFailures','logging')) {
        if ($Value.PSObject.Properties.Name -contains $key) {
            if ($Value.$key -isnot [bool]) { throw "$key 必须是布尔值" }
        }
    }
    if ($Value.PSObject.Properties.Name -contains 'allowOptionalFailures') { $allow = [bool]$Value.allowOptionalFailures }
    if ($Value.PSObject.Properties.Name -contains 'logging') { $logging = [bool]$Value.logging }
    return @{ disabled = @($optional | Where-Object { $disabled -contains $_ }); allowOptionalFailures = $allow; logging = $logging }
}

function Get-CheckConfig {
    if (-not (Test-Path -LiteralPath $CHECK_CONFIG)) {
        return @{ disabled = @(); allowOptionalFailures = $false; logging = $true }
    }
    $raw = [System.IO.File]::ReadAllText($CHECK_CONFIG, [System.Text.Encoding]::UTF8)
    if ($raw.Length -gt 16384) { throw '检查配置过大' }
    return ConvertTo-CheckConfig ($raw | ConvertFrom-Json -ErrorAction Stop)
}

function Set-CheckConfigFromJson([string]$Json) {
    if ([string]::IsNullOrWhiteSpace($Json) -or $Json.Length -gt 16384) { throw '检查配置为空或过大' }
    $config = ConvertTo-CheckConfig ($Json | ConvertFrom-Json -ErrorAction Stop)
    $jsonOut = [pscustomobject]@{ disabled = @($config.disabled); allowOptionalFailures = [bool]$config.allowOptionalFailures; logging = [bool]$config.logging } | ConvertTo-Json -Depth 3
    $tmp = Join-Path $BASE ('.checks-' + [guid]::NewGuid().Guid + '.tmp')
    try {
        [System.IO.File]::WriteAllText($tmp, $jsonOut, (New-Object System.Text.UTF8Encoding($false)))
        Move-Item -LiteralPath $tmp -Destination $CHECK_CONFIG -Force -ErrorAction Stop
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
}

function Test-CheckDisabled([string]$Name) {
    return ($null -ne $script:CheckConfig -and @($script:CheckConfig.disabled) -contains $Name)
}

function Format-OptionalResult([string]$Name, [string]$State, [string]$Detail = '') {
    if (Test-CheckDisabled $Name) { return "$Name=skip:用户已禁用" }
    if ($State -eq 'fail' -and $script:CheckConfig.allowOptionalFailures) {
        return "$Name=warn:非关键失败已忽略$(if ($Detail) { "($Detail)" })"
    }
    return "$Name=$State$(if ($Detail) { ":$Detail" })"
}

function Write-ClientLog([string]$Command, [int]$ExitCode, [long]$DurationMs, [string]$Summary = '') {
    try {
        $config = Get-CheckConfig
        if (-not $config.logging) { return }
        if ($Command -notmatch '^[a-z][a-z0-9-]{0,31}$' -or $ExitCode -lt 0 -or $ExitCode -gt 255 -or $DurationMs -lt 0) { return }
        $all = @('代理内核','TUN网卡','出口IP','IP一致','时区','地区','IPv6','DNS','WebRTC','保镖','规则置顶','客户端语言','ASN属性','生态可达性')
        $states = @{}
        foreach ($token in ($Summary -split '\s+')) {
            if (-not $token) { continue }
            $kv = $token -split '=', 2
            if ($kv.Count -ne 2 -or $all -notcontains $kv[0] -or @('ok','warn','fail','skip') -notcontains $kv[1] -or $states.ContainsKey($kv[0])) { return }
            $states[$kv[0]] = $kv[1]
        }
        $parts = @()
        foreach ($name in $all) { if ($states.ContainsKey($name)) { $parts += "$name=$($states[$name])" } }
        $version = Get-AppVersion
        if ($version -notmatch '^[A-Za-z0-9._-]{1,40}$') { $version = 'unknown' }
        $line = ([DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')) + " version=$version command=$Command exit=$ExitCode duration_ms=$DurationMs"
        if ($parts.Count) { $line += ' ' + ($parts -join ' ') }
        $line += "`n"
        $dir = Split-Path -Parent $CLIENT_LOG
        New-Item -ItemType Directory -Force -Path $dir -ErrorAction Stop | Out-Null
        if ((Test-Path -LiteralPath $CLIENT_LOG) -and ((Get-Item -LiteralPath $CLIENT_LOG).Length + [Text.Encoding]::UTF8.GetByteCount($line) -gt 524288)) {
            Move-Item -LiteralPath $CLIENT_LOG -Destination "$CLIENT_LOG.1" -Force -ErrorAction Stop
        }
        [System.IO.File]::AppendAllText($CLIENT_LOG, $line, (New-Object System.Text.UTF8Encoding($false)))
    } catch {}
}

function Export-Diagnostics([string]$Destination) {
    if ([string]::IsNullOrWhiteSpace($Destination)) { throw '请选择诊断包保存位置' }
    $stage = Join-Path $env:TEMP ('claude-diagnostics-' + [guid]::NewGuid().Guid)
    New-Item -ItemType Directory -Force -Path $stage -ErrorAction Stop | Out-Null
    try {
        $version = Get-AppVersion
        if ($version -notmatch '^[A-Za-z0-9._-]{1,40}$') { $version = 'unknown' }
        $envInfo = [ordered]@{ schema_version = 1; platform = 'Windows'; app_version = $version; generated_at = ([DateTime]::UtcNow.ToString('o')) }
        [System.IO.File]::WriteAllText((Join-Path $stage 'environment.json'), ($envInfo | ConvertTo-Json -Compress), (New-Object System.Text.UTF8Encoding($false)))
        $status = @{}
        if (Test-Path -LiteralPath "$BASE\last-check.txt") {
            $all = @('代理内核','TUN网卡','出口IP','IP一致','时区','地区','IPv6','DNS','WebRTC','保镖','规则置顶','客户端语言','ASN属性','生态可达性')
            foreach ($line in [System.IO.File]::ReadAllLines("$BASE\last-check.txt")) {
                if ($line -match '^([^=]+)=(ok|warn|fail|skip)(?::.*)?$' -and $all -contains $Matches[1] -and -not $status.ContainsKey($Matches[1])) {
                    $status[$Matches[1]] = $Matches[2]
                }
            }
        }
        [System.IO.File]::WriteAllText((Join-Path $stage 'recent-status.json'), (@{ checks = $status } | ConvertTo-Json -Depth 3), (New-Object System.Text.UTF8Encoding($false)))
        $config = Get-CheckConfig
        $configJson = [pscustomobject]@{ disabled = @($config.disabled); allowOptionalFailures = [bool]$config.allowOptionalFailures; logging = [bool]$config.logging } | ConvertTo-Json -Depth 3
        [System.IO.File]::WriteAllText((Join-Path $stage 'checks.json'), $configJson, (New-Object System.Text.UTF8Encoding($false)))
        $safe = @()
        if (Test-Path -LiteralPath $CLIENT_LOG) {
            $pattern = '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z version=[A-Za-z0-9._-]+ command=[a-z][a-z0-9-]{0,31} exit=\d{1,3} duration_ms=\d+(?: (?:[^=\s]+=(?:ok|warn|fail|skip) ?)*)?$'
            $safe = @([System.IO.File]::ReadAllLines($CLIENT_LOG) | Where-Object { $_ -match $pattern } | Select-Object -Last 2000)
        }
        [System.IO.File]::WriteAllText((Join-Path $stage 'client.log'), (($safe -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
        Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $Destination -Force -ErrorAction Stop
        'OK:诊断日志已导出（不含 IP、凭据、账号或代理配置）'
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# 版本号按 . 分段比较,每段只取前导数字(忽略 -beta 等后缀);unknown 视为最旧
function ConvertTo-VersionParts([string]$v) {
    if ([string]::IsNullOrWhiteSpace($v)) { return @() }
    $clean = $v.Trim().TrimStart('v', 'V')
    if ($clean -eq 'unknown') { return @() }
    return @($clean.Split('.'))
}

function Compare-Version([string]$a, [string]$b) {
    $pa = @(ConvertTo-VersionParts $a)
    $pb = @(ConvertTo-VersionParts $b)
    $ua = ($pa.Count -eq 0)
    $ub = ($pb.Count -eq 0)
    if ($ua -and $ub) { return 0 }
    if ($ua) { return -1 }
    if ($ub) { return 1 }
    $n = [Math]::Max($pa.Count, $pb.Count)
    for ($i = 0; $i -lt $n; $i++) {
        $na = 0; $nb = 0
        if ($i -lt $pa.Count) {
            $m = [regex]::Match([string]$pa[$i], '^\d+')
            if ($m.Success) { $na = [int64]$m.Value }
        }
        if ($i -lt $pb.Count) {
            $m = [regex]::Match([string]$pb[$i], '^\d+')
            if ($m.Success) { $nb = [int64]$m.Value }
        }
        if ($na -lt $nb) { return -1 }
        if ($na -gt $nb) { return 1 }
    }
    return 0
}

# 只允许从 GitHub 官方域名下载更新资产
function Test-GitHubDownloadUrl([string]$url) {
    if ([string]::IsNullOrWhiteSpace($url)) { return $false }
    try { $u = [Uri]$url } catch { return $false }
    if ($u.Scheme -ne 'https') { return $false }
    $allowed = @('github.com', 'api.github.com', 'objects.githubusercontent.com',
                 'github-releases.githubusercontent.com', 'codeload.github.com')
    return ($allowed -contains $u.Host.ToLowerInvariant())
}

function Get-ScriptConfigValue([string]$text, [string]$name) {
    $m = [regex]::Match($text, '(?m)^[ \t]*\$' + [regex]::Escape($name) + '[ \t]*=[ \t]*"([^"]*)"')
    if ($m.Success) { return $m.Groups[1].Value }
    return $null
}

function Set-ScriptConfigValue([string]$text, [string]$name, [string]$value) {
    $pattern = '(?m)^([ \t]*\$' + [regex]::Escape($name) + '[ \t]*=[ \t]*")[^"]*(")'
    $replacement = '${1}' + $value.Replace('$', '$$') + '${2}'
    return [regex]::Replace($text, $pattern, $replacement)
}

# 代理客户端显示名(供检查细节输出)
function Get-ProxyName {
    switch ($PROXY_MODE) {
        'clash-verge' { return 'Clash Verge' }
        'flclash'     { return 'FlClash' }
        'sing-box'    { return 'sing-box' }
        'clashx'      { return 'ClashX' }
        'surge'       { return 'Surge' }
        'wireguard'   { return 'WireGuard' }
        'other'       { return '其他代理客户端' }
        'auto'        { return '代理客户端(auto)' }
        default       { return $PROXY_MODE }
    }
}

# 代理客户端在线判定:按安装时识别的客户端分派;auto 任一命中即活,other 只依赖 TUN
function Test-ProxyAlive {
    switch ($PROXY_MODE) {
        'clash-verge' { return [bool](Get-Process -Name 'verge-mihomo' -ErrorAction SilentlyContinue) }
        'flclash'     { return [bool](Get-Process -Name 'FlClashCore','FlClash' -ErrorAction SilentlyContinue) }
        'sing-box'    { return [bool](Get-Process -Name 'sing-box' -ErrorAction SilentlyContinue) }
        'clashx'      { return [bool](Get-Process -Name 'ClashX' -ErrorAction SilentlyContinue) }
        'surge'       { return [bool](Get-Process -Name 'Surge' -ErrorAction SilentlyContinue) }
        'wireguard'   { return [bool](Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running') }
        'other'       { return $true }
        'auto' {
            if (Get-Process -Name 'verge-mihomo','FlClashCore','FlClash','sing-box','ClashX','Surge' -ErrorAction SilentlyContinue) { return $true }
            return [bool](Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running')
        }
        default { return [bool](Get-Process -Name 'verge-mihomo' -ErrorAction SilentlyContinue) }
    }
}

function Get-Client {
    "client=$PROXY_MODE"
    'clients=clash-verge,flclash,sing-box,clashx,surge,wireguard,other,auto'
}

function Set-ClientMode([string]$Mode) {
    $allowed = @('clash-verge','flclash','sing-box','clashx','surge','wireguard','other','auto')
    if ($allowed -notcontains $Mode) {
        $script:exitCode = 1
        "FAIL:用法: claude-switch.ps1 set-client <clash-verge|flclash|sing-box|clashx|surge|wireguard|other|auto>"
        return
    }
    $selfPath = $PSCommandPath
    if (-not $selfPath) { $selfPath = $MyInvocation.MyCommand.Path }
    if (-not $selfPath -or -not (Test-Path -LiteralPath $selfPath)) {
        $script:exitCode = 1
        'FAIL:无法定位当前核心脚本'
        return
    }
    $modePath = Join-Path $BASE 'proxy-mode.txt'
    $inBase = Test-SelfInBase $selfPath
    $oldText = if ($inBase) { [System.IO.File]::ReadAllText($selfPath) } else { '' }
    $oldModeExists = Test-Path -LiteralPath $modePath
    $oldMode = if ($oldModeExists) { [System.IO.File]::ReadAllText($modePath) } else { '' }
    try {
        New-Item -ItemType Directory -Force -Path $BASE -ErrorAction Stop | Out-Null
        if ($inBase) {
            $newText = Set-ScriptConfigValue $oldText 'PROXY_MODE' $Mode
            if ($newText -eq $oldText -and $PROXY_MODE -ne $Mode) { throw '核心配置行未更新' }
            [System.IO.File]::WriteAllText($selfPath, $newText, (New-Object System.Text.UTF8Encoding($true)))
        } else {
            Save-RuntimeConfig @{ proxy_mode = $Mode }
        }
        [System.IO.File]::WriteAllText($modePath, $Mode, [System.Text.Encoding]::ASCII)
        $script:PROXY_MODE = $Mode
        "OK:代理客户端已设为 $Mode"
        'INFO:保镖会在下一轮巡检读取新模式；非 Clash Verge 客户端请自行配置 Claude 域名规则'
    } catch {
        try {
            if ($inBase -and $oldText) { [System.IO.File]::WriteAllText($selfPath, $oldText, (New-Object System.Text.UTF8Encoding($true))) }
            if ($oldModeExists) { [System.IO.File]::WriteAllText($modePath, $oldMode, [System.Text.Encoding]::ASCII) }
            else { Remove-Item -LiteralPath $modePath -Force -ErrorAction SilentlyContinue }
        } catch {}
        $script:exitCode = 1
        "FAIL:代理客户端写入失败，已回滚:$($_.Exception.Message)"
    }
}

# TUN 在线判定:按客户端模式收敛,防止 Tailscale 等常驻虚拟网卡在 Clash TUN 关闭时掩盖掉线
function Get-TunStatus {
    $upAdapters = @(Get-NetAdapter -ErrorAction SilentlyContinue | Where-Object { $_.Status -eq 'Up' })
    if ($PROXY_MODE -eq 'wireguard') {
        $wgNic = $upAdapters | Where-Object {
            ($_.Name -notmatch '(?i)Tailscale' -and $_.InterfaceDescription -notmatch '(?i)Tailscale') -and
            ($_.InterfaceDescription -match '(?i)(WireGuard|Wintun)' -or $_.Name -match '(?i)^wg')
        } | Select-Object -First 1
        if ($wgNic) { return @{ up = $true; detail = "WireGuard网卡($($wgNic.Name))" } }
        return @{ up = $false; detail = '不在线' }
    }
    $fakeIp = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object {
        $_.IPAddress -match '^198\.1[89]\.' -and (-not $_.AddressState -or [string]$_.AddressState -eq 'Preferred')
    }
    if ($fakeIp) {
        return @{ up = $true; detail = '198.18 假IP段' }
    }
    $mihomoNic = $upAdapters | Where-Object {
        $_.Name -match '^(?i)(Mihomo|Meta|Clash|FlClash)' -or
        $_.InterfaceDescription -match '(?i)(Meta Tunnel|Mihomo|FlClash|Clash)'
    } | Select-Object -First 1
    if ($PROXY_MODE -in @('clash-verge', 'flclash')) {
        if ($mihomoNic) { return @{ up = $true; detail = "Mihomo网卡($($mihomoNic.Name))" } }
        return @{ up = $false; detail = '不在线' }
    }
    if ($PROXY_MODE -in @('sing-box', 'clashx', 'surge')) {
        $clientNic = $upAdapters | Where-Object {
            ($_.Name -notmatch '(?i)Tailscale' -and $_.InterfaceDescription -notmatch '(?i)Tailscale') -and
            ($_.Name -match '(?i)(sing|tun|utun|Clash|Surge|Mihomo|Meta)' -or $_.InterfaceDescription -match '(?i)(sing|Wintun|TAP|TUN|Meta Tunnel|Clash|Surge)')
        } | Select-Object -First 1
        if ($clientNic) { return @{ up = $true; detail = "客户端TUN($($clientNic.Name))" } }
        return @{ up = $false; detail = '不在线' }
    }
    if ($PROXY_MODE -eq 'auto') {
        $mihomoRunning = [bool](Get-Process -Name 'verge-mihomo','FlClashCore','FlClash' -ErrorAction SilentlyContinue)
        $otherRunning  = [bool](Get-Process -Name 'sing-box','ClashX','Surge' -ErrorAction SilentlyContinue) -or
                         [bool](Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running')
        if ($mihomoRunning -and -not $otherRunning) {
            if ($mihomoNic) { return @{ up = $true; detail = "Mihomo网卡($($mihomoNic.Name))" } }
            return @{ up = $false; detail = '不在线' }
        }
    }
    $nic = $upAdapters | Where-Object { $_.InterfaceDescription -match 'TAP|TUN|WireGuard|Wintun|VPN|Tailscale' } | Select-Object -First 1
    if ($nic) { return @{ up = $true; detail = '通用虚拟网卡' } }
    return @{ up = $false; detail = '不在线' }
}
function Test-TunUp {
    $t = Get-TunStatus
    return [bool]$t.up
}

# 代理路径是否可用 —— 与保镖判定「代理离线就立刻杀 Claude」用的是同一组信号。
# 路径失效时任何出站探测都会脱离隧道、用真实 IP 敲门,所以探测必须被闸在路径存活之后:
# 路径可能失效 → 立刻失败、零请求;路径在线 → 隧道内失败「只断不绕」,不是泄漏,可重试。
function Test-ProxyPathAlive {
    return ((Test-ProxyAlive) -and (Test-TunUp))
}

# 网络探测重试。
#
# 只在代理路径存活时才会走到这里(见 Test-ProxyPathAlive):路径在线时请求必然进隧道,
# 隧道内请求失败是「只断不绕」而不是泄漏 —— 这正是上游排障表的结论
# 「节点被重置时隧道内请求失败≠泄漏」。这种情况下重试是安全的:
# 只是把同一个问题多问几次,判定标准与 Fail-Closed 语义完全不变,
# 拿到有效答案立刻返回,不会为「答案不对」而重试。
#
# 路径可能失效时(见 Get-Egress)一个探测包都不发,直接失败 ——
# 那正是保镖会立刻杀 Claude 的条件,不能在可能泄漏的路径上继续敲门。
function Invoke-ProbeText {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [int]$Attempts = 3,
        [int]$TimeoutSec = 8,
        [scriptblock]$Valid = { param($body) return [bool]$body }
    )
    if ($script:ProbePrefetch) {
        $prefetched = Receive-ProbePrefetch "text|$Url"
        if ($null -ne $prefetched) { return [string]$prefetched }
    }
    $body = ''
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        $body = (& curl.exe -s --max-time $TimeoutSec $Url 2>$null) -join "`n"
        if ((& $Valid $body)) { return $body }
        if ($attempt -lt $Attempts) { Start-Sleep -Milliseconds (400 * $attempt) }
    }
    return $body
}

# 取 HTTP 状态码。curl 连不上时 '-w %{http_code}' 输出 000,视为传输失败并重试。
function Invoke-ProbeCode {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [int]$Attempts = 3,
        [int]$TimeoutSec = 8
    )
    if ($script:ProbePrefetch) {
        $prefetched = Receive-ProbePrefetch "code|$Url"
        if ($null -ne $prefetched) { return [string]$prefetched }
    }
    $code = '000'
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        $code = ((& curl.exe -s --max-time $TimeoutSec -o NUL -w '%{http_code}' $Url 2>$null) -join '').Trim()
        if ($code -and $code -ne '000') { return $code }
        if ($attempt -lt $Attempts) { Start-Sleep -Milliseconds (400 * $attempt) }
    }
    return $code
}

# 出口探测确认路径存活后,把后续彼此独立的只读探测(ASN 的 ipinfo→peeringdb、生态可达性的两个站点)
# 放到后台 runspace 并行发出。各检查项仍照常调用 Invoke-Probe*:命中预取结果就直接用,
# 未命中(没有预取,或检查项复核路径失败而根本不发请求)走原有同步探测。判定逻辑与路径复核都不变。
function Start-ProbePrefetch($Egress) {
    Stop-ProbePrefetch
    if (-not $Egress -or $Egress.skipped -or -not (Test-ProxyPathAlive)) { return }
    $definitions = "function Invoke-ProbeText {`n$(${function:Invoke-ProbeText})`n}`nfunction Invoke-ProbeCode {`n$(${function:Invoke-ProbeCode})`n}"
    $jobs = @()
    $dnsToken = ''
    if (-not (Test-CheckDisabled 'DNS')) {
        $dnsToken = ([guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')).Substring(0, 40)
        $dnsDetectUrl = "https://$dnsToken.ipleak.net/dnsdetection/"
        $dnsChain = @'
$results = @{}
$detectBody = Invoke-ProbeText -Url $Url -Attempts 2 -TimeoutSec 6
$results["text|$Url"] = $detectBody
try {
    $detectObj = $detectBody | ConvertFrom-Json -ErrorAction Stop
    if ($detectObj -and $detectObj.ip -is [pscustomobject]) {
        foreach ($rip in @($detectObj.ip.PSObject.Properties.Name | Where-Object { $_ } | Select-Object -First 4)) {
            $rUrl = "https://ipleak.net/json/$([Uri]::EscapeDataString([string]$rip))"
            $results["text|$rUrl"] = Invoke-ProbeText -Url $rUrl -Attempts 2 -TimeoutSec 5
        }
    }
} catch {}
$results
'@
        $jobs += New-ProbePrefetchJob $definitions $dnsChain @($dnsDetectUrl, '') '^text\|https://([0-9a-f]+\.)?ipleak\.net/'
    }
    if ($Egress.ip -and -not (Test-CheckDisabled 'ASN属性')) {
        $ipInfoUrl = "https://ipinfo.io/$([Uri]::EscapeDataString([string]$Egress.ip))/json"
        $asnChain = @'
$results = @{}
$ipInfoResponse = Invoke-ProbeText -Url $Url -TimeoutSec 6
$results["text|$Url"] = $ipInfoResponse
try {
    $ipInfo = $ipInfoResponse | ConvertFrom-Json -ErrorAction Stop
    if ([string]$ipInfo.ip -eq $Ip -and [string]$ipInfo.org -match '^AS(\d+)\s') {
        $networkUrl = "https://www.peeringdb.com/api/net?asn=$($Matches[1])&fields=asn,info_type"
        $results["text|$networkUrl"] = Invoke-ProbeText -Url $networkUrl -TimeoutSec 6
    }
} catch {}
$results
'@
        $jobs += New-ProbePrefetchJob $definitions $asnChain @($ipInfoUrl, [string]$Egress.ip) '^text\|https://(ipinfo\.io|www\.peeringdb\.com)/'
    }
    if (-not (Test-CheckDisabled '生态可达性')) {
        foreach ($url in @('https://claude.ai/', 'https://api.anthropic.com/v1/models')) {
            $jobs += New-ProbePrefetchJob $definitions '@{ ("code|" + $Url) = (Invoke-ProbeCode -Url $Url) }' @($url, '') ('^code\|' + [regex]::Escape($url) + '$')
        }
    }
    $script:ProbePrefetch = @{ Jobs = $jobs; Results = @{}; DnsToken = $dnsToken }
}

function New-ProbePrefetchJob([string]$Definitions, [string]$Body, [object[]]$Arguments, [string]$KeyPattern) {
    $shell = [powershell]::Create()
    [void]$shell.AddScript("param(`$Url, `$Ip)`n$Definitions`n$Body")
    foreach ($argument in $Arguments) { [void]$shell.AddArgument($argument) }
    return @{ Shell = $shell; Handle = $shell.BeginInvoke(); Pattern = $KeyPattern; Done = $false }
}

function Receive-ProbePrefetch([string]$Key) {
    $cache = $script:ProbePrefetch
    if (-not $cache) { return $null }
    if (-not $cache.Results.ContainsKey($Key)) {
        foreach ($job in @($cache.Jobs | Where-Object { -not $_.Done -and $Key -match $_.Pattern })) {
            try {
                foreach ($output in @($job.Shell.EndInvoke($job.Handle))) {
                    $table = $output.psobject.BaseObject
                    if ($table -is [hashtable]) { foreach ($k in @($table.Keys)) { $cache.Results[$k] = $table[$k] } }
                }
            } catch {} finally {
                $job.Done = $true
                $job.Shell.Dispose()
            }
        }
    }
    if (-not $cache.Results.ContainsKey($Key)) { return $null }
    $value = $cache.Results[$Key]
    $cache.Results.Remove($Key)
    return $value
}

function Stop-ProbePrefetch {
    $cache = $script:ProbePrefetch
    $script:ProbePrefetch = $null
    if (-not $cache) { return }
    foreach ($job in @($cache.Jobs | Where-Object { -not $_.Done })) {
        try { $job.Shell.Stop() } catch {}
        $job.Shell.Dispose()
        $job.Done = $true
    }
}

function Get-Egress {
    # 代理路径失效时立刻放弃,一个探测包都不发:
    # 这正是保镖会立刻杀 Claude 的条件,此时再发请求等于用真实 IP 再敲一次门。
    if (-not (Test-ProxyPathAlive)) {
        return @{ ip = ''; loc = ''; skipped = $true }
    }
    # 响应为空时不能对空数组调用 Trim —— 链路失败必须降级成"未取到",
    # 而不是抛异常中断整轮检查(ip/loc 都取到才算有效答案)。
    $trace = Invoke-ProbeText -Url 'https://www.cloudflare.com/cdn-cgi/trace' -Valid {
            param($body)
            return ($body -match '(?m)^ip=') -and ($body -match '(?m)^loc=')
        }
    $ip  = (((($trace -split "`n") | Where-Object { $_ -like 'ip=*' })  -replace 'ip=','') -join '').Trim()
    $loc = (((($trace -split "`n") | Where-Object { $_ -like 'loc=*' }) -replace 'loc=','') -join '').Trim()
    return @{ ip = $ip; loc = $loc; skipped = $false }
}

# ============ 地区映射(安装器与 set-region 共用同一套目标) ============
$COUNTRY_TZ = @{
    'US' = 'America/New_York'; 'JP' = 'Asia/Tokyo'; 'TW' = 'Asia/Taipei'
    'HK' = 'Asia/Hong_Kong'; 'SG' = 'Asia/Singapore'; 'GB' = 'Europe/London'
    'DE' = 'Europe/Berlin'; 'FR' = 'Europe/Paris'; 'KR' = 'Asia/Seoul'
    'AU' = 'Australia/Sydney'; 'CA' = 'America/Toronto'; 'MY' = 'Asia/Kuala_Lumpur'
}
$COUNTRY_LOCALE = @{
    'US' = 'en-US'; 'JP' = 'ja-JP'; 'TW' = 'zh-TW'
    'HK' = 'zh-HK'; 'SG' = 'en-SG'; 'GB' = 'en-GB'
    'DE' = 'de-DE'; 'FR' = 'fr-FR'; 'KR' = 'ko-KR'
    'AU' = 'en-AU'; 'CA' = 'en-CA'; 'MY' = 'ms-MY'
}

function Get-CountryDefaultTimeZone([string]$country) {
    $c = ([string]$country).Trim().ToUpperInvariant()
    if ($COUNTRY_TZ.ContainsKey($c)) { return $COUNTRY_TZ[$c] }
    return 'America/New_York'
}

function Get-CountryLocale([string]$country) {
    $c = ([string]$country).Trim().ToUpperInvariant()
    if ($COUNTRY_LOCALE.ContainsKey($c)) { return $COUNTRY_LOCALE[$c] }
    return 'en-US'
}

# IANA -> Windows 时区 ID;未知返回 $null,由调用方决定回退
function ConvertTo-WindowsTimeZone([string]$iana) {
    $map = @{
        'America/New_York'    = 'Eastern Standard Time'
        'America/Chicago'     = 'Central Standard Time'
        'America/Denver'      = 'Mountain Standard Time'
        'America/Los_Angeles' = 'Pacific Standard Time'
        'America/Toronto'     = 'Eastern Standard Time'
        'America/Vancouver'   = 'Pacific Standard Time'
        'Asia/Singapore'      = 'Singapore Standard Time'
        'Asia/Tokyo'          = 'Tokyo Standard Time'
        'Asia/Seoul'          = 'Korea Standard Time'
        'Asia/Taipei'         = 'Taipei Standard Time'
        'Asia/Hong_Kong'      = 'China Standard Time'
        'Europe/London'       = 'GMT Standard Time'
        'Europe/Berlin'       = 'W. Europe Standard Time'
        'Europe/Paris'        = 'Romance Standard Time'
        'Australia/Sydney'    = 'AUS Eastern Standard Time'
        'Australia/Melbourne' = 'AUS Eastern Standard Time'
        'Asia/Kuala_Lumpur'   = 'Singapore Standard Time'
    }
    if ($map.ContainsKey($iana)) { return $map[$iana] }
    return $null
}

# Windows 时区 ID -> IANA 时区(供进程级 TZ 环境变量注入使用)
function ConvertFrom-WindowsTimeZone([string]$winTz, [string]$countryFallback = 'US') {
    if ($IANA_TZ_OVERRIDE) { return $IANA_TZ_OVERRIDE }
    $c = ([string]$countryFallback).Trim().ToUpperInvariant()
    if ($winTz -eq 'Eastern Standard Time' -and $c -eq 'CA') { return 'America/Toronto' }
    if ($winTz -eq 'Pacific Standard Time' -and $c -eq 'CA') { return 'America/Vancouver' }
    if ($winTz -eq 'Singapore Standard Time' -and $c -eq 'MY') { return 'Asia/Kuala_Lumpur' }
    $rev = @{
        'Eastern Standard Time'     = 'America/New_York'
        'Central Standard Time'     = 'America/Chicago'
        'Mountain Standard Time'    = 'America/Denver'
        'Pacific Standard Time'     = 'America/Los_Angeles'
        'Singapore Standard Time'   = 'Asia/Singapore'
        'Tokyo Standard Time'       = 'Asia/Tokyo'
        'Korea Standard Time'       = 'Asia/Seoul'
        'Taipei Standard Time'      = 'Asia/Taipei'
        'China Standard Time'       = 'Asia/Hong_Kong'
        'GMT Standard Time'         = 'Europe/London'
        'W. Europe Standard Time'   = 'Europe/Berlin'
        'Romance Standard Time'     = 'Europe/Paris'
        'AUS Eastern Standard Time' = 'Australia/Sydney'
    }
    if ($winTz -and $rev.ContainsKey($winTz)) { return $rev[$winTz] }
    return (Get-CountryDefaultTimeZone $c)
}

# 只记录第一次改成目标前的原时区,之后不覆盖,供 shield-restore 还原
function Save-HomeTimeZone([string]$TargetTz) {
    $homeTzFile = Join-Path $BASE 'home-tz.txt'
    $current = (Get-TimeZone).Id
    if ((Test-Path -LiteralPath $homeTzFile) -or -not $current -or $current -eq $TargetTz) { return }
    try { [System.IO.File]::WriteAllText($homeTzFile, $current, (New-Object System.Text.UTF8Encoding($false))) } catch {}
}

# 记录关闭 IPv6 前各网卡的 ms_tcpip6 原状态(仅在尚无快照且至少有一块网卡启用 IPv6 时记录,避免把护盾态当成日常态)
function Save-HomeIpv6State {
    $stateFile = Join-Path $BASE 'ipv6-state.json'
    if (Test-Path -LiteralPath $stateFile) { return }
    $bindings = @(Get-NetAdapterBinding -ComponentID ms_tcpip6 -ErrorAction SilentlyContinue)
    if (-not @($bindings | Where-Object { $_.Enabled }).Count) { return }
    $map = [ordered]@{}
    foreach ($b in $bindings) {
        if ($b -and $b.Name) { $map[[string]$b.Name] = [bool]$b.Enabled }
    }
    if ($map.Count -eq 0) { return }
    try {
        New-Item -ItemType Directory -Force -Path $BASE -ErrorAction Stop | Out-Null
        $payload = ([pscustomobject]@{ version = 1; adapters = [pscustomobject]$map }) | ConvertTo-Json -Depth 4
        [System.IO.File]::WriteAllText($stateFile, $payload, (New-Object System.Text.UTF8Encoding($false)))
    } catch {}
}

# 保镖健康度检查:不仅看计划任务是否存在,还实查运行态/心跳与 72h·电池策略隐患
function Get-BodyguardHealth {
    $bg = Get-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue
    if (-not $bg) {
        return @{ state = 'fail'; detail = '未安装'; installed = $false; running = $false; legacyPolicy = $false }
    }
    $legacyPolicy = $false
    if ($bg.Settings) {
        $limit = [string]$bg.Settings.ExecutionTimeLimit
        if ($bg.Settings.StopIfGoingOnBatteries -or $bg.Settings.DisallowStartIfOnBatteries -or ($limit -and $limit -notin @('PT0S','P0D'))) {
            $legacyPolicy = $true
        }
    }
    $hbAlive = $false
    $hbFile = Join-Path $BASE 'bodyguard-heartbeat.txt'
    if (Test-Path -LiteralPath $hbFile -PathType Leaf) {
        try {
            $raw = ([System.IO.File]::ReadAllText($hbFile, [System.Text.Encoding]::ASCII)).Trim()
            if ($raw -match '^(\d+)\s+(\d+)$') {
                $hbPid = [int]$Matches[1]
                $hbTs  = [long]$Matches[2]
                $age   = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - $hbTs
                if ($age -ge -5 -and $age -le 35) {
                    $proc = Get-Process -Id $hbPid -ErrorAction SilentlyContinue
                    if ($proc -and $proc.ProcessName -match '^(powershell|pwsh)$') { $hbAlive = $true }
                }
            }
        } catch {}
    }
    $taskRunning = ([string]$bg.State -eq 'Running')
    $running = ($hbAlive -or $taskRunning)
    if (-not $running) {
        return @{ state = 'fail'; detail = '已注册但未运行(可能超时或被系统停止,点一键修复重启)'; installed = $true; running = $false; legacyPolicy = $legacyPolicy }
    }
    if ($legacyPolicy) {
        return @{ state = 'warn'; detail = '在线(含72h/电池停止旧限制,建议点一键修复升级)'; installed = $true; running = $true; legacyPolicy = $true }
    }
    return @{ state = 'ok'; detail = '在线(10s巡检)'; installed = $true; running = $true; legacyPolicy = $false }
}

# 多源出口探测(与安装器同源):ipinfo -> ipapi -> ifconfig -> Cloudflare trace
function Get-GeoInfo {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $ua = 'claude-switcher/1.0'
    $diag = New-Object System.Collections.Generic.List[string]
    $first = $null
    $firstTzDefault = $false
    $sources = @(
        @{ Name = 'ipinfo'; Url = 'https://ipinfo.io/json'; Country = 'country'; Timezone = 'timezone' },
        @{ Name = 'ipapi'; Url = 'https://ipapi.co/json/'; Country = 'country_code'; Timezone = 'timezone' },
        @{ Name = 'ifconfig'; Url = 'https://ifconfig.co/json'; Country = 'country_iso'; Timezone = 'timezone' }
    )
    foreach ($src in $sources) {
        try {
            $data = Invoke-RestMethod -Uri $src.Url -UserAgent $ua -TimeoutSec 8 -ErrorAction Stop
        } catch {
            $reason = '超时/连接失败'
            try {
                if ($_.Exception.Response) { $reason = 'HTTP ' + [int]$_.Exception.Response.StatusCode }
            } catch {}
            $diag.Add("$($src.Name) $reason")
            continue
        }
        $ccField = $src.Country
        $tzField = $src.Timezone
        $cc = ([string]$data.$ccField).Trim().ToUpperInvariant()
        $tz = [string]$data.$tzField
        if ($cc -notmatch '^[A-Z]{2}$') { $diag.Add("$($src.Name) 国家字段无效"); continue }
        $usedDefault = $false
        if ($tz -notmatch '^[A-Za-z_]+/[A-Za-z_]+(/[A-Za-z_]+)?$') {
            $tz = Get-CountryDefaultTimeZone $cc
            $usedDefault = $true
        }
        if (-not $first) {
            $first = [pscustomobject]@{ Country = $cc; Timezone = $tz; Source = $src.Name; Ip = [string]$data.ip }
            $firstTzDefault = $usedDefault
        } elseif ($cc -ne $first.Country) {
            $diag.Add("出口国家不一致($($first.Source)=$($first.Country) $($src.Name)=$cc),采用优先级更高的 $($first.Source)")
        }
    }
    try {
        $trace = Invoke-WebRequest -Uri 'https://www.cloudflare.com/cdn-cgi/trace' -UserAgent $ua -TimeoutSec 8 -UseBasicParsing -ErrorAction Stop
        $loc = $null; $tip = ''
        foreach ($line in ($trace.Content -split "`r?`n")) {
            if ($line -match '^loc=([A-Za-z]{2})$') { $loc = $Matches[1].ToUpperInvariant() }
            if ($line -match '^ip=(\S+)$') { $tip = $Matches[1] }
        }
        if ($loc -and $loc -match '^[A-Z]{2}$') {
            $tz = Get-CountryDefaultTimeZone $loc
            if (-not $first) {
                $first = [pscustomobject]@{ Country = $loc; Timezone = $tz; Source = 'cloudflare'; Ip = $tip }
                $firstTzDefault = $true
            } elseif ($loc -ne $first.Country) {
                $diag.Add("出口国家不一致($($first.Source)=$($first.Country) cloudflare=$loc),采用优先级更高的 $($first.Source)")
            }
        } else {
            $diag.Add('cloudflare loc 无效')
        }
    } catch {
        $reason = '超时/连接失败'
        try {
            if ($_.Exception.Response) { $reason = 'HTTP ' + [int]$_.Exception.Response.StatusCode }
        } catch {}
        $diag.Add("cloudflare $reason")
    }
    $outCountry = ''; $outTz = ''; $outSource = ''; $outIp = '未知'
    if ($first) {
        $outCountry = $first.Country
        $outTz = $first.Timezone
        $outSource = $first.Source
        if ($first.Ip) { $outIp = $first.Ip }
    }
    return [pscustomobject]@{
        Country = $outCountry
        Timezone = $outTz
        Source = $outSource
        Ip = $outIp
        TimezoneDefault = $firstTzDefault
        Diagnostics = ($diag -join '; ')
    }
}

function Invoke-SetRegion {
    $geo = Get-GeoInfo
    if (-not $geo.Country) {
        "FAIL:所有出口探测源均失败($($geo.Diagnostics))"
        "HINT:请检查代理规则/稍后重试"
        return
    }
    $country = ([string]$geo.Country).Trim().ToUpperInvariant()
    if ($country -eq 'CN') {
        "FAIL:出口国家为 CN,已中止"
        return
    }

    $ianaTz = [string]$geo.Timezone
    $tzFromDefault = [bool]$geo.TimezoneDefault
    if ([string]::IsNullOrWhiteSpace($ianaTz) -or $ianaTz -notmatch '^[A-Za-z_]+/[A-Za-z_]+(/[A-Za-z_]+)?$') {
        $ianaTz = Get-CountryDefaultTimeZone $country
        $tzFromDefault = $true
    }
    $tzId = ConvertTo-WindowsTimeZone $ianaTz
    if ([string]::IsNullOrWhiteSpace($tzId)) {
        $tzId = 'Eastern Standard Time'
        "WARN:时区 $ianaTz 无 Windows 映射,回退 Eastern Standard Time"
    }
    $locale = Get-CountryLocale $country

    $selfPath = $PSCommandPath
    if (-not $selfPath) { $selfPath = $MyInvocation.MyCommand.Path }
    if (-not $selfPath -or -not (Test-Path -LiteralPath $selfPath)) {
        "FAIL:无法定位当前脚本,配置未写入"
        return
    }
    try {
        if (Test-SelfInBase $selfPath) {
            $text = [System.IO.File]::ReadAllText($selfPath)
            $text = Set-ScriptConfigValue $text 'COUNTRY_TARGET' $country
            $text = Set-ScriptConfigValue $text 'TZ_TARGET' $tzId
            $text = Set-ScriptConfigValue $text 'LOCALE_TARGET' $locale
            [System.IO.File]::WriteAllText($selfPath, $text, (New-Object System.Text.UTF8Encoding($true)))
        } else {
            Save-RuntimeConfig @{
                country_target = $country
                tz_target      = $tzId
                iana_tz_target = $ianaTz
                locale_target  = $locale
            }
        }
    } catch {
        "FAIL:写入配置失败:$($_.Exception.Message)"
        return
    }
    $script:COUNTRY_TARGET = $country
    $script:TZ_TARGET = $tzId
    $script:IANA_TZ_OVERRIDE = $ianaTz
    $script:LOCALE_TARGET = $locale

    Save-HomeTimeZone $tzId
    $tmp = "$env:TEMP\claude-set-region-$([guid]::NewGuid().Guid).ps1"
    try {
        $tzEsc = $tzId.Replace("'", "''")
        $tmpScript = "`$ErrorActionPreference = 'Stop'`r`nSet-TimeZone -Id '$tzEsc'`r`nif ((Get-TimeZone).Id -ne '$tzEsc') { exit 1 }`r`n"
        $tmpScript | Out-File -Encoding utf8 $tmp
        $proc = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$tmp`""
        if ($proc -and $proc.ExitCode -eq 0) {
            "OK:时区已设置为 $tzId"
        } else {
            "WARN:时区未修改,请手动设置"
        }
    } catch {
        "WARN:时区未修改,请手动设置"
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }

    try {
        Set-Culture -CultureInfo $locale -ErrorAction Stop
        "OK:地区格式已设置为 $locale"
    } catch {
        "WARN:地区格式未修改,请手动在「区域格式」中修改"
    }

    "OK:region=$country tz=$ianaTz locale=$locale source=$($geo.Source)"
    if ($tzFromDefault) { "INFO:时区使用国家默认(可手动改)" }
}

# 从 STUN Binding 响应中取 (XOR-)MAPPED-ADDRESS 的 IPv4
function ConvertFrom-StunResponse([byte[]]$Response) {
    $pos = 20
    while ($pos + 4 -le $Response.Length) {
        $atype = ([int]$Response[$pos] -shl 8) -bor $Response[$pos + 1]
        $alen  = ([int]$Response[$pos + 2] -shl 8) -bor $Response[$pos + 3]
        if (($atype -eq 0x20 -or $atype -eq 0x01) -and $alen -ge 8 -and $pos + 12 -le $Response.Length -and $Response[$pos + 5] -eq 0x01) {
            $ipBytes = @($Response[$pos + 8], $Response[$pos + 9], $Response[$pos + 10], $Response[$pos + 11])
            if ($atype -eq 0x20) {
                $cookie = [byte[]](0x21, 0x12, 0xA4, 0x42)
                for ($i = 0; $i -lt 4; $i++) { $ipBytes[$i] = $ipBytes[$i] -bxor $cookie[$i] }
            }
            return ($ipBytes -join '.')
        }
        $pos += 4 + $alen + ((4 - ($alen % 4)) % 4)
    }
    return $null
}

# STUN 探针(纯 .NET UDP,手工构造 RFC5389 报文):所有服务器同时发出、共用一个超时窗口,按服务器顺序去重返回
function Get-StunPublicIPs([object[]]$Servers, [int]$TimeoutMs = 2000) {
    $probes = @()
    foreach ($s in $Servers) {
        $addr = $null
        try { $addr = [System.Net.Dns]::GetHostAddresses([string]$s[0]) | Where-Object { $_.AddressFamily -eq 'InterNetwork' } | Select-Object -First 1 } catch {}
        if (-not $addr) { continue }
        $udp = New-Object System.Net.Sockets.UdpClient
        try {
            $txn = New-Object byte[] 12; (New-Object Random).NextBytes($txn)
            $req = [byte[]](0x00, 0x01, 0x00, 0x00) + [byte[]](0x21, 0x12, 0xA4, 0x42) + $txn
            $udp.Connect($addr, [int]$s[1])
            [void]$udp.Send($req, $req.Length)
            $probes += @{ Udp = $udp; Receive = $udp.ReceiveAsync() }
        } catch { $udp.Close() }
    }
    try {
        $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
        while ($true) {
            $pending = @($probes | Where-Object { -not $_.Receive.IsCompleted } | ForEach-Object { $_.Receive })
            $left = [int]($deadline - [DateTime]::UtcNow).TotalMilliseconds
            if ($pending.Count -eq 0 -or $left -le 0) { break }
            [void][System.Threading.Tasks.Task]::WaitAny([System.Threading.Tasks.Task[]]$pending, $left)
        }
        $found = @()
        foreach ($p in $probes) {
            if ($p.Receive.Status -ne [System.Threading.Tasks.TaskStatus]::RanToCompletion) { continue }
            $ip = ConvertFrom-StunResponse $p.Receive.Result.Buffer
            if ($ip -and ($found -notcontains $ip)) { $found += $ip }
        }
        return $found
    } finally {
        foreach ($p in $probes) { $p.Udp.Close() }
    }
}

function Test-WebRTC($exitIp) {
    $servers = @(@("stun.miwifi.com", 3478), @("stun.chat.bilibili.com", 3478), @("stun.l.google.com", 19302), @("stun.cloudflare.com", 3478))
    $found = @(Get-StunPublicIPs $servers)
    # STUN 无响应不等于没有泄漏:可能只是节点不支持 UDP,或规则拦了探测目标却漏了别的 UDP 出口
    if ($found.Count -eq 0) { return @{ code = 'warn'; detail = 'STUN 无响应,未确认 UDP 出口(不等于零暴露)' } }
    $leak = $found | Where-Object { $_ -ne $exitIp }
    if ($leak) { return @{ code = 'fail'; detail = "暴露非代理IP: $($leak -join ',')" } }
    return @{ code = 'ok'; detail = '仅暴露代理IP' }
}

function Find-ClashVerge {
    # 优先取正在运行进程的真实路径(最可靠),再回退到常见安装目录
    $running = Get-Process -Name 'clash-verge','Clash Verge' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path } | Select-Object -First 1
    if ($running) { return $running.Path }
    $roots = @("$env:LOCALAPPDATA\Programs", $env:ProgramFiles,
               [Environment]::GetEnvironmentVariable('ProgramFiles(x86)'))
    foreach ($root in $roots) {
        if (-not $root) { continue }
        $hit = Get-ChildItem $root -Recurse -Filter "Clash Verge*.exe" -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

# 切换器管理的 13 个 Claude 域名 + 3 个 STUN 端口
$CLAUDE_DOMAINS = @(
    'claude.ai', 'claude.com', 'anthropic.com', 'claudeusercontent.com',
    'challenges.cloudflare.com', 'datadoghq.com', 'segment.io', 'stripe.com',
    'stripe.network', 'stripecdn.com', 'intercom.io', 'intercomcdn.com', 'intercomassets.com'
)
$CLAUDE_PORTS = @('3478', '5349', '19302')

# 读取 YAML 中第一个 key: value 标量(用于 clash-verge.yaml / config.yaml / verge.yaml 的顶层字段)
function Get-YamlScalar([string]$path, [string]$key) {
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    try {
        foreach ($line in Get-Content -LiteralPath $path -Encoding UTF8 -ErrorAction Stop) {
            if ($line -match ('^\s*' + [regex]::Escape($key) + '\s*:\s*(.*?)\s*$')) {
                $v = $Matches[1].Trim()
                $v = $v -replace '\s+#.*$', ''
                $v = $v.Trim('"').Trim("'")
                return $v
            }
        }
    } catch {}
    return $null
}

function Get-FlClashControllerFile {
    return (Join-Path $BASE 'flclash-controller.txt')
}

function Get-FlClashControllerOverride {
    $path = Get-FlClashControllerFile
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    try { return ([System.IO.File]::ReadAllText($path)).Trim() } catch { return '' }
}

function Get-FlClashConfigPath {
    foreach ($yaml in @(
        (Join-Path $env:APPDATA 'com.follow\clash\config.yaml'),
        (Join-Path $env:LOCALAPPDATA 'com.follow\clash\config.yaml')
    )) {
        if (Test-Path -LiteralPath $yaml) { return $yaml }
    }
    return $null
}

# 只接受回环地址。裸端口视为 127.0.0.1，0.0.0.0 改写到本机，其它主机拒绝。
function ConvertTo-LoopbackClashEndpoint([string]$addr, [string]$secret, [string]$source) {
    if ([string]::IsNullOrWhiteSpace($addr)) { return $null }
    $addr = $addr.Trim()
    if ($addr -match '^unix://' -or $addr -match '^/' -or $addr -match '^\\') { return $null }
    if ($addr -match '^(?i)https://') { return $null }
    if ($addr -match '^[0-9]{1,5}$') { $addr = '127.0.0.1:' + $addr }
    $addr = $addr -replace '^0\.0\.0\.0:', '127.0.0.1:'
    $addr = $addr -replace '^\[::\]:', '127.0.0.1:'
    $addr = $addr -replace '^:', '127.0.0.1:'
    if ($addr -notmatch '^https?://') { $addr = 'http://' + $addr }
    $uri = $null
    try { $uri = [Uri]$addr } catch { return $null }
    if ($uri.Scheme -ne 'http') { return $null }
    $hostName = $uri.Host.Trim('[]').ToLowerInvariant()
    if ($hostName -ne '127.0.0.1' -and $hostName -ne 'localhost' -and $hostName -ne '::1') { return $null }
    $port = [int]$uri.Port
    if ($port -lt 1 -or $port -gt 65535) { return $null }
    $headers = @{}
    if (-not [string]::IsNullOrWhiteSpace($secret)) { $headers['Authorization'] = 'Bearer ' + $secret }
    return @{ base = ('http://127.0.0.1:' + $port); headers = $headers; source = $source }
}

function Test-ClashVersionEndpoint($api) {
    if (-not $api) { return $false }
    try {
        $params = @{ Uri = ($api.base + '/version'); TimeoutSec = 2; ErrorAction = 'Stop' }
        if ($api.headers.Count -gt 0) { $params['Headers'] = $api.headers }
        $version = Invoke-RestMethod @params
        return [bool]($version -and $version.version)
    } catch { return $false }
}

function Get-FlClashListenCandidates {
    $candidates = @('127.0.0.1:9090')
    try {
        $procs = @(Get-Process -Name 'FlClash','FlClashCore' -ErrorAction SilentlyContinue)
        if ($procs) {
            $ids = @($procs | ForEach-Object { $_.Id })
            $listen = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object {
                ($ids -contains $_.OwningProcess) -and (@('127.0.0.1','::1','0.0.0.0','::') -contains $_.LocalAddress)
            })
            foreach ($port in @($listen | ForEach-Object { $_.LocalPort } | Sort-Object -Unique)) {
                if ($port -and [int]$port -ne 9090) { $candidates += ('127.0.0.1:' + $port) }
            }
        }
    } catch {}
    return $candidates
}

function Test-FlClashClientSelected {
    if ($PROXY_MODE -eq 'flclash') { return $true }
    if ($PROXY_MODE -ne 'auto') { return $false }
    return [bool](Get-Process -Name 'FlClashCore','FlClash' -ErrorAction SilentlyContinue)
}

# FlClash 默认外部控制器是 127.0.0.1:9090。优先用户填写，其次它自己的 config.yaml，最后才探测默认端口和该进程的本机监听口。
function Get-FlClashApi {
    $yaml = Get-FlClashConfigPath
    $secret = ''
    if ($yaml) { $secret = Get-YamlScalar $yaml 'secret' }
    $override = Get-FlClashControllerOverride
    if ($override) {
        $chosen = ConvertTo-LoopbackClashEndpoint $override $secret 'flclash-override'
        if ($chosen) { return $chosen }
    }
    if ($yaml) {
        $addr = Get-YamlScalar $yaml 'external-controller'
        $secret = Get-YamlScalar $yaml 'secret'
        if (-not [string]::IsNullOrWhiteSpace($addr)) {
            $chosen = ConvertTo-LoopbackClashEndpoint $addr $secret 'flclash-config'
            if ($chosen) { return $chosen }
            return $null
        }
    }
    foreach ($candidate in @(Get-FlClashListenCandidates)) {
        $source = 'flclash-listen'
        if ($candidate -eq '127.0.0.1:9090') { $source = 'flclash-9090' }
        $chosen = ConvertTo-LoopbackClashEndpoint $candidate $secret $source
        if ($chosen -and (Test-ClashVersionEndpoint $chosen)) { return $chosen }
    }
    return $null
}

function Get-FlClashControllerSetting {
    $override = Get-FlClashControllerOverride
    if ([string]::IsNullOrWhiteSpace($override)) { 'port=auto'; return }
    $chosen = ConvertTo-LoopbackClashEndpoint $override '' 'flclash-override'
    if ($chosen) { 'port=' + ($chosen.base -replace '^http://', ''); return }
    'port=auto'
}

function Set-FlClashControllerSetting([string]$Value) {
    $raw = ''
    if ($null -ne $Value) { $raw = $Value.Trim() }
    $path = Get-FlClashControllerFile
    if ([string]::IsNullOrWhiteSpace($raw) -or $raw -eq 'auto') {
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
        'OK:FlClash 控制端口改为自动发现'
        return
    }
    $chosen = ConvertTo-LoopbackClashEndpoint $raw '' 'flclash-override'
    if (-not $chosen) {
        $script:exitCode = 1
        'FAIL:只接受本机端口，例如 9090 或 127.0.0.1:9090'
        return
    }
    $stored = $chosen.base -replace '^http://', ''
    $dir = Split-Path -Parent $path
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    [System.IO.File]::WriteAllText($path, $stored, (New-Object System.Text.UTF8Encoding($false)))
    "OK:FlClash 控制端口已设为 $stored"
}

function Get-NamedPipeNames {
    try {
        return @([System.IO.Directory]::GetFiles('\\.\pipe\') | ForEach-Object { [System.IO.Path]::GetFileName($_) })
    } catch {
        return @(Get-ChildItem -Path '\\.\pipe\' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
    }
}

# Clash Verge Rev 关闭 HTTP 外部控制时,内核仍在命名管道上提供同一套 REST 接口。
# 服务模式下实际管道名与配置里的 external-controller-pipe 只差中间段(如 sidecar-release / production),
# 所以先找配置的原名,找不到再只接受 verge-mihomo- 开头、哈希后缀相同的唯一候选。
function Get-ClashPipeApi {
    foreach ($name in @('clash-verge.yaml', 'config.yaml')) {
        $yaml = Join-Path $CLASH_DIR $name
        $configured = Get-YamlScalar $yaml 'external-controller-pipe'
        if ([string]::IsNullOrWhiteSpace($configured)) { continue }
        $configuredName = ($configured -replace '^\\\\\.\\pipe\\', '').Trim()
        if ($configuredName -notmatch '^[A-Za-z0-9._-]{1,200}$') { continue }
        $existing = @(Get-NamedPipeNames)
        $pipe = $null
        if ($existing -contains $configuredName) {
            $pipe = $configuredName
        } elseif ($configuredName -match '-([0-9a-fA-F]{16,})$') {
            $suffix = $Matches[1]
            $similar = @($existing | Where-Object { $_ -match ('^verge-mihomo-[A-Za-z0-9-]+-' + $suffix + '$') })
            if ($similar.Count -eq 1) { $pipe = $similar[0] }
        }
        if (-not $pipe) { continue }
        $secret = Get-YamlScalar $yaml 'secret'
        $headers = @{}
        if (-not [string]::IsNullOrWhiteSpace($secret)) { $headers['Authorization'] = 'Bearer ' + $secret }
        return @{ base = "pipe:$pipe"; pipe = $pipe; headers = $headers; source = "$name(pipe)" }
    }
    return $null
}

# 经命名管道发 HTTP/1.0 GET:服务端不会用分块编码,回完即关闭连接,读到 EOF 就是完整响应
function Invoke-ClashPipeRequest([string]$Pipe, [string]$Path, [hashtable]$Headers, [int]$TimeoutSec = 8) {
    $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Pipe, [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
    try {
        try {
            $client.Connect([Math]::Min(2000, [Math]::Max(1, $TimeoutSec) * 1000))
        } catch {
            return @{ ok = $false; reason = 'pipe-unavailable'; data = $null }
        }
        $lines = @("GET $Path HTTP/1.0", 'Host: localhost')
        foreach ($key in @($Headers.Keys)) { $lines += "${key}: $($Headers[$key])" }
        $request = [System.Text.Encoding]::UTF8.GetBytes(($lines -join "`r`n") + "`r`n`r`n")
        $client.Write($request, 0, $request.Length)
        $client.Flush()
        $buffer = New-Object System.IO.MemoryStream
        $chunk = New-Object byte[] 65536
        $deadline = [DateTime]::UtcNow.AddSeconds([Math]::Max(1, $TimeoutSec))
        while ($true) {
            $read = $client.ReadAsync($chunk, 0, $chunk.Length)
            $left = [int][Math]::Max(0, ($deadline - [DateTime]::UtcNow).TotalMilliseconds)
            if (-not $read.Wait($left)) { return @{ ok = $false; reason = 'timeout'; data = $null } }
            if ($read.Result -le 0) { break }
            $buffer.Write($chunk, 0, $read.Result)
        }
        $text = [System.Text.Encoding]::UTF8.GetString($buffer.ToArray())
        $split = $text.IndexOf("`r`n`r`n")
        if ($split -lt 0 -or $text -notmatch '^HTTP/1\.[01] (\d{3})') { return @{ ok = $false; reason = 'error'; data = $null } }
        $status = [int]$Matches[1]
        if ($status -eq 401 -or $status -eq 403) { return @{ ok = $false; reason = 'auth-failed'; data = $null } }
        if ($status -lt 200 -or $status -ge 300) { return @{ ok = $false; reason = "http-$status"; data = $null } }
        if ($text.Substring(0, $split) -match '(?im)^Transfer-Encoding:') { return @{ ok = $false; reason = 'error'; data = $null } }
        return @{ ok = $true; reason = ''; data = ($text.Substring($split + 4) | ConvertFrom-Json -ErrorAction Stop) }
    } catch {
        return @{ ok = $false; reason = 'error'; data = $null }
    } finally {
        $client.Dispose()
    }
}

# 解析 Clash 控制接口:Clash Verge 优先命名管道,其次 HTTP;Windows 版不使用 unix socket
function Get-ClashApi {
    if (Test-FlClashClientSelected) { return Get-FlClashApi }
    $pipeApi = Get-ClashPipeApi
    if ($pipeApi) { return $pipeApi }
    foreach ($name in @('clash-verge.yaml', 'config.yaml', 'verge.yaml')) {
        $yaml = Join-Path $CLASH_DIR $name
        if (-not (Test-Path -LiteralPath $yaml)) { continue }
        $addr = Get-YamlScalar $yaml 'external-controller'
        if ([string]::IsNullOrWhiteSpace($addr)) { continue }
        if ($addr -match '^unix://' -or $addr -match '^/') { continue }
        $addr = $addr -replace '^0\.0\.0\.0:', '127.0.0.1:'
        $addr = $addr -replace '^:', '127.0.0.1:'
        if ($addr -notmatch '^https?://') { $addr = 'http://' + $addr }
        $base = $addr.TrimEnd('/')
        $secret = Get-YamlScalar $yaml 'secret'
        $headers = @{}
        if (-not [string]::IsNullOrWhiteSpace($secret)) { $headers['Authorization'] = 'Bearer ' + $secret }
        return @{ base = $base; headers = $headers; source = $name }
    }
    if ($PROXY_MODE -eq 'auto') { return Get-FlClashApi }
    return $null
}

# 把控制接口异常归类成可执行的原因,供规则检查与 rules-status 复用
function Get-ClashApiError($err) {
    $status = $null
    try { if ($err.Response) { $status = [int]$err.Response.StatusCode } } catch {}
    if (-not $status) {
        $m = [regex]::Match([string]$err.Message, '\b(401|403|404|429|500|502|503|504)\b')
        if ($m.Success) { $status = [int]$m.Value }
    }
    if ($status -eq 401 -or $status -eq 403) { return 'auth-failed' }
    if ($status) { return "http-$status" }

    $text = [string]$err.Message
    try { if ($err.InnerException) { $text = $text + ' ' + [string]$err.InnerException.Message } } catch {}
    if ($text -match 'timed out|timeout|超时') { return 'timeout' }
    if ($text -match 'refused|拒绝|Unable to connect|No connection could be made|无法连接') { return 'connect-failed' }
    return 'error'
}

# 统一的控制接口调用:返回 ok/reason/data,调用方不必各自 try/catch
function Invoke-ClashApi([string]$Path, [int]$TimeoutSec = 8) {
    $api = Get-ClashApi
    if (-not $api) { return @{ ok = $false; reason = 'no-endpoint'; data = $null; endpoint = ''; source = '' } }
    if ($api.pipe) {
        $response = Invoke-ClashPipeRequest $api.pipe $Path $api.headers $TimeoutSec
        return @{ ok = $response.ok; reason = $response.reason; data = $response.data; endpoint = $api.base; source = $api.source }
    }
    try {
        $params = @{ Uri = ($api.base + $Path); TimeoutSec = $TimeoutSec; ErrorAction = 'Stop' }
        if ($api.headers.Count -gt 0) { $params['Headers'] = $api.headers }
        $data = Invoke-RestMethod @params
        return @{ ok = $true; reason = ''; data = $data; endpoint = $api.base; source = $api.source }
    } catch {
        return @{ ok = $false; reason = (Get-ClashApiError $_.Exception); data = $null; endpoint = $api.base; source = $api.source }
    }
}

function Get-ClashApiHint([string]$reason) {
    $noEndpoint = '控制接口未配置:检查 Clash Verge 的 external-controller'
    $connectFailed = '控制接口拒绝连接:Verge 未开外部控制或端口不对'
    if ($PROXY_MODE -eq 'flclash') {
        $noEndpoint = '控制接口未配置:检查 FlClash 的 external-controller，或填写本机端口'
        $connectFailed = '控制接口拒绝连接:FlClash 未开外部控制或端口不对'
    }
    switch ($reason) {
        'no-endpoint'    { return $noEndpoint }
        'connect-failed' { return $connectFailed }
        'pipe-unavailable' { return '控制管道不可用:Clash Verge 未运行或内核未就绪' }
        'auth-failed'    { return '控制接口鉴权失败:secret 不匹配' }
        'timeout'        { return '控制接口超时:内核无响应或 /rules 过大' }
        ''               { return '控制接口不可用' }
        default         { return "控制接口异常($reason)" }
    }
}

# 返回 global|rule|direct|unknown;API 优先,失败再读 YAML
function Get-ClashMode {
    try {
        $probe = Invoke-ClashApi '/configs' 3
        if ($probe.ok) {
            $m = ([string]$probe.data.mode).Trim().ToLowerInvariant()
            if ($m -eq 'global' -or $m -eq 'rule' -or $m -eq 'direct') { return $m }
        }
        if (-not (Test-FlClashClientSelected)) {
            foreach ($name in @('clash-verge.yaml', 'config.yaml', 'verge.yaml')) {
                $p = Join-Path $CLASH_DIR $name
                if (-not (Test-Path -LiteralPath $p)) { continue }
                foreach ($line in Get-Content -LiteralPath $p -Encoding UTF8 -ErrorAction SilentlyContinue) {
                    if ($line -match '(?m)^mode:\s*[''"]?(\w+)') {
                        $m = $Matches[1].ToLowerInvariant()
                        if ($m -eq 'global' -or $m -eq 'rule' -or $m -eq 'direct') { return $m }
                    }
                }
            }
        }
        if ($PROXY_MODE -eq 'flclash' -or $PROXY_MODE -eq 'auto') {
            $flclashYaml = Get-FlClashConfigPath
            if ($flclashYaml) {
                foreach ($line in Get-Content -LiteralPath $flclashYaml -Encoding UTF8 -ErrorAction SilentlyContinue) {
                    if ($line -match '(?m)^mode:\s*[''"]?(\w+)') {
                        $m = $Matches[1].ToLowerInvariant()
                        if ($m -eq 'global' -or $m -eq 'rule' -or $m -eq 'direct') { return $m }
                    }
                }
            }
        }
    } catch {}
    return 'unknown'
}

function Get-ClashRules {
    $probe = Invoke-ClashApi '/rules' 8
    if (-not $probe.ok) { return @{ available = $false; rules = @(); reason = $probe.reason } }
    return @{ available = $true; rules = @($probe.data.rules); reason = '' }
}

# 内核前 16 条必须包含 13 个域名 + 3 个端口;兼容 DOMAIN-SUFFIX/DomainSuffix 与 AND(...DST-PORT...) 形式
function Test-PinnedRules($rules) {
    $head = @($rules | Select-Object -First 16)
    if ($head.Count -lt 16) { return $false }
    $domainHits = @($head | Where-Object {
        (([string]$_.type).ToLowerInvariant() -replace '[-_]', '') -eq 'domainsuffix' -and
        ($CLAUDE_DOMAINS -contains [string]$_.payload)
    } | ForEach-Object { ([string]$_.payload).ToLowerInvariant() } | Sort-Object -Unique).Count
    $portHits = @($head | ForEach-Object {
        $type = ([string]$_.type).ToLowerInvariant() -replace '[-_]', ''
        $payload = [string]$_.payload
        if ($type -eq 'dstport' -and ($CLAUDE_PORTS -contains $payload)) { $payload }
        elseif ($payload -match ('DST-PORT,(' + ($CLAUDE_PORTS -join '|') + ')(?:[,)]|$)')) {
            foreach ($p in $CLAUDE_PORTS) {
                if ($payload -match ('DST-PORT,' + [regex]::Escape($p) + '(?:[,)]|$)')) { $p }
            }
        }
    } | Sort-Object -Unique).Count
    return ($domainHits -eq $CLAUDE_DOMAINS.Count -and $portHits -eq $CLAUDE_PORTS.Count)
}

# 规则模式只写 profiles\Script.js 托管块,不再碰 Merge.yaml
function Write-ClashScriptRules {
    $result = @{ ok = $false; changed = $false; existed = $false; backup = ''; message = '' }
    $scriptPath = Join-Path $CLASH_DIR 'profiles\Script.js'
    $backup = ''
    $existed = Test-Path -LiteralPath $scriptPath
    $result.existed = $existed
    try {
        if ([string]::IsNullOrWhiteSpace($PROXY_GROUP) -or $PROXY_GROUP -match '[,\r\n]') {
            $result.message = '代理组名为空或包含逗号/换行'
            return $result
        }
        $rules = @()
        foreach ($d in $CLAUDE_DOMAINS) { $rules += "DOMAIN-SUFFIX,$d,$PROXY_GROUP" }
        foreach ($p in $CLAUDE_PORTS) { $rules += "DST-PORT,$p,$PROXY_GROUP" }
        $json = ConvertTo-Json -InputObject @($rules) -Compress
        $managed = "// >>> claude-switcher managed rules >>>`r`n" +
            "  const pinnedRules = $json;`r`n" +
            "  config.rules = pinnedRules.concat((config.rules||[]).filter(r=>!pinnedRules.includes(r)));`r`n" +
            "// <<< claude-switcher managed rules <<<"

        if (-not $existed) {
            $dir = Split-Path -Parent $scriptPath
            New-Item -ItemType Directory -Force -Path $dir -ErrorAction Stop | Out-Null
            $content = "function main(config) {`r`n" + $managed + "`r`n  return config;`r`n}`r`n"
            [System.IO.File]::WriteAllText($scriptPath, $content, (New-Object System.Text.UTF8Encoding($false)))
            $result.ok = $true
            $result.changed = $true
            $result.message = '已创建 Script.js'
            return $result
        }

        $existing = [System.IO.File]::ReadAllText($scriptPath)
        if ($existing -notmatch 'function\s+main\s*\(') {
            $result.message = 'Script.js 已存在但没有 function main(...),未改写,请手动合并'
            return $result
        }
        if ($existing -match '// >>> claude-switcher managed rules >>>') {
            $pattern = '(?s)// >>> claude-switcher managed rules >>>.*?// <<< claude-switcher managed rules <<<'
            $replacement = $managed.Replace('$', '$$')
            $updated = [regex]::Replace($existing, $pattern, $replacement)
        } else {
            $fn = [regex]::Match($existing, 'function\s+main\s*\(')
            $braceMatch = [regex]::Match($existing, 'function\s+main\s*\([^)]*\)\s*\{')
            if ($braceMatch.Success) {
                $brace = $braceMatch.Index + $braceMatch.Length - 1
            } else {
                $brace = $existing.IndexOf('{', $fn.Index + $fn.Length)
            }
            if ($brace -lt 0) {
                $result.message = 'Script.js 的 function main 缺少函数体,未改写'
                return $result
            }
            $updated = $existing.Substring(0, $brace + 1) + "`r`n" + $managed + "`r`n" + $existing.Substring($brace + 1)
        }
        if ($updated -ceq $existing) {
            $result.ok = $true
            $result.changed = $false
            $result.message = '规则已是当前版本'
            return $result
        }
        $backup = "$scriptPath.bak.$((Get-Date).ToString('yyyyMMddHHmmss'))"
        Copy-Item -LiteralPath $scriptPath -Destination $backup -Force -ErrorAction Stop
        [System.IO.File]::WriteAllText($scriptPath, $updated, (New-Object System.Text.UTF8Encoding($false)))
        $result.ok = $true
        $result.changed = $true
        $result.backup = $backup
        $result.message = '规则已写入 Script.js'
        return $result
    } catch {
        $result.ok = $false
        $result.message = $_.Exception.Message
        if ($backup -and (Test-Path -LiteralPath $backup)) {
            Copy-Item -LiteralPath $backup -Destination $scriptPath -Force -ErrorAction SilentlyContinue
        } elseif (-not $existed) {
            Remove-Item -LiteralPath $scriptPath -Force -ErrorAction SilentlyContinue
        }
        return $result
    }
}

# 清理旧版写进 Merge.yaml 的规则、多文档分隔符和重复顶层键
function Remove-LegacyMergeRules {
    $result = @{ changed = $false; backup = ''; removedRules = 0; removedDocs = 0; removedDupes = 0; message = '' }
    $mergePath = Join-Path $CLASH_DIR 'profiles\Merge.yaml'
    if (-not (Test-Path -LiteralPath $mergePath)) { return $result }
    $backup = ''
    try {
        $lines = @(Get-Content -LiteralPath $mergePath -Encoding UTF8 -ErrorAction Stop)
        $text = $lines -join "`n"
        $domainAlt = ($CLAUDE_DOMAINS | ForEach-Object { [regex]::Escape($_) }) -join '|'
        $portAlt = ($CLAUDE_PORTS | ForEach-Object { [regex]::Escape($_) }) -join '|'
        $ruleRe = '(?m)^\s*-\s*((DOMAIN-SUFFIX,(' + $domainAlt + '),)|(DST-PORT,(' + $portAlt + '),))'
        $hasRules = $text -match $ruleRe
        $hasDocs = [bool](@($lines | Where-Object { $_ -match '^\s*---\s*(#.*)?$' }).Count)
        $topKeys = @($lines | Where-Object { $_ -match '^[A-Za-z0-9_.-]+:' } | ForEach-Object { ($_ -split ':', 2)[0] })
        $hasDupes = [bool](@($topKeys | Group-Object | Where-Object { $_.Count -gt 1 }).Count)
        # 用户可能已经手动删了规则行但留下空的 prepend-rules:,同样需要清理
        $hasEmptyPrepend = $false
        for ($i = 0; $i -lt $lines.Count; $i++) {
            if ($lines[$i] -match '^\s*prepend-rules:\s*(#.*)?$') {
                $hasItem = $false
                for ($j = $i + 1; $j -lt $lines.Count; $j++) {
                    $next = [string]$lines[$j]
                    if ($next -match '^\s*$') { continue }
                    if ($next -match '^\s*#') { continue }
                    if ($next -match '^\s*-\s+') { $hasItem = $true }
                    break
                }
                if (-not $hasItem) { $hasEmptyPrepend = $true; break }
            }
        }
        if (-not $hasRules -and -not $hasDocs -and -not $hasDupes -and -not $hasEmptyPrepend) { return $result }

        $backup = "$mergePath.bak.$((Get-Date).ToString('yyyyMMddHHmmss'))"
        Copy-Item -LiteralPath $mergePath -Destination $backup -Force -ErrorAction Stop

        $out = @()
        $removedRules = 0
        $removedDocs = 0
        foreach ($line in $lines) {
            if ($line -match '^\s*---\s*(#.*)?$') { $removedDocs++; continue }
            if ($line -match $ruleRe) { $removedRules++; continue }
            $out += $line
        }

        $cleaned = @()
        for ($i = 0; $i -lt $out.Count; $i++) {
            $line = $out[$i]
            if ($line -match '^\s*prepend-rules:\s*(#.*)?$') {
                $hasItem = $false
                for ($j = $i + 1; $j -lt $out.Count; $j++) {
                    $next = [string]$out[$j]
                    if ($next -match '^\s*$') { continue }
                    if ($next -match '^\s*#') { continue }
                    if ($next -match '^\s*-\s+') { $hasItem = $true }
                    break
                }
                if (-not $hasItem) { continue }
            }
            $cleaned += $line
        }

        $final = @()
        $seen = @{}
        $skip = $false
        $removedDupes = 0
        foreach ($line in $cleaned) {
            if ($line -match '^([A-Za-z0-9_.-]+):') {
                $key = $Matches[1]
                if ($seen.ContainsKey($key)) {
                    $skip = $true
                    $removedDupes++
                    continue
                }
                $seen[$key] = $true
                $skip = $false
                $final += $line
                continue
            }
            if ($skip) { continue }
            $final += $line
        }

        $before = ($lines -join "`n")
        $after = ($final -join "`n")
        if ($after -ceq $before) {
            Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
            return $result
        }

        if ([string]::IsNullOrWhiteSpace($after)) {
            Remove-Item -LiteralPath $mergePath -Force -ErrorAction Stop
        } else {
            [System.IO.File]::WriteAllText($mergePath, ($final -join "`r`n") + "`r`n", (New-Object System.Text.UTF8Encoding($false)))
        }
        $result.changed = $true
        $result.backup = $backup
        $result.removedRules = $removedRules
        $result.removedDocs = $removedDocs
        $result.removedDupes = $removedDupes
        $result.message = '已清理历史遗留 Merge 规则'
        return $result
    } catch {
        $result.message = $_.Exception.Message
        if ($backup -and (Test-Path -LiteralPath $backup)) {
            Copy-Item -LiteralPath $backup -Destination $mergePath -Force -ErrorAction SilentlyContinue
        }
        return $result
    }
}

function Restart-ClashVerge {
    $cv = Find-ClashVerge
    if (-not $cv) { return $false }
    Get-Process -Name 'clash-verge','Clash Verge' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep 2
    try { Start-Process $cv } catch { return $false }
    return $true
}

function Wait-Tun([int]$seconds) {
    for ($i = 0; $i -lt $seconds; $i++) {
        if (Test-TunUp) { return $true }
        Start-Sleep 1
    }
    return [bool](Test-TunUp)
}

function Wait-ClashRules([int]$seconds) {
    $last = @{ available = $false; rules = @() }
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $last = Get-ClashRules
        if ($last.available -and (Test-PinnedRules $last.rules)) { return $last }
        Start-Sleep 1
    }
    return $last
}

function Restore-ClashScriptChange($write) {
    if (-not $write.changed) { return }
    $scriptPath = Join-Path $CLASH_DIR 'profiles\Script.js'
    if ($write.backup -and (Test-Path -LiteralPath $write.backup)) {
        Copy-Item -LiteralPath $write.backup -Destination $scriptPath -Force -ErrorAction SilentlyContinue
    } elseif (-not $write.existed) {
        Remove-Item -LiteralPath $scriptPath -Force -ErrorAction SilentlyContinue
    }
}

function Test-ClashScriptManaged {
    $p = Join-Path $CLASH_DIR 'profiles\Script.js'
    if (-not (Test-Path -LiteralPath $p)) { return $false }
    try { return ([System.IO.File]::ReadAllText($p)) -match '// >>> claude-switcher managed rules >>>' } catch { return $false }
}

function Test-MihomoClientSelected {
    if ($PROXY_MODE -in @('clash-verge', 'flclash')) { return $true }
    if ($PROXY_MODE -ne 'auto') { return $false }
    if (Get-Process -Name 'verge-mihomo','FlClashCore','FlClash' -ErrorAction SilentlyContinue) { return $true }
    if ((Get-Process -Name 'sing-box','ClashX','Surge' -ErrorAction SilentlyContinue) -or
        (Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running')) {
        return $false
    }
    return (Test-Path -LiteralPath $CLASH_DIR) -or [bool](Get-FlClashConfigPath)
}

function Get-RulesStatus {
    if (-not (Test-MihomoClientSelected)) {
        $wgMode = ($PROXY_MODE -eq 'wireguard') -or ($PROXY_MODE -eq 'auto' -and [bool](Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running'))
        if ($wgMode) {
            $wgUp = @(Get-NetAdapter -ErrorAction SilentlyContinue | Where-Object {
                $_.Status -eq 'Up' -and
                ($_.Name -notmatch '(?i)Tailscale' -and $_.InterfaceDescription -notmatch '(?i)Tailscale') -and
                ($_.InterfaceDescription -match '(?i)(WireGuard|Wintun)' -or $_.Name -match '(?i)^wg')
            })
            $wgIdx = @($wgUp | ForEach-Object { if ($null -ne $_.ifIndex) { [int]$_.ifIndex } elseif ($null -ne $_.InterfaceIndex) { [int]$_.InterfaceIndex } })
            $defRoute = @()
            if ($wgIdx.Count -gt 0) {
                $defRoute = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Where-Object { $wgIdx -contains [int]$_.InterfaceIndex })
            }
            if ($defRoute.Count -gt 0) { return '规则置顶=ok:WireGuard 全局路由已接管' }
            return '规则置顶=warn:WireGuard 未检测到全局默认路由(0.0.0.0/0),无法核验分流'
        }
        $d1 = (Resolve-DnsName claude.ai -Type A -ErrorAction SilentlyContinue | Select-Object -First 1).IPAddress
        $d2 = (Resolve-DnsName api.anthropic.com -Type A -ErrorAction SilentlyContinue | Select-Object -First 1).IPAddress
        if ($d1 -match '^198\.1[89]\.' -and $d2 -match '^198\.1[89]\.') {
            return '规则置顶=ok:Fake-IP 已接管 Claude 域名(非 Mihomo 客户端,STUN 规则请自行确认)'
        }
        return "规则置顶=warn:非 Mihomo 客户端($PROXY_MODE)未检测到 Fake-IP 接管,无法核验置顶规则"
    }
    $mode = Get-ClashMode
    if ($mode -eq 'global') { return '规则置顶=ok:全局模式(全部流量走代理)' }
    if ($mode -eq 'direct') { return '规则置顶=fail:直连模式(流量不走代理)' }
    if ($mode -eq 'unknown') { return '规则置顶=warn:无法确认 Clash 模式,未检查规则' }
    $rulesInfo = Get-ClashRules
    if ($rulesInfo.available) {
        if (Test-PinnedRules $rulesInfo.rules) { return '规则置顶=ok' }
        return '规则置顶=fail:内核未加载置顶规则'
    }
    if (Test-ClashScriptManaged) { return "规则置顶=warn:规则已写入但无法验证内核加载($(Get-ClashApiHint $rulesInfo.reason))" }
    return '规则置顶=fail:未配置'
}

function Format-MaskedIp([string]$Ip) {
    if ([string]::IsNullOrWhiteSpace($Ip)) { return '未知' }
    $v = $Ip.Trim()
    if ($v -match '^(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}$') { return "$($Matches[1]).$($Matches[2]).*.*" }
    if ($v.Contains(':')) {
        $parts = @($v.Split(':') | Where-Object { $_ -ne '' })
        if ($parts.Count -ge 2) { return "$($parts[0]):$($parts[1]):*" }
        return '*:*'
    }
    return $v
}

function Test-DnsResolverVerdict([string]$LocalIp, [bool]$IsFakeIp, [object[]]$Resolvers, [bool]$DetectReachable) {
    $displayLocal = if ($IsFakeIp) { $LocalIp } else { Format-MaskedIp $LocalIp }
    if ($DetectReachable -and $Resolvers.Count -gt 0) {
        $cn = @($Resolvers | Where-Object { $_.country -eq 'CN' })
        if ($cn.Count -gt 0) {
            $masked = ($cn | ForEach-Object { Format-MaskedIp $_.ip }) -join ','
            $prefix = if ($IsFakeIp) { "Fake-IP($displayLocal)但上游经国内解析器" } else { '经国内解析器' }
            return @{ state = 'fail'; detail = "${prefix}($masked)" }
        }
        if (@($Resolvers | Where-Object { -not $_.country }).Count -gt 0) {
            if ($IsFakeIp) { return @{ state = 'warn'; detail = "Fake-IP($displayLocal) · 部分解析器归属未知" } }
            return @{ state = 'fail'; detail = "实IP($displayLocal) · 部分解析器归属未知" }
        }
        $countries = @($Resolvers | ForEach-Object { $_.country } | Select-Object -Unique) -join '/'
        if ($IsFakeIp) { return @{ state = 'ok'; detail = "Fake-IP走隧道 · 解析器位于 $countries" } }
        return @{ state = 'ok'; detail = "实IP解析 · 解析器位于 $countries" }
    }
    if ($IsFakeIp) { return @{ state = 'warn'; detail = "Fake-IP($displayLocal)已接管,未确认上游解析器归属" } }
    return @{ state = 'fail'; detail = "未命中Fake-IP($displayLocal)且无法核验解析器归属" }
}

function Test-DnsCheck {
    $rec = Resolve-DnsName claude.ai -Type A -ErrorAction SilentlyContinue | Select-Object -First 1
    $dns = if ($rec -and $rec.IPAddress) { ([string]$rec.IPAddress).Trim() } else { '' }
    if (-not $dns) { return @{ state = 'fail'; detail = '域名解析失败(claude.ai)' } }
    if ($dns -match '^(0\.|127\.)') { return @{ state = 'fail'; detail = "异常解析地址($dns)" } }
    $isFakeIp = ($dns -match '^198\.1[89]\.')
    if (-not (Test-ProxyPathAlive)) {
        return @{ state = 'fail'; detail = '代理路径失效,已跳过解析器探测(避免真实 IP 出网)' }
    }
    $token = if ($script:ProbePrefetch -and $script:ProbePrefetch.DnsToken) {
        [string]$script:ProbePrefetch.DnsToken
    } else {
        ([guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')).Substring(0, 40)
    }
    $detectReachable = $false
    $resolvers = @()
    try {
        $detectRaw = Invoke-ProbeText -Url "https://$token.ipleak.net/dnsdetection/" -Attempts 2 -TimeoutSec 6
        $detectObj = $detectRaw | ConvertFrom-Json -ErrorAction Stop
        if ($null -ne $detectObj -and $null -ne $detectObj.ip -and $detectObj.ip -is [pscustomobject]) {
            $detectReachable = $true
            foreach ($rip in @($detectObj.ip.PSObject.Properties.Name | Where-Object { $_ } | Select-Object -First 4)) {
                $cc = ''
                try {
                    $infoRaw = Invoke-ProbeText -Url "https://ipleak.net/json/$([Uri]::EscapeDataString([string]$rip))" -Attempts 2 -TimeoutSec 5
                    $infoObj = $infoRaw | ConvertFrom-Json -ErrorAction Stop
                    $cc = ([string]$infoObj.country_code).Trim().ToUpperInvariant()
                } catch {}
                $resolvers += [pscustomobject]@{ ip = [string]$rip; country = $cc }
            }
        }
    } catch {}
    return (Test-DnsResolverVerdict $dns $isFakeIp $resolvers $detectReachable)
}

function Add-IpHistory([string]$Ip) {
    if ([string]::IsNullOrWhiteSpace($Ip) -or $Ip -notmatch '^\S+$') { return 0 }
    $history = Join-Path $BASE 'iphistory.log'
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    [System.IO.File]::AppendAllText($history, "$Ip $now`n", [System.Text.Encoding]::ASCII)
    $cutoff = $now - 172800
    $recent = @()
    $unique = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($line in @([System.IO.File]::ReadAllLines($history))) {
        if ($line -notmatch '^(\S+) (\d+)$') { continue }
        $stamp = [long]$Matches[2]
        if ($stamp -lt $cutoff) { continue }
        $recent += $line
        [void]$unique.Add($Matches[1])
    }
    $tmp = "$history.tmp"
    [System.IO.File]::WriteAllLines($tmp, $recent, [System.Text.Encoding]::ASCII)
    Move-Item -LiteralPath $tmp -Destination $history -Force
    return $unique.Count
}

function Invoke-CheckDetail {
    try { $script:CheckConfig = Get-CheckConfig } catch { '检查配置无效，请恢复默认设置'; return }
    $pname = Get-ProxyName
    if (Test-ProxyAlive) {
        if ($PROXY_MODE -eq 'other') { "代理内核=ok:$pname(仅校验TUN)" }
        else { "代理内核=ok:$pname 运行中" }
    } else { "代理内核=fail:$pname 未运行" }

    $tun = Get-TunStatus
    if ($tun.up) {
        if ($tun.detail -eq '198.18 假IP段') { "TUN网卡=ok" }
        else { "TUN网卡=ok:$($tun.detail)" }
    } else { "TUN网卡=fail:不在线" }

    $e = Get-Egress
    if ($e.skipped) { "出口IP=fail:代理路径失效,已跳过探测(避免真实 IP 出网)" }
    elseif ($e.loc -eq $COUNTRY_TARGET) { "出口IP=ok:$($e.ip)" }
    else { "出口IP=fail:$($e.ip)/$($e.loc)(期望$COUNTRY_TARGET)" }

    Start-ProbePrefetch $e

    if (Test-CheckDisabled 'IP一致') {
        Format-OptionalResult 'IP一致' 'ok'
    } else {
        $lastipFile = "$BASE\lastip.txt"
        $lastip = if (Test-Path $lastipFile) { (Get-Content $lastipFile -Raw).Trim() } else { '' }
        $ipCount = Add-IpHistory $e.ip
        $historyWarn = if ($ipCount -ge 3) { "；48h内出口IP变化$ipCount 次，建议固定节点" } else { '' }
        $ipState = if ($ipCount -ge 3) { 'warn' } else { 'ok' }
        if (-not $e.ip) { Format-OptionalResult 'IP一致' 'fail' '未获取到出口IP' }
        elseif (-not $lastip) { Format-OptionalResult 'IP一致' $ipState "首次记录$historyWarn" }
        elseif ($e.ip -eq $lastip) { Format-OptionalResult 'IP一致' $ipState $historyWarn.TrimStart('；') }
        else { Format-OptionalResult 'IP一致' 'warn' "$lastip → $($e.ip)$historyWarn" }
        if ($e.ip) { $e.ip | Out-File -Encoding ascii -NoNewline $lastipFile }
    }

    $tz = (Get-TimeZone).Id
    if ($tz -eq $TZ_TARGET) { "时区=ok" } else { "时区=fail:$tz" }

    $cult = [System.Globalization.CultureInfo]::CurrentCulture.Name
    if ($cult -eq $LOCALE_TARGET) { "地区=ok:$cult" } else { "地区=fail:$cult(期望$LOCALE_TARGET)" }

    $v6off = [bool](Get-NetAdapterBinding -ComponentID ms_tcpip6 -ErrorAction SilentlyContinue | Where-Object { $_.Enabled -eq $true })
    if (-not $v6off) { "IPv6=ok:已关闭" } else { "IPv6=fail:未关闭" }

    # 先执行本地 WebRTC STUN 与控制接口检查,让后台预取的 DNS(ipleak) 与 ASN/生态请求并行跑完后再收集,输出顺序不变
    $webrtcLine = if (Test-CheckDisabled 'WebRTC') {
        Format-OptionalResult 'WebRTC' 'ok'
    } else {
        $w = Test-WebRTC $e.ip
        Format-OptionalResult 'WebRTC' ([string]$w.code) ([string]$w.detail)
    }

    $bgHealth = Get-BodyguardHealth
    $bgLine = if ($bgHealth.state -eq 'ok') { "保镖=ok:$($bgHealth.detail)" } else { "保镖=$($bgHealth.state):$($bgHealth.detail)" }

    $rulesLine = Get-RulesStatus

    if (Test-CheckDisabled 'DNS') {
        Format-OptionalResult 'DNS' 'ok'
    } else {
        $dnsRes = Test-DnsCheck
        Format-OptionalResult 'DNS' ([string]$dnsRes.state) ([string]$dnsRes.detail)
    }

    $webrtcLine
    $bgLine
    $rulesLine

    # Claude 的语言设置独立于 Windows 显示语言，使用当前数据目录中的配置。
    if (Test-CheckDisabled '客户端语言') {
        Format-OptionalResult '客户端语言' 'ok'
    } else {
        $clientLang = ''
        $clientConfig = Join-Path $DATA 'config.json'
        # MSIX 版可能在旧目录仅保留 MCP 配置，而实际设置已迁移到 Claude-Data。
        if (-not (Test-Path -LiteralPath $clientConfig)) {
            $msix = Find-ClaudeMsixDataPath
            if ($msix) { $clientConfig = Join-Path $msix 'config.json' }
        }
        try {
            $locale = (Get-Content -LiteralPath $clientConfig -Raw -Encoding UTF8 -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop).locale
            if ($locale -is [string] -and $locale -match '^[a-z]{2,3}(?:-[a-z0-9]{2,8})*\z') { $clientLang = $locale }
        } catch {}
        $wantLang = ([string]$LOCALE_TARGET -split '-')[0].ToLowerInvariant()
        if (-not $clientLang) {
            Format-OptionalResult '客户端语言' 'warn' '未能读取 Claude 的语言设置'
        } elseif ((($clientLang -split '-')[0].ToLowerInvariant()) -eq $wantLang) {
            Format-OptionalResult '客户端语言' 'ok' "Claude=$clientLang"
        } else {
            Format-OptionalResult '客户端语言' 'warn' "Claude=$clientLang,期望 $wantLang 前缀"
        }
    }

    if (Test-CheckDisabled 'ASN属性') {
        Format-OptionalResult 'ASN属性' 'ok'
    } else {
        $org = ''
        $asnType = ''
        if ($e.ip -and (Test-ProxyPathAlive)) {
            # ASN 查询要把出口 IP 交给第三方,路径一旦失效就会用真实 IP 的源地址发出去,
            # 所以这里要再确认一次路径存活,而不是只看上一轮的 $e.ip。
            try {
                $ipInfoResponse = Invoke-ProbeText -Url "https://ipinfo.io/$([Uri]::EscapeDataString([string]$e.ip))/json" -TimeoutSec 6
                $ipInfo = $ipInfoResponse | ConvertFrom-Json -ErrorAction Stop
                if ([string]$ipInfo.ip -eq [string]$e.ip) { $org = [string]$ipInfo.org }
            } catch {}
        }
        if ($org -match '^AS(\d+)\s') {
            $asn = $Matches[1]
            try {
                $networkResponse = Invoke-ProbeText -Url "https://www.peeringdb.com/api/net?asn=$asn&fields=asn,info_type" -TimeoutSec 6
                $network = ($networkResponse | ConvertFrom-Json -ErrorAction Stop).data |
                    Where-Object { [string]$_.asn -eq $asn } | Select-Object -First 1
                $asnType = [string]$network.info_type
            } catch {}
        }
        if (-not $org) { Format-OptionalResult 'ASN属性' 'warn' '未获取' }
        elseif ($asnType -eq 'Cable/DSL/ISP') { Format-OptionalResult 'ASN属性' 'ok' "ISP($org);具体 IP 的住宅属性未验证" }
        elseif ($asnType) { Format-OptionalResult 'ASN属性' 'warn' "$asnType($org);不代表具体 IP 用途" }
        else { Format-OptionalResult 'ASN属性' 'warn' "网络类型未确认($org)" }
    }

    if (Test-CheckDisabled '生态可达性') {
        Format-OptionalResult '生态可达性' 'ok'
    } elseif (-not (Test-ProxyPathAlive)) {
        # 路径可能已失效时不再发探测请求:探测会脱离隧道、用真实 IP 敲门。
        Format-OptionalResult '生态可达性' 'fail' '代理路径失效,已跳过探测(避免真实 IP 出网)'
    } else {
        $r1 = Invoke-ProbeCode -Url 'https://claude.ai/'
        $r2 = Invoke-ProbeCode -Url 'https://api.anthropic.com/v1/models'
        if ($r1 -ne '000' -and $r2 -ne '000') { Format-OptionalResult '生态可达性' 'ok' "claude.ai:$r1 api:$r2" } else { Format-OptionalResult '生态可达性' 'fail' "claude.ai:$r1 api:$r2" }
    }

    Stop-ProbePrefetch
}

$HARD_CHECKS = @('代理内核','TUN网卡','出口IP','时区','地区','IPv6','保镖','规则置顶')

function Invoke-Check {
    $results = @(Invoke-CheckDetail)
    $states = [ordered]@{}
    $invalid = $false
    foreach ($line in $results) {
        if ($line -notmatch '^([^=]+)=(ok|warn|fail|skip)(?::.*)?$') { continue }
        $name = $Matches[1]; $state = $Matches[2]
        if ($states.Contains($name)) { $invalid = $true; continue }
        $states[$name] = $state
    }
    # 8 项硬性检查必须全部为 ok(warn/skip 同样阻断);6 项可选检查仅 fail 阻断
    $blocked = @($states.Keys | Where-Object {
        $states[$_] -eq 'fail' -or (($HARD_CHECKS -contains $_) -and $states[$_] -ne 'ok')
    })
    if ($invalid -or $states.Count -ne 14 -or $blocked.Count -gt 0) {
        $script:exitCode = 1
        $detail = if ($blocked.Count) { $blocked -join '、' } else { '检查输出不完整或重复' }
        "FAIL:$detail"
        return
    }
    'OK:环境检查通过'
}

function Invoke-Fix {
    "FIX:开始一键修复"

    # A. 拉起代理客户端并等待内核+TUN(仅 Clash Verge 可自动拉起)
    if ($PROXY_MODE -eq 'clash-verge') {
        if (-not (Test-ProxyAlive)) {
            "FIX:正在启动 Clash Verge..."
            $cv = Find-ClashVerge
            if ($cv) { Start-Process $cv } else { "FIX:未找到 Clash Verge 可执行文件,请手动启动" }
        }
        $ok = $false
        for ($i = 0; $i -lt 40; $i++) { if ((Test-ProxyAlive) -and (Test-TunUp)) { $ok = $true; break }; Start-Sleep 1 }
        if ($ok) { "FIX:代理内核+TUN ✓" } else { "FIX:代理内核/TUN ✗(请打开 Clash Verge 检查节点/线路及 TUN 开关)" }
    } else {
        $pname = Get-ProxyName
        if ((Test-ProxyAlive) -and (Test-TunUp)) { "FIX:代理客户端($pname)+TUN ✓" }
        elseif (-not (Test-ProxyAlive)) { "FIX:代理客户端($pname)未运行,请手动启动并开启 TUN 模式" }
        else { "FIX:TUN 未在线($pname),请检查客户端 TUN 开关" }
    }

    # B. 时区/IPv6/DNS/保镖(需管理员:通过 UAC 弹一次)
    $adminScript = @()
    if ((Get-TimeZone).Id -ne $TZ_TARGET) {
        Save-HomeTimeZone $TZ_TARGET
        $tzEscFix = $TZ_TARGET.Replace("'", "''")
        $adminScript += "Set-TimeZone -Id '$tzEscFix'"
    }
    if (Get-NetAdapterBinding -ComponentID ms_tcpip6 -ErrorAction SilentlyContinue | Where-Object { $_.Enabled }) {
        Save-HomeIpv6State
        $adminScript += "Get-NetAdapterBinding -ComponentID ms_tcpip6 | Disable-NetAdapterBinding -Confirm:`$false"
    }
    $adminScript += "Clear-DnsClientCache"
    $bgFix = Get-BodyguardHealth
    $siblingBg = if ($PSScriptRoot) { Join-Path $PSScriptRoot 'claude-bodyguard.ps1' } else { '' }
    $targetBg  = Join-Path $BASE 'claude-bodyguard.ps1'
    if ($siblingBg -and (Test-Path -LiteralPath $siblingBg -PathType Leaf) -and (-not (Test-Path -LiteralPath $targetBg) -or $bgFix.legacyPolicy)) {
        try {
            New-Item -ItemType Directory -Force -Path $BASE | Out-Null
            if (([IO.Path]::GetFullPath($siblingBg)) -ine ([IO.Path]::GetFullPath($targetBg))) {
                Copy-Item -LiteralPath $siblingBg -Destination $targetBg -Force -ErrorAction Stop
            }
        } catch {}
    }
    if (-not $bgFix.installed -or $bgFix.legacyPolicy) {
        $bgPathEsc = ("$BASE\claude-bodyguard.ps1").Replace("'", "''")
        $adminScript += "`$a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"' + '$bgPathEsc' + '`"'); `$t = New-ScheduledTaskTrigger -AtLogOn; `$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1); Register-ScheduledTask -TaskName 'ClaudeBodyguard' -Action `$a -Trigger `$t -Settings `$s -Force | Out-Null; Stop-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue; Start-ScheduledTask -TaskName 'ClaudeBodyguard'"
    } elseif (-not $bgFix.running) {
        try { Start-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction Stop; "FIX:保镖任务已在用户态重新拉起 ✓" } catch {
            $adminScript += "Start-ScheduledTask -TaskName 'ClaudeBodyguard'"
        }
    }
    if ($adminScript.Count -gt 0) {
        "FIX:修复时区/IPv6/DNS/保镖(需要管理员授权)..."
        $tmp = "$env:TEMP\claude-fix-$([guid]::NewGuid().Guid).ps1"
        [System.IO.File]::WriteAllText($tmp, ($adminScript -join "`r`n"), (New-Object System.Text.UTF8Encoding($true)))
        try { Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$tmp`"" } catch { "FIX:管理员授权被取消" }
        Remove-Item $tmp -ErrorAction SilentlyContinue
        "FIX:时区/IPv6/DNS/保镖 ✓"
    } else {
        "FIX:时区/IPv6/保镖 无需修复 ✓"
    }

    # B2. 置顶规则:先识别 Clash 模式,绝不盲目写 Merge.yaml
    if ($PROXY_MODE -ne 'clash-verge') {
        "FIX:请在 $(Get-ProxyName) 中自行添加 Claude 域名置顶/直连规则"
    } else {
        $clashMode = Get-ClashMode
        if ($clashMode -eq 'unknown') {
            "FIX:无法确认 Clash 模式，已跳过规则修改"
        } elseif ($clashMode -eq 'global' -or $clashMode -eq 'direct') {
            $modeLabel = if ($clashMode -eq 'global') { '全局模式' } else { '直连模式' }
            "FIX:Clash 模式=$clashMode"
            $legacy = Remove-LegacyMergeRules
            if ($legacy.message -and -not $legacy.changed) {
                "FIX:Merge.yaml 清理提示: $($legacy.message)"
            }
            if ($legacy.changed) {
                "FIX:发现历史遗留 Merge 规则,已清理并备份: $($legacy.backup)"
                if (Restart-ClashVerge) {
                    "FIX:已重启 Clash Verge 以重新加载配置"
                    if ($clashMode -eq 'global') {
                        $tunOk = Wait-Tun 40
                        if ((Test-ProxyAlive) -and $tunOk) { "FIX:代理/TUN 已恢复 ✓" } else { "FIX:代理/TUN 未完全恢复,请手动检查 Clash 和 TUN 开关" }
                    }
                } else {
                    "FIX:未找到 Clash Verge 可执行文件,请手动重启以完成清理"
                }
                "FIX:$modeLabel,规则不适用,未写入任何规则,已清理历史遗留配置 ✓"
            } else {
                "FIX:$modeLabel,规则不适用,未改动任何配置 ✓"
            }
        } else {
            "FIX:Clash 模式=rule"
            $legacy = Remove-LegacyMergeRules
            if ($legacy.message -and -not $legacy.changed) {
                "FIX:Merge.yaml 清理提示: $($legacy.message)"
            }
            if ($legacy.changed) {
                "FIX:已清理历史遗留 Merge 规则并备份: $($legacy.backup)"
            }

            $write = Write-ClashScriptRules
            if (-not $write.ok) {
                "FIX:置顶规则 FAIL: $($write.message)"
                if ($legacy.changed) {
                    if (Restart-ClashVerge) {
                        Wait-Tun 20 | Out-Null
                        "FIX:已重启 Clash Verge 应用 Merge 清理"
                    } else {
                        "FIX:请手动重启 Clash Verge 应用 Merge 清理"
                    }
                }
            } else {
                $current = Get-ClashRules
                $alreadyOk = $current.available -and (Test-PinnedRules $current.rules)
                if (-not $write.changed -and -not $legacy.changed -and $alreadyOk) {
                    "FIX:置顶规则已是当前版本,内核已加载 ✓"
                } else {
                    if ($write.changed) {
                        "FIX:已写入 Script.js 规则并备份: $($write.backup)"
                    }
                    if (Restart-ClashVerge) {
                        $rulesInfo = Wait-ClashRules 40
                        if (-not $rulesInfo.available) {
                            "FIX:规则已写入但无法验证内核加载($(Get-ClashApiHint $rulesInfo.reason))"
                        } elseif (Test-PinnedRules $rulesInfo.rules) {
                            "FIX:置顶规则 ✓"
                            Wait-Tun 20 | Out-Null
                        } else {
                            "FIX:置顶规则验证失败,正在回滚 Script.js..."
                            Restore-ClashScriptChange $write
                            Restart-ClashVerge | Out-Null
                            Wait-Tun 20 | Out-Null
                            "FIX:置顶规则 FAIL: 验证失败,已回滚 Script.js,请确认 Clash Verge 已启用全局扩展脚本"
                        }
                    } else {
                        "FIX:规则已写入但未找到 Clash Verge 可执行文件,请手动重启后验证"
                    }
                }
            }
        }
    }

    # C. 出口IP最终验证
    Start-Sleep 2
    $e = Get-Egress
    if ($e.skipped) { "FIX:出口IP ✗ 代理路径失效,已跳过探测 — 请先恢复代理/TUN 再复验" }
    elseif ($e.loc -eq $COUNTRY_TARGET) { "FIX:出口IP ✓ ($($e.ip))" }
    else { "FIX:出口IP ✗ ($($e.ip)/$($e.loc),期望$COUNTRY_TARGET) — 请换节点/线路" }
    "FIX:修复流程结束"
}

function Format-AccountSize([double]$Bytes) {
    if ($Bytes -ge 1TB) { return '{0:0.#}T' -f ($Bytes / 1TB) }
    if ($Bytes -ge 1GB) { return '{0:0.#}G' -f ($Bytes / 1GB) }
    if ($Bytes -ge 1MB) { return '{0:0.#}M' -f ($Bytes / 1MB) }
    if ($Bytes -ge 1KB) { return '{0:0.#}K' -f ($Bytes / 1KB) }
    return '{0}B' -f [int]$Bytes
}

function Invoke-Status {
    $cur = if (Test-Path "$BASE\current.txt") { (Get-Content "$BASE\current.txt" -Raw).Trim() } else { 'none' }
    if ($cur -notmatch '^[A-D]$') { $cur = 'none' }
    $flags = @(); $archived = @(); $extras = @()
    foreach ($slot in @('A','B','C','D')) {
        $dir = Join-Path $BASE "accounts\$slot"
        $exists = Test-Path -LiteralPath $dir -PathType Container
        $flags += "has$slot=$(if ($exists) { 1 } else { 0 })"
        if (-not $exists) { continue }
        $archived += $slot
        $measure = Get-ChildItem -LiteralPath $dir -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum
        $size = Format-AccountSize ([double]($measure.Sum))
        $stamp = (Get-Item -LiteralPath $dir).LastWriteTime.ToString('yyyy-MM-dd')
        $extras += "acc$slot=$size@$stamp"
    }
    $accountList = if ($archived.Count) { $archived -join ',' } else { 'none' }
    $suffix = if ($extras.Count) { ' ' + ($extras -join ' ') } else { '' }
    "current=$cur $($flags -join ' ') accounts=$accountList$suffix"
}

$script:SwitchError = ''

function Set-SwitchError([string]$message) {
    $script:SwitchError = $message
    return $false
}

# robocopy 退出码 0-7 都算成功(1=有文件复制,2=有额外文件,3=两者兼有,4-7 为可恢复异常),8 及以上才是失败
function Invoke-Robocopy([string]$Source, [string]$Dest, [string]$Mode, [switch]$ExcludeJunctions) {
    if (-not (Get-Command robocopy -ErrorAction SilentlyContinue)) {
        return [pscustomobject]@{ ok = $false; code = -1 }
    }
    if (-not (Test-Path -LiteralPath $Source)) {
        return [pscustomobject]@{ ok = $false; code = 16 }
    }
    $rcArgs = @($Source, $Dest, $Mode, '/NFL', '/NDL', '/NJH', '/NJS', '/R:2', '/W:2')
    if ($ExcludeJunctions) { $rcArgs += '/XJ' }
    $null = & robocopy @rcArgs 2>&1
    return [pscustomobject]@{ ok = ($LASTEXITCODE -lt 8); code = $LASTEXITCODE }
}

# 正常关闭优先:先请求主窗口关闭让 Electron 落盘,10 秒不退再强制结束;强杀期间立刻拷贝数据目录会产生半写文件
function Stop-ClaudeApp {
    $p = Get-Process -Name 'Claude' -ErrorAction SilentlyContinue
    if (-not $p) { return $true }
    foreach ($proc in $p) {
        try { $null = $proc.CloseMainWindow() } catch {}
    }
    for ($i = 0; $i -lt 10; $i++) {
        if (-not (Get-Process -Name 'Claude' -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep 1
    }
    Get-Process -Name 'Claude' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 15; $i++) {
        if (-not (Get-Process -Name 'Claude' -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep 1
    }
    return $false
}

function Get-SwitchStagePath {
    $baseRoot = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($BASE)).TrimEnd('\')
    $dataRoot = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($DATA)).TrimEnd('\')
    if ($baseRoot -ieq $dataRoot) { return "$BASE\staging\Claude.new" }
    return "$DATA.new"
}

function Get-SwitchPreviousPath {
    $baseRoot = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($BASE)).TrimEnd('\')
    $dataRoot = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($DATA)).TrimEnd('\')
    if ($baseRoot -ieq $dataRoot) { return "$BASE\staging\Claude.old" }
    return "$DATA.old"
}

# 切换暂存(Claude.new)的归属标记。暂存内容只会是 accounts\<槽位> 的副本(载入)或空目录(新建),
# 从不是唯一用户数据(唯一数据在 $DATA / accounts\ / 由 cleanup.json 保护的 .old)。因此当标记
# 能证明某个残留暂存是本工具为同一槽位建立时,自动清理重试是安全的;无法证明则维持"拒绝覆盖"。
# 这样可消除"切换被文件占用打断后 Claude.new 残留 → 之后每次切换都被拒绝"的卡死。
function Get-SwitchStageMarkerPath { return "$BASE\staging\stage-owner.json" }

function Register-SwitchStageOwner([string]$Stage, [string]$Target) {
    $marker = Get-SwitchStageMarkerPath
    try {
        $dir = Split-Path -Parent $marker
        if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        $value = [ordered]@{ version = 1; path = $Stage; target = $Target }
        [IO.File]::WriteAllText($marker, ($value | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding($false)))
    } catch {
        Log "WARN:切换暂存归属标记写入失败:$($_.Exception.Message)"
    }
}

function Clear-SwitchStageOwner {
    $marker = Get-SwitchStageMarkerPath
    if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue }
}

function Resolve-SwitchStageLeftover([string]$Stage, [string]$Target) {
    $marker = Get-SwitchStageMarkerPath
    if (-not (Test-Path -LiteralPath $Stage)) {
        # 暂存已不在(上次可能提交成功只剩悬空标记),清掉标记即可
        if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue }
        return $true
    }
    $valid = $false
    if (Test-Path -LiteralPath $marker -PathType Leaf) {
        try {
            $record = (Get-Content -LiteralPath $marker -Raw -Encoding UTF8 -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop)
            $valid = ([int]$record.version -eq 1 -and [string]$record.path -eq $Stage -and [string]$record.target -eq $Target)
        } catch { $valid = $false }
    }
    if ($valid) {
        try {
            Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction Stop
            if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue }
            Log "切换暂存残留已按归属标记自动清理:$Stage"
            return $true
        } catch {
            return (Set-SwitchError "切换暂存残留无法清理:$Stage；请关闭占用程序后重试")
        }
    }
    return (Set-SwitchError "存在待恢复数据，拒绝覆盖:$Stage(确认数据归属后手动恢复或清理)")
}

function Write-AtomicCurrent([string]$Target) {
    $tmp = Join-Path $BASE ('.current-' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        [IO.File]::WriteAllText($tmp, $Target, [Text.Encoding]::ASCII)
        Move-Item -LiteralPath $tmp -Destination "$BASE\current.txt" -Force -ErrorAction Stop
        return $true
    } catch {
        return (Set-SwitchError "账号指针写入失败:$($_.Exception.Message)")
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
}

function Register-OwnedSwitchCleanup([string]$Path, [string]$Target) {
    $marker = "$BASE\staging\cleanup.json"
    try {
        $value = [ordered]@{ version = 1; path = $Path; target = $Target }
        [IO.File]::WriteAllText($marker, ($value | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding($false)))
    } catch {
        Log "WARN:旧目录清理归属标记写入失败:$($_.Exception.Message)"
    }
}

function Resolve-OwnedSwitchCleanup([string]$Path) {
    $marker = "$BASE\staging\cleanup.json"
    if (-not (Test-Path -LiteralPath $Path) -and -not (Test-Path -LiteralPath $marker)) { return $true }
    $current = if (Test-Path "$BASE\current.txt") { (Get-Content "$BASE\current.txt" -Raw).Trim() } else { '' }
    $valid = $false
    if (Test-Path -LiteralPath $marker -PathType Leaf) {
        try {
            $record = (Get-Content -LiteralPath $marker -Raw -Encoding UTF8 -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop)
            $valid = [int]$record.version -eq 1 -and [string]$record.path -eq $Path -and [string]$record.target -eq $current
        } catch { $valid = $false }
    }
    if ($valid) {
        if (-not (Test-Path -LiteralPath $Path)) {
            Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
            return $true
        }
        try {
            Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            Remove-Item -LiteralPath $marker -Force -ErrorAction Stop
            return $true
        } catch {
            return (Set-SwitchError "已确认归属的旧目录仍无法清理:$Path；请关闭占用程序后重试")
        }
    }
    if (Test-Path -LiteralPath $Path) {
        return (Set-SwitchError "存在待恢复数据，拒绝覆盖:$Path(确认数据归属后手动恢复或清理)")
    }
    return $true
}

# 数据目录换位:新目录先备好,再提交数据和账号指针;失败回滚，任何时候都不先删当前数据
function Switch-DataDir([string]$Stage, [string]$Target) {
    $old = Get-SwitchPreviousPath
    if (-not (Resolve-OwnedSwitchCleanup $old)) { return $false }
    if (Test-Path -LiteralPath $old) {
        return (Set-SwitchError "存在待恢复数据，拒绝覆盖:$old(确认数据归属后手动恢复或清理)")
    }
    if (Test-Path -LiteralPath $DATA) {
        try { Move-Item -LiteralPath $DATA -Destination $old -ErrorAction Stop }
        catch { return (Set-SwitchError "当前数据目录无法移开:$($_.Exception.Message)") }
    }
    try { Move-Item -LiteralPath $Stage -Destination $DATA -ErrorAction Stop }
    catch {
        $commitError = $_.Exception.Message
        if (Test-Path -LiteralPath $old) {
            try { Move-Item -LiteralPath $old -Destination $DATA -ErrorAction Stop }
            catch { return (Set-SwitchError "新数据目录提交失败，当前数据保留在:$old；自动回滚失败:$($_.Exception.Message)") }
        }
        return (Set-SwitchError "新数据目录提交失败，已回滚:$commitError")
    }
    if (-not (Write-AtomicCurrent $Target)) {
        $pointerError = $script:SwitchError
        try {
            if (Test-Path -LiteralPath $DATA) { Move-Item -LiteralPath $DATA -Destination $Stage -ErrorAction Stop }
            if (Test-Path -LiteralPath $old) { Move-Item -LiteralPath $old -Destination $DATA -ErrorAction Stop }
        } catch {
            return (Set-SwitchError "$pointerError；回滚未完成，恢复材料保留在:$Stage 和 $old")
        }
        return (Set-SwitchError "$pointerError；已回滚")
    }
    if (Test-Path -LiteralPath $old) {
        try { Remove-Item -LiteralPath $old -Recurse -Force -ErrorAction Stop }
        catch {
            Register-OwnedSwitchCleanup $old $Target
            "WARN:目标账号已切换，旧目录暂未清理；已记录归属，下次切换会先重试清理"
        }
    }
    # 数据与指针均已成功提交,暂存已移入正式数据目录,归属标记到此作废
    Clear-SwitchStageOwner
    return $true
}

# 归档当前数据:复制到 <槽位>.new，复制成功后才改名为正式快照;旧快照暂存 .previous，提交后删除
function Save-Account([string]$name) {
    $stage = "$BASE\accounts\$name.new"
    $currentDir = "$BASE\accounts\$name"
    $previous = "$BASE\accounts\$name.previous"
    foreach ($p in @($stage, $previous)) {
        if (Test-Path -LiteralPath $p) {
            Log "FAIL:账号$name 存在待恢复快照，拒绝覆盖($p)"
            return (Set-SwitchError "账号$name 存在待恢复快照，拒绝覆盖:$p(确认无价值后手动删除再试)")
        }
    }
    $rc = Invoke-Robocopy $DATA $stage '/MIR'
    if (-not $rc.ok) {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        Log "FAIL:账号$name 归档复制失败(robocopy $($rc.code))，当前数据未改动"
        return (Set-SwitchError "账号$name 归档复制失败(robocopy $($rc.code))，当前数据未改动")
    }
    if (Test-Path -LiteralPath $currentDir) {
        try { Move-Item -LiteralPath $currentDir -Destination $previous -ErrorAction Stop }
        catch {
            Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
            Log "FAIL:账号$name 旧快照无法安全转存"
            return (Set-SwitchError "账号$name 旧快照无法安全转存，已停止切换")
        }
    }
    try { Move-Item -LiteralPath $stage -Destination $currentDir -ErrorAction Stop }
    catch {
        if (Test-Path -LiteralPath $previous) { Move-Item -LiteralPath $previous -Destination $currentDir -ErrorAction SilentlyContinue }
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        Log "FAIL:账号$name 归档提交失败，已回滚旧快照"
        return (Set-SwitchError "账号$name 归档提交失败，已回滚旧快照")
    }
    if (Test-Path -LiteralPath $previous) { Remove-Item -LiteralPath $previous -Recurse -Force -ErrorAction SilentlyContinue }
    Log "已存档账号$name"
    return $true
}

# 载入快照:先复制到事务暂存区，再整体换位;提交前当前数据始终保留
function Load-Account([string]$name) {
    $source = "$BASE\accounts\$name"
    $stage = Get-SwitchStagePath
    if (-not (Test-Path -LiteralPath $source)) { return (Set-SwitchError "账号$name 没有可用快照") }
    if (-not (Resolve-SwitchStageLeftover $stage $name)) { return $false }
    Register-SwitchStageOwner $stage $name
    $rc = Invoke-Robocopy $source $stage '/E'
    if (-not $rc.ok) {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        Log "FAIL:账号$name 存档载入失败(robocopy $($rc.code))"
        return (Set-SwitchError "账号$name 存档载入失败(robocopy $($rc.code))，当前数据未改动")
    }
    if (-not (Switch-DataDir $stage $name)) { return $false }
    Log "已载入账号$name 存档"
    return $true
}

# 空槽位:用空目录换位，不先删当前数据
function New-AccountData([string]$name) {
    $stage = Get-SwitchStagePath
    if (-not (Resolve-SwitchStageLeftover $stage $name)) { return $false }
    Register-SwitchStageOwner $stage $name
    if (-not (New-Item -ItemType Directory -Force -Path $stage -ErrorAction SilentlyContinue)) {
        return (Set-SwitchError "无法创建账号$name 数据目录")
    }
    if (-not (Switch-DataDir $stage $name)) { return $false }
    Log "账号$name 无存档，已初始化为全新状态"
    return $true
}

function Test-AccountSlot([string]$slot) { return ([string]$slot -match '^[A-D]$') }

# Store 安装的 Claude 没有常见 exe 路径，按 Appx 包清单拼 AUMID 启动
function Find-ClaudeAumid {
    # Get-AppxPackage 在受限账户/策略禁用环境下会直接终止脚本，这里整体兜住
    try {
        $pkg = Get-AppxPackage -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like '*Claude*' -or $_.PackageFamilyName -like '*Claude*' } |
            Select-Object -First 1
        if (-not $pkg) { return '' }
        $manifest = Get-AppxPackageManifest -Package $pkg.PackageFullName -ErrorAction Stop
        $appId = [string](@($manifest.Package.Applications.Application)[0].Id)
        if (-not $appId) { return '' }
        return "shell:AppsFolder\$($pkg.PackageFamilyName)!$appId"
    } catch {
        return ''
    }
}

function Start-ClaudeApp {
    $exe = @("$env:LOCALAPPDATA\AnthropicClaude\Claude.exe", "$env:LOCALAPPDATA\Programs\Claude\Claude.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($exe) {
        try { Start-Process $exe -ErrorAction Stop; return $true } catch {}
    }
    $aumid = Find-ClaudeAumid
    if ($aumid) {
        try { Start-Process $aumid -ErrorAction Stop; return $true } catch {}
    }
    return $false
}

function Test-KeepPathSafe([string]$RelativePath) {
    if ([string]::IsNullOrWhiteSpace($RelativePath)) { return $false }
    $rel = $RelativePath.Trim().Replace('/', '\')
    while ($rel.EndsWith('\')) { $rel = $rel.Substring(0, $rel.Length - 1) }
    while ($rel.StartsWith('.\')) { $rel = $rel.Substring(2) }
    if (-not $rel -or $rel -eq '.' -or [System.IO.Path]::IsPathRooted($rel) -or $rel -match '^[A-Za-z]:') { return $false }
    $parts = $rel.Split([char]92)
    if ($parts -contains '..' -or $parts -contains '') { return $false }
    $prefix = $DATA
    foreach ($part in $parts) {
        $prefix = Join-Path $prefix $part
        if (Test-Path -LiteralPath $prefix) {
            $item = Get-Item -LiteralPath $prefix -Force -ErrorAction SilentlyContinue
            if ($item -and ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) { return $false }
        }
    }
    return $true
}

function Stage-KeepPaths {
    $script:KeepStaged = 0
    $keepFile = Join-Path $BASE 'keep-in-workspace.txt'
    if (-not (Test-Path -LiteralPath $keepFile -PathType Leaf)) { return $true }
    $stage = Join-Path $BASE 'keep-stage'
    if (Test-Path -LiteralPath $stage) {
        return (Set-SwitchError "跨身份保留暂存目录已存在，请检查或重试上次切换:$stage")
    }
    $created = $false
    $count = 0
    try {
        foreach ($raw in @([System.IO.File]::ReadAllLines($keepFile))) {
            $rel = $raw.Trim()
            if (-not $rel -or $rel.StartsWith('#')) { continue }
            if (-not (Test-KeepPathSafe $rel)) {
                Log "WARN:keep 路径必须是相对、无越界且不含重解析点的路径，已跳过:$rel"
                continue
            }
            $normalized = $rel.Trim().Replace('/', '\').TrimEnd('\')
            while ($normalized.StartsWith('.\')) { $normalized = $normalized.Substring(2) }
            $source = Join-Path $DATA $normalized
            if (-not (Test-Path -LiteralPath $source)) { continue }
            $dest = Join-Path $stage $normalized
            $parent = Split-Path -Parent $dest
            New-Item -ItemType Directory -Force -Path $parent -ErrorAction Stop | Out-Null
            $created = $true
            $item = Get-Item -LiteralPath $source -Force -ErrorAction Stop
            if ($item.PSIsContainer) {
                $rc = Invoke-Robocopy $source $dest '/E' -ExcludeJunctions
                if (-not $rc.ok) { throw "robocopy $($rc.code)" }
            } else {
                Copy-Item -LiteralPath $source -Destination $dest -Force -ErrorAction Stop
            }
            $count++
        }
    } catch {
        if ($created) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
        return (Set-SwitchError "跨身份保留暂存失败:$($_.Exception.Message)")
    }
    if ($count -eq 0) {
        if ($created) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
        return $true
    }
    $script:KeepStaged = $count
    Log "跨身份保留:已收起 $count 条路径"
    return $true
}

function Restore-KeepPaths {
    $stage = Join-Path $BASE 'keep-stage'
    if (-not (Test-Path -LiteralPath $stage -PathType Container)) { return $true }
    $rc = Invoke-Robocopy $stage $DATA '/E' -ExcludeJunctions
    if (-not $rc.ok) {
        return (Set-SwitchError "跨身份保留还原失败，暂存保留在 $stage(robocopy $($rc.code))")
    }
    try {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction Stop
    } catch {
        return (Set-SwitchError "跨身份保留已合并，但暂存目录清理失败:$($_.Exception.Message)")
    }
    Log "跨身份保留:已还原 $($script:KeepStaged) 条路径到新账号"
    return $true
}

function Write-SwitchFailure([string]$Message) {
    $script:exitCode = 1
    "FAIL:$Message"
}

function Invoke-Switch($target) {
    $script:SwitchError = ''
    $script:exitCode = 0
    if (-not (Test-AccountSlot $target)) { $script:exitCode = 1; "FAIL:账号必须是 A、B、C 或 D"; return }
    $cur = if (Test-Path "$BASE\current.txt") { (Get-Content "$BASE\current.txt" -Raw).Trim() } else { 'none' }
    $justArchived = $false

    # 首次运行:收编当前登录为账号A
    if ($cur -eq 'none' -and (Test-Path $DATA) -and (Get-ChildItem $DATA -ErrorAction SilentlyContinue)) {
        if (-not (Stop-ClaudeApp)) { Write-SwitchFailure 'Claude 未能退出，请手动退出后重试'; return }
        if ((Test-Path "$BASE\keep-in-workspace.txt") -and -not (Stage-KeepPaths)) {
            Write-SwitchFailure $script:SwitchError; return
        }
        if (-not (Save-Account 'A')) {
            Remove-Item -LiteralPath "$BASE\keep-stage" -Recurse -Force -ErrorAction SilentlyContinue
            Write-SwitchFailure $script:SwitchError; return
        }
        $cur = 'A'; 'A' | Out-File -Encoding ascii -NoNewline "$BASE\current.txt"
        Log "首次运行:当前登录状态已收编为账号A"
        $justArchived = $true
        "NOTE:已将当前登录收编为账号A"
    }

    if ($cur -eq $target) {
        if (-not (Test-Path $DATA)) {
            if (-not (Test-Path "$BASE\accounts\$target")) { Write-SwitchFailure "账号$target 数据目录缺失，已停止切换"; return }
            if (-not (Load-Account $target)) { Write-SwitchFailure $script:SwitchError; return }
        }
        if ((Test-Path "$BASE\keep-stage") -and -not (Restore-KeepPaths)) {
            Write-SwitchFailure $script:SwitchError; return
        }
        if (-not (Start-ClaudeApp)) { "WARN:未找到 Claude 可执行文件，请手动启动" }
        "INFO:当前已是账号$target"
        return
    }

    if (-not (Stop-ClaudeApp)) { Write-SwitchFailure 'Claude 未能退出，请手动退出后重试'; return }

    if (-not $justArchived -and $cur -ne 'none' -and (Test-Path $DATA)) {
        if ((Test-Path "$BASE\keep-in-workspace.txt") -and -not (Stage-KeepPaths)) {
            Write-SwitchFailure $script:SwitchError; return
        }
        if (-not (Save-Account $cur)) {
            Remove-Item -LiteralPath "$BASE\keep-stage" -Recurse -Force -ErrorAction SilentlyContinue
            Write-SwitchFailure $script:SwitchError; return
        }
    }

    if (Test-Path "$BASE\accounts\$target") {
        if (-not (Load-Account $target)) { Write-SwitchFailure $script:SwitchError; return }
    } else {
        if (-not (New-AccountData $target)) { Write-SwitchFailure $script:SwitchError; return }
    }
    if ((Test-Path "$BASE\keep-stage") -and -not (Restore-KeepPaths)) {
        Write-SwitchFailure $script:SwitchError; return
    }
    if (-not (Start-ClaudeApp)) { "WARN:已切换但未找到 Claude 可执行文件，请手动启动" }
    Log "已切换到账号$target 并启动"
    "OK:已切换到账号$target"
}

function Get-Fingerprint {
    "# 设备指纹体检 $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
    $officeHits = New-Object 'System.Collections.Generic.HashSet[string]'
    $sysHits = New-Object 'System.Collections.Generic.HashSet[string]'
    $fontDirs = @("$env:WINDIR\Fonts", "$env:LOCALAPPDATA\Microsoft\Windows\Fonts")
    foreach ($dir in $fontDirs) {
        if (-not (Test-Path -LiteralPath $dir -PathType Container)) { continue }
        foreach ($file in @(Get-ChildItem -LiteralPath $dir -Recurse -File -ErrorAction SilentlyContinue)) {
            if ($file.Extension -notin @('.ttf','.ttc','.otf')) { continue }
            $name = $file.Name.ToLowerInvariant()
            if ($name -match 'wqy|fzshu|fzkai|fzhei|fangsong') { [void]$officeHits.Add($name) }
            elseif ($name -match 'simsun|simhei|simkai|simfang|msyh|msjh|dengxian') { [void]$sysHits.Add($name) }
        }
    }
    if ($officeHits.Count) {
        "字体=info:检测到第三方中文办公字体:$($officeHits -join ',')"
    } elseif ($sysHits.Count) {
        "字体=info:仅含 Windows 系统自带中文字体($($sysHits.Count) 项,属正常系统特征)"
    } else {
        '字体=ok:未发现中文特征字体'
    }
    $gpu = Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | Select-Object -First 1
    $gpuName = if ($gpu -and $gpu.Name) { [string]$gpu.Name } else { '未知' }
    "GPU=ok:$gpuName"
    $resolution = if ($gpu -and $gpu.CurrentHorizontalResolution -and $gpu.CurrentVerticalResolution) { "$($gpu.CurrentHorizontalResolution)x$($gpu.CurrentVerticalResolution)" } else { '未知' }
    "显示=ok:$resolution"
    $computer = Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue
    $machine = if ($computer) { "$($computer.Manufacturer) $($computer.Model)" } else { '' }
    if ($gpuName -match 'Virtual|VMware|VirtualBox|SwiftShader|llvmpipe|Microsoft Basic' -or $machine -match 'VMware|VirtualBox|QEMU|KVM|Parallels|HVM|Hyper-V') {
        "虚拟机=warn:检测到虚拟化特征($gpuName)"
    } else {
        '虚拟机=ok:未发现常见虚拟化特征'
    }
    'CanvasWebGL=info:请打开 Web 检测页 https://yoursjeyo.github.io/claude-switcher/ 查看 Canvas/WebGL 指纹'
}

function Invoke-ExternalCapture([string]$FilePath, [string[]]$Arguments, [int]$TimeoutMs = 5000) {
    try {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $FilePath
        $psi.Arguments = ConvertTo-WindowsCommandLine $Arguments
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $process = [System.Diagnostics.Process]::Start($psi)
        if (-not $process.WaitForExit($TimeoutMs)) {
            try { $process.Kill() } catch {}
            return ''
        }
        return ($process.StandardOutput.ReadToEnd().Trim() -split "`r?`n")[0]
    } catch { return '' }
}

function Get-CliInfo {
    "# Claude Code CLI 体检 $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
    $command = Get-Command claude -ErrorAction SilentlyContinue | Select-Object -First 1
    $path = if ($command) { [string]$command.Source } else { '' }
    if ($path) {
        "CLI=ok:$path"
        $version = Invoke-ExternalCapture $path @('--version') 4000
        "版本=ok:$(if ($version) { $version } else { '未获取' })"
        $method = if ($path -match '\\.claude\\local|\\.local\\share\\claude') { 'native(官方安装脚本)' }
            elseif ($path -match 'node_modules|\\.npm|\\.bun') { 'npm/bun' }
            else { '未知(建议核对 PATH)' }
        "安装方式=info:$method"
    } else {
        'CLI=warn:PATH 中没有 claude 命令'
        '安装方式=info:未安装(桌面版可能自带内置 CLI,见下)'
    }
    $bundledRoot = Join-Path $DATA 'claude-code'
    $bundled = if (Test-Path -LiteralPath $bundledRoot) { Get-ChildItem -LiteralPath $bundledRoot -Directory -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1 } else { $null }
    if ($bundled) { "桌面版内置=ok:$($bundled.Name)" } else { '桌面版内置=info:未发现' }
    $npm = Get-Command npm.cmd,npm -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($npm) {
        $npmList = Invoke-ExternalCapture $npm.Source @('ls','-g','--depth=0','@anthropic-ai/claude-code') 6000
        if ($npmList -match '@anthropic-ai/claude-code') { "npm全局包=warn:$npmList" } else { 'npm全局包=ok:未检测到' }
    } else { 'npm全局包=ok:未检测到 npm' }
    $config = Join-Path $env:USERPROFILE '.claude'
    if (Test-Path -LiteralPath $config -PathType Container) {
        $measure = Get-ChildItem -LiteralPath $config -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum
        "配置目录=ok:$config ($(Format-AccountSize ([double]$measure.Sum)))"
    } else { '配置目录=info:未创建(尚未运行过 CLI)' }
    if ($path) { '建议=info:升级请使用原安装方式；卸载前先确认桌面版是否依赖它' }
}

function Quote-WindowsArgument([string]$Argument) {
    if ($Argument.Length -eq 0) { return '""' }
    if ($Argument -notmatch '[\s"]') { return $Argument }
    $result = New-Object System.Text.StringBuilder
    [void]$result.Append('"')
    $slashes = 0
    foreach ($char in $Argument.ToCharArray()) {
        if ($char -eq [char]92) { $slashes++; continue }
        if ($char -eq '"') {
            [void]$result.Append(('\' * ($slashes * 2 + 1)))
            [void]$result.Append('"')
            $slashes = 0
            continue
        }
        if ($slashes) { [void]$result.Append(('\' * $slashes)); $slashes = 0 }
        [void]$result.Append($char)
    }
    [void]$result.Append(('\' * ($slashes * 2)))
    [void]$result.Append('"')
    return $result.ToString()
}

function ConvertTo-WindowsCommandLine([string[]]$Arguments) {
    return (@($Arguments | ForEach-Object { Quote-WindowsArgument ([string]$_) }) -join ' ')
}

function Open-SafeUrl([string]$Url) {
    $parsed = $null
    if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$parsed) -or $parsed.Scheme -notin @('http','https') -or $Url -match '[\x00-\x20"]') {
        $script:exitCode = 1
        'FAIL:只允许有效的 http/https 链接'
        return
    }
    if ($env:CLAUDE_SWITCHER_SKIP_NET_CHECK -ne '1' -and -not (Test-TunUp) -and -not (Test-ProxyAlive)) {
        $script:exitCode = 1
        'FAIL:代理未就绪(TUN 未接管且客户端未运行),拒绝打开以免直连'
        return
    }
    $browser = [string]$env:CLAUDE_SWITCHER_CHROME
    $candidates = @(
        "$env:PROGRAMFILES\Google\Chrome\Application\chrome.exe",
        "${env:PROGRAMFILES(X86)}\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
        "$env:PROGRAMFILES\Microsoft\Edge\Application\msedge.exe",
        "${env:PROGRAMFILES(X86)}\Microsoft\Edge\Application\msedge.exe",
        "$env:LOCALAPPDATA\Microsoft\Edge\Application\msedge.exe",
        "$env:PROGRAMFILES\BraveSoftware\Brave-Browser\Application\brave.exe",
        "$env:LOCALAPPDATA\BraveSoftware\Brave-Browser\Application\brave.exe",
        "$env:LOCALAPPDATA\Chromium\Application\chrome.exe"
    )
    if (-not $browser) { $browser = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1 }
    if (-not $browser -or -not (Test-Path -LiteralPath $browser -PathType Leaf)) {
        $script:exitCode = 1
        'FAIL:未找到 Chrome/Chromium/Edge/Brave'
        return
    }
    $profile = Join-Path $env:TEMP ('claude-switcher-safe-' + [guid]::NewGuid().ToString('N'))
    $watcher = Join-Path $env:TEMP ('claude-switcher-watch-' + [guid]::NewGuid().ToString('N') + '.ps1')
    try {
        New-Item -ItemType Directory -Path $profile -ErrorAction Stop | Out-Null
        $browserArgs = @(
            "--user-data-dir=$profile", '--no-first-run', '--no-default-browser-check',
            '--disable-sync', '--disable-extensions',
            '--force-webrtc-ip-handling-policy=disable_non_proxied_udp',
            "--lang=$($LOCALE_TARGET.Replace('_','-'))", $Url
        )
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $browser
        $psi.Arguments = ConvertTo-WindowsCommandLine $browserArgs
        $psi.UseShellExecute = $false
        $browserProcess = [System.Diagnostics.Process]::Start($psi)
        $profileEsc = $profile.Replace("'", "''")
        $watcherEsc = $watcher.Replace("'", "''")
        $watchScript = "`$ErrorActionPreference = 'SilentlyContinue'`nWait-Process -Id $($browserProcess.Id)`nRemove-Item -LiteralPath '$profileEsc' -Recurse -Force`nRemove-Item -LiteralPath '$watcherEsc' -Force`n"
        [System.IO.File]::WriteAllText($watcher, $watchScript, (New-Object System.Text.UTF8Encoding($true)))
        Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File',('"' + $watcher + '"') -ErrorAction Stop
        'OK:已在一次性隔离浏览器打开(profile 将在窗口关闭后销毁)'
        'INFO:该窗口不继承日常浏览器的 cookies/插件；其中产生的会话会随窗口关闭丢弃'
    } catch {
        Remove-Item -LiteralPath $profile -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $watcher -Force -ErrorAction SilentlyContinue
        $script:exitCode = 1
        "FAIL:无法启动隔离浏览器:$($_.Exception.Message)"
    }
}

function Invoke-ProxyDiagnostics([string[]]$DiagnosticArgs) {
    $report = [ordered]@{
        schema_version = 1; mode = 'proxy_diagnostics'; scope = 'unauthenticated_transport_only'
        account_status = 'not_evaluated'; leak_protection = 'not_evaluated'; launch_allowed = $null
        application_started = $false; system_modified = $false
    }
    $proxy = ''; $public = $null
    try {
        if ($DiagnosticArgs.Count -eq 2 -and $DiagnosticArgs[0] -eq '--proxy') {
            $proxy = [string]$DiagnosticArgs[1]; $source = 'argument'
        } elseif ($DiagnosticArgs.Count -eq 2 -and $DiagnosticArgs[0] -eq '--config') {
            $raw = [System.IO.File]::ReadAllText([string]$DiagnosticArgs[1], [System.Text.Encoding]::UTF8)
            if ($raw.Length -gt 16384) { throw 'invalid config' }
            $config = $raw | ConvertFrom-Json -ErrorAction Stop
            $proxy = [string]$config.proxy_url; $source = 'config'
        } elseif ($DiagnosticArgs.Count -eq 0 -and $env:CLAUDE_SWITCHER_PROXY_URL) {
            $proxy = [string]$env:CLAUDE_SWITCHER_PROXY_URL; $source = 'environment'
        } else { throw 'invalid arguments' }
        if (-not $proxy -or $proxy -match '[\s\x00-\x1f\x7f]') { throw 'invalid proxy' }
        $uri = [Uri]$proxy
        if ($uri.Scheme -notin @('http','https','socks5','socks5h') -or -not $uri.Host -or
            $uri.Port -lt 1 -or $uri.Port -gt 65535 -or $proxy -notmatch ':\d+/?$' -or
            $uri.AbsolutePath -notin @('','/') -or $uri.Query -or $uri.Fragment) { throw 'invalid proxy' }
        $address = $null
        $loopback = ([System.Net.IPAddress]::TryParse($uri.Host.Trim('[',']'), [ref]$address) -and [System.Net.IPAddress]::IsLoopback($address)) -or $uri.Host -eq 'localhost'
        $public = [ordered]@{
            scheme = $uri.Scheme; port = $uri.Port; host_scope = $(if ($loopback) { 'loopback' } else { 'remote' })
            authentication_configured = [bool]$uri.UserInfo; source = $source
        }
    } catch {
        $report.state = 'configuration_error'; $report.error = 'invalid_or_missing_proxy'
        $report.hint = 'Use --proxy, --config, or CLAUDE_SWITCHER_PROXY_URL; an explicit port is required.'
        [pscustomobject]$report | ConvertTo-Json -Depth 8 -Compress
        $script:exitCode = 2
        return
    }

    $targets = @(
        [pscustomobject]@{ name = 'anthropic_api'; url = 'https://api.anthropic.com/v1/models' },
        [pscustomobject]@{ name = 'claude_web'; url = 'https://claude.ai/' }
    )
    $checks = @()
    foreach ($target in $targets) {
        $check = [ordered]@{ name = $target.name; url = $target.url; transport_reached = $false; access = 'unknown' }
        try {
            $curl = Get-Command curl.exe -ErrorAction SilentlyContinue | Select-Object -First 1
            if (-not $curl) { $check.error = 'curl_unavailable'; $checks += [pscustomobject]$check; continue }
            $psi = New-Object System.Diagnostics.ProcessStartInfo
            $psi.FileName = $curl.Source
            $curlArgs = @(
                '-q', '--config', '-', '-sS', '-o', 'NUL', '--proto', '=https', '--noproxy', '',
                '--connect-timeout', '6', '--max-time', '12', '-w',
                '%{http_code}\t%{http_connect}\t%{ssl_verify_result}\t%{time_appconnect}\t%{time_total}', $target.url
            )
            $psi.Arguments = ConvertTo-WindowsCommandLine $curlArgs
            $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
            $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
            $psi.EnvironmentVariables.Clear()
            foreach ($name in @('PATH','SYSTEMROOT','WINDIR','TEMP','TMP','USERPROFILE','LANG','LC_ALL','SSL_CERT_FILE','SSL_CERT_DIR','CURL_CA_BUNDLE')) {
                $value = [Environment]::GetEnvironmentVariable($name)
                if ($null -ne $value) { $psi.EnvironmentVariables[$name] = $value }
            }
            $process = [System.Diagnostics.Process]::Start($psi)
            $escapedProxy = $proxy.Replace('\', '\\').Replace('"', '\"')
            $process.StandardInput.WriteLine('proxy = "' + $escapedProxy + '"')
            $process.StandardInput.Close()
            $timedOut = -not $process.WaitForExit(15000)
            if ($timedOut) {
                try { $process.Kill() } catch {}
                $check.error = 'probe_timeout'
                $checks += [pscustomobject]$check
                continue
            }
            $stdout = $process.StandardOutput.ReadToEnd().Trim()
            $null = $process.StandardError.ReadToEnd()
            $fields = $stdout -split '\s+'
            if ($fields.Count -lt 5) { throw 'invalid probe output' }
            $status = [int]$fields[0]; $connect = [int]$fields[1]; $verify = [int]$fields[2]
            $tls = [double]::Parse($fields[3], [Globalization.CultureInfo]::InvariantCulture)
            $elapsed = [double]::Parse($fields[4], [Globalization.CultureInfo]::InvariantCulture)
            if ($status -lt 0 -or $status -gt 599 -or $connect -lt 0 -or $connect -gt 599 -or $verify -lt 0 -or
                [double]::IsNaN($tls) -or [double]::IsInfinity($tls) -or $tls -lt 0 -or
                [double]::IsNaN($elapsed) -or [double]::IsInfinity($elapsed) -or $elapsed -lt 0) { throw 'invalid probe metrics' }
            $reached = $process.ExitCode -eq 0 -and $status -gt 0 -and $verify -eq 0 -and $tls -gt 0
            $check.curl_exit = $process.ExitCode; $check.http_status = $status; $check.proxy_connect_status = $connect
            $check.tls_verified = $reached; $check.elapsed_seconds = $elapsed; $check.transport_reached = $reached
            if ($reached) {
                $check.access = if ($status -eq 401) { 'unauthenticated' } elseif ($status -in @(403,407)) { 'access_restricted' } elseif ($status -ge 500) { 'server_error' } else { 'http_response' }
            }
        } catch {
            if (-not $check.Contains('error')) { $check.error = 'invalid_probe_output' }
        }
        $checks += [pscustomobject]$check
    }
    $reachedAll = @($checks | Where-Object { -not $_.transport_reached }).Count -eq 0
    $report.proxy = $public; $report.checks = $checks
    $report.state = if ($reachedAll) { 'transport_reached' } else { 'transport_failed' }
    [pscustomobject]$report | ConvertTo-Json -Depth 8 -Compress
    $script:exitCode = if ($reachedAll) { 0 } else { 1 }
}

function Read-SnapshotPassword([string]$Mode, [string]$Prompt = '输入快照密码') {
    if ($Mode -eq '--password-stdin') { return [Console]::In.ReadLine() }
    $secure = Read-Host $Prompt -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try { return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

function Get-SnapshotKeys([string]$Password, [byte[]]$Salt) {
    $passwordBytes = [System.Text.Encoding]::UTF8.GetBytes($Password)
    try {
        # 必须用 ::new()：New-Object -ArgumentList 会把 HashAlgorithmName 结构体转成字符串，
        # Rfc2898DeriveBytes 随即抛 "is not a known hash algorithm"（Windows PowerShell 5.1 实测）
        $kdf = [System.Security.Cryptography.Rfc2898DeriveBytes]::new($passwordBytes, $Salt, 200000, [System.Security.Cryptography.HashAlgorithmName]::SHA256)
        try { $material = $kdf.GetBytes(64) } finally { $kdf.Dispose() }
        $encryptionKey = New-Object byte[] 32
        $macKey = New-Object byte[] 32
        [Array]::Copy($material, 0, $encryptionKey, 0, 32)
        [Array]::Copy($material, 32, $macKey, 0, 32)
        [Array]::Clear($material, 0, $material.Length)
        return [pscustomobject]@{ encryption = $encryptionKey; mac = $macKey }
    } finally {
        [Array]::Clear($passwordBytes, 0, $passwordBytes.Length)
    }
}

function Get-SnapshotMac([string]$Path, [byte[]]$Key) {
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    $hmac = New-Object System.Security.Cryptography.HMACSHA256 -ArgumentList (,$Key)
    try {
        $header = New-Object byte[] 40
        $offset = 0
        while ($offset -lt 40) {
            $read = $stream.Read($header, $offset, 40 - $offset)
            if ($read -le 0) { throw '快照头不完整' }
            $offset += $read
        }
        [void]$hmac.TransformBlock($header, 0, $header.Length, $header, 0)
        $stream.Position = 72
        $buffer = New-Object byte[] 1048576
        while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            [void]$hmac.TransformBlock($buffer, 0, $read, $buffer, 0)
        }
        [void]$hmac.TransformFinalBlock((New-Object byte[] 0), 0, 0)
        return ,$hmac.Hash
    } finally {
        $hmac.Dispose(); $stream.Dispose()
    }
}

function Protect-SnapshotFile([string]$PlainPath, [string]$Destination, [string]$Password) {
    $salt = New-Object byte[] 16; $iv = New-Object byte[] 16
    $random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $random.GetBytes($salt); $random.GetBytes($iv) } finally { $random.Dispose() }
    $keys = Get-SnapshotKeys $Password $salt
    $input = [System.IO.File]::OpenRead($PlainPath)
    $output = [System.IO.File]::Open($Destination, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $magic = [System.Text.Encoding]::ASCII.GetBytes('CSNAP001')
        $output.Write($magic, 0, $magic.Length); $output.Write($salt, 0, $salt.Length); $output.Write($iv, 0, $iv.Length)
        $placeholder = New-Object byte[] 32
        $output.Write($placeholder, 0, $placeholder.Length)
        $aes = [System.Security.Cryptography.Aes]::Create()
        $aes.KeySize = 256; $aes.Key = $keys.encryption; $aes.IV = $iv
        $aes.Mode = [System.Security.Cryptography.CipherMode]::CBC
        $aes.Padding = [System.Security.Cryptography.PaddingMode]::PKCS7
        $crypto = New-Object System.Security.Cryptography.CryptoStream($output, $aes.CreateEncryptor(), [System.Security.Cryptography.CryptoStreamMode]::Write)
        try { $input.CopyTo($crypto) } finally { $crypto.Dispose(); $aes.Dispose() }
    } finally {
        $input.Dispose(); $output.Dispose()
    }
    $hash = Get-SnapshotMac $Destination $keys.mac
    $output = [System.IO.File]::Open($Destination, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try { $output.Position = 40; $output.Write($hash, 0, $hash.Length); $output.Flush() } finally { $output.Dispose() }
}

function Test-SnapshotAuthentication([string]$Path, [string]$Password) {
    try {
        $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
        try {
            if ($stream.Length -lt 73) { return @{ ok = $false } }
            $header = New-Object byte[] 72
            $offset = 0
            while ($offset -lt 72) {
                $read = $stream.Read($header, $offset, 72 - $offset)
                if ($read -le 0) { return @{ ok = $false } }
                $offset += $read
            }
            $magic = [System.Text.Encoding]::ASCII.GetString($header, 0, 8)
            if ($magic -ne 'CSNAP001') { return @{ ok = $false } }
            $salt = New-Object byte[] 16; $iv = New-Object byte[] 16; $expected = New-Object byte[] 32
            [Array]::Copy($header, 8, $salt, 0, 16); [Array]::Copy($header, 24, $iv, 0, 16); [Array]::Copy($header, 40, $expected, 0, 32)
        } finally { $stream.Dispose() }
        $keys = Get-SnapshotKeys $Password $salt
        $actual = Get-SnapshotMac $Path $keys.mac
        $difference = 0
        for ($i = 0; $i -lt 32; $i++) { $difference = $difference -bor ($actual[$i] -bxor $expected[$i]) }
        if ($difference -ne 0) { return @{ ok = $false } }
        return @{ ok = $true; keys = $keys; iv = $iv }
    } catch { return @{ ok = $false } }
}

function Unprotect-SnapshotFile([string]$Path, [string]$PlainPath, [string]$Password) {
    $auth = Test-SnapshotAuthentication $Path $Password
    if (-not $auth.ok) { return $false }
    try {
        $input = [System.IO.File]::OpenRead($Path)
        $output = [System.IO.File]::Open($PlainPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        try {
            $input.Position = 72
            $aes = [System.Security.Cryptography.Aes]::Create()
            $aes.KeySize = 256; $aes.Key = $auth.keys.encryption; $aes.IV = $auth.iv
            $aes.Mode = [System.Security.Cryptography.CipherMode]::CBC; $aes.Padding = [System.Security.Cryptography.PaddingMode]::PKCS7
            $crypto = New-Object System.Security.Cryptography.CryptoStream($input, $aes.CreateDecryptor(), [System.Security.Cryptography.CryptoStreamMode]::Read)
            try { $crypto.CopyTo($output) } finally { $crypto.Dispose(); $aes.Dispose() }
        } finally { $input.Dispose(); $output.Dispose() }
        return $true
    } catch {
        Remove-Item -LiteralPath $PlainPath -Force -ErrorAction SilentlyContinue
        return $false
    }
}

function Test-SnapshotArchive([string]$ZipPath, [string]$Target) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stream = [System.IO.File]::OpenRead($ZipPath)
    $zip = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Read)
    try {
        $seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
        $marker = $null
        foreach ($entry in $zip.Entries) {
            $name = [string]$entry.FullName
            if (-not $name -or $name.Contains('\') -or $name.Contains(':') -or [System.IO.Path]::IsPathRooted($name) -or $name.StartsWith('/') -or $name -match '^[A-Za-z]:') { throw '快照包含非法路径' }
            $parts = $name.Split('/')
            if ($parts -contains '..') { throw '快照包含越界路径' }
            $fileType = ([uint32]$entry.ExternalAttributes -shr 16) -band 0xF000
            if ($fileType -eq 0xA000) { throw '快照包含符号链接' }
            if (-not $seen.Add($name)) { throw '快照包含重复路径' }
            if ($name -eq '.claude-switcher-snapshot.json') { $marker = $entry }
        }
        if (-not $marker -or $marker.Length -gt 16384) { throw '缺少快照元数据' }
        $reader = New-Object System.IO.StreamReader($marker.Open(), [System.Text.Encoding]::UTF8)
        try { $meta = ($reader.ReadToEnd() | ConvertFrom-Json -ErrorAction Stop) } finally { $reader.Dispose() }
        if ($meta.format -ne 'claude-switcher-snapshot' -or [int]$meta.version -ne 1) { throw '快照格式不支持' }
        if ([string]$meta.account -notmatch '^[A-D]$' -or [string]$meta.account -ne $Target) { throw '账号不匹配或元数据无效' }
        return $true
    } finally { $zip.Dispose(); $stream.Dispose() }
}

function Expand-SnapshotArchive([string]$ZipPath, [string]$Destination) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($ZipPath, $Destination)
}

# .NET Framework 的 ZipFile::CreateFromDirectory 用 '\' 写条目名，会被 Test-SnapshotArchive
# 的路径校验判为「非法路径」；这里手工写条目，统一成 '/'（.NET Core 才默认 '/')
function New-SnapshotArchive([string]$SourceDir, [string]$ZipPath) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $root = [System.IO.Path]::GetFullPath($SourceDir).TrimEnd('\', '/')
    $stream = [System.IO.File]::Open($ZipPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $zip = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($dir in @(Get-ChildItem -LiteralPath $root -Recurse -Directory -Force -ErrorAction SilentlyContinue)) {
                [void]$zip.CreateEntry($dir.FullName.Substring($root.Length + 1).Replace('\', '/') + '/')
            }
            foreach ($file in @(Get-ChildItem -LiteralPath $root -Recurse -File -Force -ErrorAction SilentlyContinue)) {
                $entry = $zip.CreateEntry($file.FullName.Substring($root.Length + 1).Replace('\', '/'), [System.IO.Compression.CompressionLevel]::Optimal)
                $entry.ExternalAttributes = [int]$file.Attributes
                $entryStream = $entry.Open()
                try {
                    $fileStream = [System.IO.File]::OpenRead($file.FullName)
                    try { $fileStream.CopyTo($entryStream) } finally { $fileStream.Dispose() }
                } finally { $entryStream.Dispose() }
            }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
}

function Export-EncryptedSnapshot([string]$Target, [string]$Out, [string]$Mode) {
    if (-not (Test-AccountSlot $Target)) { $script:exitCode = 1; 'FAIL:账号必须是 A、B、C 或 D'; return }
    $source = Join-Path $BASE "accounts\$Target"
    if (-not (Test-Path -LiteralPath $source -PathType Container)) { $script:exitCode = 1; "FAIL:账号$Target 没有已归档快照"; return }
    if ([string]::IsNullOrWhiteSpace($Out)) { $script:exitCode = 1; 'FAIL:请指定导出文件路径'; return }
    $Out = [System.IO.Path]::GetFullPath($Out)
    if (Test-Path -LiteralPath $Out) { $script:exitCode = 1; 'FAIL:目标文件已存在，请换一个路径'; return }
    $current = if (Test-Path "$BASE\current.txt") { (Get-Content "$BASE\current.txt" -Raw).Trim() } else { '' }
    if ($current -eq $Target) { $script:exitCode = 1; "FAIL:账号$Target 正在使用中，请先切换到另一账号再导出"; return }
    $password = Read-SnapshotPassword $Mode '设置导出密码(至少 8 位)'
    if ($Mode -ne '--password-stdin') {
        $confirm = Read-SnapshotPassword $Mode '再次输入'
        if ($password -cne $confirm) { $script:exitCode = 1; 'FAIL:两次密码不一致'; return }
    }
    if ([string]::IsNullOrEmpty($password) -or $password.Length -lt 8) { $script:exitCode = 1; 'FAIL:密码至少 8 位'; return }
    $stage = Join-Path $env:TEMP ('claude-snapshot-' + [guid]::NewGuid().ToString('N'))
    $plain = Join-Path $env:TEMP ('claude-snapshot-' + [guid]::NewGuid().ToString('N') + '.zip')
    try {
        $rc = Invoke-Robocopy $source $stage '/E' -ExcludeJunctions
        if (-not $rc.ok) { throw "复制快照失败(robocopy $($rc.code))" }
        $meta = [ordered]@{ format = 'claude-switcher-snapshot'; version = 1; account = $Target; created = ([DateTime]::UtcNow.ToString('o')); platform = 'Windows' }
        [System.IO.File]::WriteAllText((Join-Path $stage '.claude-switcher-snapshot.json'), ($meta | ConvertTo-Json -Compress), (New-Object System.Text.UTF8Encoding($false)))
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        New-SnapshotArchive $stage $plain
        Protect-SnapshotFile $plain $Out $password
        "OK:账号$Target 已加密导出到 $Out"
    } catch {
        Remove-Item -LiteralPath $Out -Force -ErrorAction SilentlyContinue
        $script:exitCode = 1
        "FAIL:$($_.Exception.Message)"
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $plain -Force -ErrorAction SilentlyContinue
        $password = $null; $confirm = $null
    }
}

function Import-EncryptedSnapshot([string]$File, [string]$Target, [string]$Mode) {
    if (-not (Test-AccountSlot $Target)) { $script:exitCode = 1; 'FAIL:账号必须是 A、B、C 或 D'; return }
    if (-not (Test-Path -LiteralPath $File -PathType Leaf)) { $script:exitCode = 1; 'FAIL:找不到快照文件'; return }
    $destination = Join-Path $BASE "accounts\$Target"
    $stage = "$BASE\accounts\$target.new"
    $previous = "$BASE\accounts\$target.previous"
    foreach ($path in @($destination, $stage, $previous)) {
        if (Test-Path -LiteralPath $path) { $script:exitCode = 1; "FAIL:账号$Target 已有快照或待恢复数据，拒绝覆盖"; return }
    }
    $password = Read-SnapshotPassword $Mode '输入快照密码'
    if ([string]::IsNullOrEmpty($password)) { $script:exitCode = 1; 'FAIL:密码为空'; return }
    $plain = Join-Path $env:TEMP ('claude-import-' + [guid]::NewGuid().ToString('N') + '.zip')
    $extract = Join-Path $env:TEMP ('claude-import-' + [guid]::NewGuid().ToString('N'))
    $ownsStage = $false
    try {
        if (-not (Unprotect-SnapshotFile $File $plain $password)) { throw '密码错误或快照损坏' }
        if (-not (Test-SnapshotArchive $plain $Target)) { throw '快照元数据校验失败' }
        Expand-SnapshotArchive $plain $extract
        Remove-Item -LiteralPath (Join-Path $extract '.claude-switcher-snapshot.json') -Force -ErrorAction Stop
        $ownsStage = $true
        $rc = Invoke-Robocopy $extract $stage '/E' -ExcludeJunctions
        if (-not $rc.ok) { throw "写入账号目录失败(robocopy $($rc.code))" }
        Move-Item -LiteralPath $stage -Destination $destination -ErrorAction Stop
        $ownsStage = $false
        "OK:已导入为账号$Target(切换时生效)"
    } catch {
        if ($ownsStage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
        $script:exitCode = 1
        "FAIL:$($_.Exception.Message)"
    } finally {
        Remove-Item -LiteralPath $plain -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $extract -Recurse -Force -ErrorAction SilentlyContinue
        $password = $null
    }
}

function Get-ReleaseInfo {
    $current = Get-AppVersion
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        # GitHub 要求 User-Agent;PowerShell 5.1 的 -Headers 不允许改它,-UserAgent 等价写入请求头
        $rel = Invoke-RestMethod -Uri 'https://api.github.com/repos/YOURSJEYO/claude-switcher/releases/latest' -UserAgent 'ClaudeSwitcher' -TimeoutSec 8
        $latest = ([string]$rel.tag_name).Trim() -replace '^[vV]', ''
        if (-not $latest) { throw '无法解析最新版本号' }

        $zip = @($rel.assets) | Where-Object { $_ -and $_.name -like '*-windows.zip' } | Select-Object -First 1
        $sums = @($rel.assets) | Where-Object { $_ -and $_.name -eq 'SHA256SUMS-release.txt' } | Select-Object -First 1
        $zipUrl = if ($zip) { [string]$zip.browser_download_url } else { '' }
        $sumsUrl = if ($sums) { [string]$sums.browser_download_url } else { '' }
        $pageUrl = [string]$rel.html_url
        $status = if ((Compare-Version $latest $current) -gt 0) { 'update_available' } else { 'latest' }

        if ($status -eq 'update_available' -and (-not (Test-GitHubDownloadUrl $zipUrl) -or -not (Test-GitHubDownloadUrl $sumsUrl))) {
            return @{ status = 'error'; current = $current; latest = $latest; zip = ''; sums = ''; page = ''; message = '更新资产缺失或下载地址不是 GitHub 官方域名' }
        }
        return @{ status = $status; current = $current; latest = $latest; zip = $zipUrl; sums = $sumsUrl; page = $pageUrl; message = '' }
    } catch {
        $msg = ([string]$_.Exception.Message) -replace '\r?\n', ' '
        if ($msg -match 'rate limit|API rate|403') { $msg = 'GitHub 接口限流，请稍后再试' }
        return @{ status = 'error'; current = $current; latest = ''; zip = ''; sums = ''; page = ''; message = $msg }
    }
}

function Write-ReleaseInfo($info) {
    switch ($info.status) {
        'update_available' {
            "STATUS=update_available"
            "CURRENT=$($info.current)"
            "LATEST=$($info.latest)"
            "ZIP_URL=$($info.zip)"
            "SUMS_URL=$($info.sums)"
            "PAGE_URL=$($info.page)"
        }
        'latest' {
            "STATUS=latest"
            "CURRENT=$($info.current)"
            "LATEST=$($info.latest)"
        }
        default {
            "STATUS=error"
            "MESSAGE=$(([string]$info.message) -replace '\r?\n', ' ')"
        }
    }
}

function Invoke-CheckUpdate {
    Write-ReleaseInfo (Get-ReleaseInfo)
}

function Invoke-UpdateNow {
    $info = Get-ReleaseInfo
    if ($info.status -ne 'update_available') { Write-ReleaseInfo $info; return }

    $previous = $info.current
    $tmp = Join-Path $env:TEMP ("claude-switcher-update-" + [guid]::NewGuid().ToString('N'))
    $files = @('claude-switch.ps1', 'claude-bodyguard.ps1', 'SwitcherUI.ps1')
    $backupDir = $null
    $backedUp = @{}
    $task = Get-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue
    $versionPath = Join-Path $BASE 'version.txt'
    $versionExisted = Test-Path -LiteralPath $versionPath
    $versionRaw = $null
    if ($versionExisted) {
        try { $versionRaw = [System.IO.File]::ReadAllText($versionPath) } catch { $versionExisted = $false }
    }

    try {
        New-Item -ItemType Directory -Force -Path $tmp -ErrorAction Stop | Out-Null

        $zipUrl = [string]$info.zip
        $sumsUrl = [string]$info.sums
        if (-not (Test-GitHubDownloadUrl $zipUrl)) { throw '更新包下载地址不是 GitHub 官方域名' }
        if (-not (Test-GitHubDownloadUrl $sumsUrl)) { throw '校验文件下载地址不是 GitHub 官方域名' }

        $zipName = [System.IO.Path]::GetFileName(([Uri]$zipUrl).AbsolutePath)
        if ([string]::IsNullOrWhiteSpace($zipName)) { $zipName = 'windows-update.zip' }
        $zipPath = Join-Path $tmp $zipName
        $sumsPath = Join-Path $tmp 'SHA256SUMS-release.txt'
        Invoke-WebRequest -Uri $zipUrl -OutFile $zipPath -UseBasicParsing -TimeoutSec 60 -ErrorAction Stop
        Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -UseBasicParsing -TimeoutSec 60 -ErrorAction Stop

        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sumsPath -ErrorAction Stop) {
            if ($line -match '^([0-9A-Fa-f]{64})\s+\*?(.+?)\s*$') {
                $name = [System.IO.Path]::GetFileName($Matches[2].Trim())
                if ($name -eq $zipName) { $expected = $Matches[1].ToUpperInvariant(); break }
            }
        }
        if (-not $expected) { throw 'SHA256SUMS 中未找到更新包哈希' }
        $actual = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256 -ErrorAction Stop).Hash.ToUpperInvariant()
        if ($actual -ne $expected) { throw '校验失败' }

        $extract = Join-Path $tmp 'extract'
        Expand-Archive -LiteralPath $zipPath -DestinationPath $extract -Force -ErrorAction Stop
        $newCore = Get-ChildItem -LiteralPath $extract -Recurse -Filter 'claude-switch.ps1' -File -ErrorAction Stop | Select-Object -First 1
        if (-not $newCore) { throw '更新包中未找到 claude-switch.ps1' }
        $pkgDir = $newCore.Directory.FullName
        foreach ($fn in $files) {
            if (-not (Test-Path -LiteralPath (Join-Path $pkgDir $fn))) { throw "更新包缺少 $fn" }
        }

        # 保留当前安装的 5 项用户配置
        $currentText = [System.IO.File]::ReadAllText((Join-Path $BASE 'claude-switch.ps1'))
        $newText = [System.IO.File]::ReadAllText($newCore.FullName)
        foreach ($name in @('PROXY_MODE', 'COUNTRY_TARGET', 'TZ_TARGET', 'LOCALE_TARGET', 'PROXY_GROUP')) {
            $value = Get-ScriptConfigValue $currentText $name
            if ($null -ne $value) { $newText = Set-ScriptConfigValue $newText $name ([string]$value) }
        }

        $safeVersion = if ($previous) { $previous -replace '[^0-9A-Za-z._-]', '_' } else { 'unknown' }
        $backupDir = Join-Path $BASE ("backup-" + $safeVersion + "-" + (Get-Date -Format 'yyyyMMddHHmmss'))
        New-Item -ItemType Directory -Force -Path $backupDir -ErrorAction Stop | Out-Null
        foreach ($fn in $files) {
            $src = Join-Path $BASE $fn
            if (Test-Path -LiteralPath $src) {
                Copy-Item -LiteralPath $src -Destination (Join-Path $backupDir $fn) -Force -ErrorAction Stop
                $backedUp[$fn] = $true
            } else {
                $backedUp[$fn] = $false
            }
        }

        if ($task) { Stop-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue }

        $stagedCore = Join-Path $tmp 'claude-switch.new.ps1'
        [System.IO.File]::WriteAllText($stagedCore, $newText, (New-Object System.Text.UTF8Encoding($true)))
        Copy-Item -LiteralPath $stagedCore -Destination (Join-Path $BASE 'claude-switch.ps1') -Force -ErrorAction Stop
        foreach ($fn in @('claude-bodyguard.ps1', 'SwitcherUI.ps1')) {
            Copy-Item -LiteralPath (Join-Path $pkgDir $fn) -Destination (Join-Path $BASE $fn) -Force -ErrorAction Stop
        }
        [System.IO.File]::WriteAllText($versionPath, [string]$info.latest, (New-Object System.Text.UTF8Encoding($false)))
        if ($task) { Start-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction Stop }

        "STATUS=updated"
        "LATEST=$($info.latest)"
        "PREVIOUS=$previous"
        "MESSAGE=更新完成"
    } catch {
        $reason = ([string]$_.Exception.Message) -replace '\r?\n', ' '
        if ($backupDir -and (Test-Path -LiteralPath $backupDir)) {
            foreach ($fn in $files) {
                $dst = Join-Path $BASE $fn
                $bak = Join-Path $backupDir $fn
                if ($backedUp[$fn] -and (Test-Path -LiteralPath $bak)) {
                    Copy-Item -LiteralPath $bak -Destination $dst -Force -ErrorAction SilentlyContinue
                } elseif ($backedUp.ContainsKey($fn) -and -not $backedUp[$fn]) {
                    Remove-Item -LiteralPath $dst -Force -ErrorAction SilentlyContinue
                }
            }
            try {
                if ($versionExisted -and $null -ne $versionRaw) {
                    [System.IO.File]::WriteAllText($versionPath, $versionRaw, (New-Object System.Text.UTF8Encoding($false)))
                } else {
                    Remove-Item -LiteralPath $versionPath -Force -ErrorAction SilentlyContinue
                }
            } catch {}
        }
        if ($task) {
            try { Start-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue } catch {}
        }
        "STATUS=error"
        "MESSAGE=$reason"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# 只读诊断:控制接口是否可达、Script.js 是否写入、内核前 18 条规则是什么
function Invoke-RulesStatus {
    if (-not (Test-MihomoClientSelected)) {
        "client=$PROXY_MODE(非 Mihomo 客户端,无 Mihomo 控制接口)"
        Get-RulesStatus
        return
    }
    $api = Get-ClashApi
    "mode=$(Get-ClashMode)"
    if ($api) {
        $auth = if ($api.headers.Count -gt 0) { 'configured' } else { 'none' }
        "api=source=$($api.source) endpoint=$($api.base) auth=$auth"
    } else {
        if ($PROXY_MODE -eq 'flclash') { 'api=未配置(填写 FlClash 本机端口，或确认 external-controller)' }
        else { 'api=未配置(检查 Clash Verge 的 external-controller)' }
    }
    $cfg = Invoke-ClashApi '/configs' 5
    if ($cfg.ok) { "configs=ok mode=$(([string]$cfg.data.mode).ToLowerInvariant())" } else { "configs=fail:$($cfg.reason)" }
    $info = Get-ClashRules
    if ($info.available) {
        $pinned = if (Test-PinnedRules $info.rules) { 'yes' } else { 'no' }
        "rules=ok count=$(@($info.rules).Count) pinned=$pinned"
        $i = 0
        foreach ($r in @($info.rules | Select-Object -First 18)) {
            $i++
            "rule{0:d2}={1} {2} -> {3}" -f $i, $r.type, $r.payload, $r.proxy
        }
    } else {
        "rules=fail:$($info.reason)"
        "hint=$(Get-ClashApiHint $info.reason)"
    }
    $scriptPath = Join-Path $CLASH_DIR 'profiles\Script.js'
    if (Test-Path -LiteralPath $scriptPath) {
        $managed = if (Test-ClashScriptManaged) { 'yes' } else { 'no' }
        $stamp = (Get-Item -LiteralPath $scriptPath).LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss')
        "script=exists managed=$managed modified=$stamp"
    } else {
        'script=missing'
    }
    Get-RulesStatus
}

# ============ EnvGuard 新增:受检安全启动 / 护盾还原 / 单次聚合 JSON ============
function Invoke-SafeLaunch([string]$Target = 'desktop', [string[]]$ExtraArgs = @()) {
    $mode = if ([string]::IsNullOrWhiteSpace($Target)) { 'desktop' } else { $Target.Trim().ToLowerInvariant() }
    if ($mode -notin @('desktop','cli')) {
        $script:exitCode = 1
        'FAIL:用法: claude-switch.ps1 launch [desktop|cli] [args...]'
        return
    }
    if ($mode -eq 'cli') {
        $authOverrides = @('ANTHROPIC_API_KEY','ANTHROPIC_AUTH_TOKEN','CLAUDE_CODE_OAUTH_TOKEN','CLAUDE_CODE_USE_BEDROCK','CLAUDE_CODE_USE_VERTEX','CLAUDE_CODE_USE_FOUNDRY','ANTHROPIC_PROFILE')
        $hitEnv = @($authOverrides | Where-Object { -not [string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($_)) })
        if ($hitEnv.Count -gt 0) {
            $script:exitCode = 1
            "FAIL:检测到认证环境变量覆盖($($hitEnv -join ',')),已拒绝启动"
            return
        }
    }
    $checkOut = @(Invoke-Check)
    if ($script:exitCode -ne 0 -or ($checkOut -join "`n") -match '(?m)^FAIL:') {
        $script:exitCode = 1
        $checkOut
        return
    }
    if (-not ((Test-ProxyAlive) -and (Test-TunUp))) {
        $script:exitCode = 1
        'FAIL:代理路径未就绪(内核或TUN离线),已阻断启动'
        return
    }
    if ($mode -eq 'desktop') {
        if (-not (Start-ClaudeApp)) {
            $script:exitCode = 1
            'FAIL:未找到 Claude 桌面版可执行文件或商店应用'
            return
        }
        Log '安全启动 Claude 桌面版'
        'OK:环境检查通过,已安全启动 Claude 桌面版'
        return
    }
    $cliCmd = Get-Command claude -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cliCmd) {
        $bundledRoot = Join-Path $DATA 'claude-code'
        $bundledExe = if (Test-Path -LiteralPath $bundledRoot) {
            Get-ChildItem -LiteralPath $bundledRoot -Recurse -Filter 'claude.exe' -File -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
        } else { $null }
        if ($bundledExe) { $cliPath = $bundledExe.FullName } else { $cliPath = '' }
    } else {
        $cliPath = [string]$cliCmd.Source
    }
    if (-not $cliPath) {
        $script:exitCode = 1
        'FAIL:未找到 Claude Code CLI(claude)'
        return
    }
    $argLine = if ($ExtraArgs.Count -gt 0) { ' ' + (ConvertTo-WindowsCommandLine $ExtraArgs) } else { '' }
    $escapedCli = $cliPath.Replace("'", "''")
    $launchCmd  = "& '$escapedCli'$argLine"
    Start-Process powershell.exe -ArgumentList @('-NoExit','-NoProfile','-ExecutionPolicy','Bypass','-Command',$launchCmd) -ErrorAction Stop | Out-Null
    Log '安全启动 Claude Code CLI'
    'OK:环境检查通过,已在独立终端启动 Claude Code CLI'
}

# npm/bun 方式运行的 Claude Code CLI;原生 claude.exe 与桌面版同名,由 Stop-ClaudeApp 一并处理
function Get-ClaudeCliHostIds {
    $session = try { (Get-Process -Id $PID -ErrorAction Stop).SessionId } catch { -1 }
    $hostIds = @(Get-Process -Name 'node','bun' -ErrorAction SilentlyContinue | Where-Object { $session -lt 0 -or $_.SessionId -eq $session } | ForEach-Object { [int]$_.Id })
    if (-not $hostIds.Count) { return @() }
    return @(Get-CimInstance Win32_Process -Filter "Name='node.exe' OR Name='bun.exe'" -ErrorAction SilentlyContinue | Where-Object {
        ($hostIds -contains [int]$_.ProcessId) -and ([string]$_.CommandLine -match '@anthropic-ai[\\/]+claude-code')
    } | ForEach-Object { [int]$_.ProcessId })
}

function Stop-ClaudeCliHosts {
    foreach ($id in @(Get-ClaudeCliHostIds)) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
    for ($i = 0; $i -lt 5; $i++) {
        if (-not @(Get-ClaudeCliHostIds).Count) { return $true }
        Start-Sleep 1
    }
    return $false
}

function Invoke-ShieldRestore {
    # 撤防前必须先确保受保护进程已完全退出,绝不在运行中撤防
    if (-not (Stop-ClaudeApp)) {
        $script:exitCode = 1
        'FAIL:Claude 桌面版未能退出,拒绝在运行中撤除防护'
        return
    }
    if (-not (Stop-ClaudeCliHosts)) {
        $script:exitCode = 1
        'FAIL:Claude Code CLI 未能退出,拒绝在运行中撤除防护'
        return
    }
    $homeTzFile = Join-Path $BASE 'home-tz.txt'
    $restoreTz = ''
    if (Test-Path -LiteralPath $homeTzFile -PathType Leaf) {
        try { $restoreTz = ([System.IO.File]::ReadAllText($homeTzFile, [System.Text.Encoding]::UTF8)).Trim() } catch {}
    }
    $ipv6StateFile = Join-Path $BASE 'ipv6-state.json'
    $savedAdapters = $null
    if (Test-Path -LiteralPath $ipv6StateFile -PathType Leaf) {
        try {
            $parsedState = ([System.IO.File]::ReadAllText($ipv6StateFile, [System.Text.Encoding]::UTF8)) | ConvertFrom-Json -ErrorAction Stop
            if ($null -ne $parsedState -and [int]$parsedState.version -eq 1 -and $parsedState.adapters -is [pscustomobject]) {
                $savedAdapters = $parsedState.adapters
            }
        } catch {}
    }
    try { Stop-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue } catch {}
    $hbFile = Join-Path $BASE 'bodyguard-heartbeat.txt'
    if (Test-Path -LiteralPath $hbFile) {
        try {
            $raw = ([System.IO.File]::ReadAllText($hbFile, [System.Text.Encoding]::ASCII)).Trim()
            if ($raw -match '^(\d+)\s+') { Stop-Process -Id ([int]$Matches[1]) -Force -ErrorAction SilentlyContinue }
        } catch {}
        Remove-Item -LiteralPath $hbFile -Force -ErrorAction SilentlyContinue
    }
    $adminLines = @(
        "Stop-ScheduledTask -TaskName 'ClaudeBodyguard' -ErrorAction SilentlyContinue"
    )
    $ipv6Summary = ''
    if ($null -ne $savedAdapters) {
        $enableNames = @()
        $disableNames = @()
        foreach ($prop in @($savedAdapters.PSObject.Properties)) {
            if (-not $prop.Name) { continue }
            if ([bool]$prop.Value) { $enableNames += [string]$prop.Name }
            else { $disableNames += [string]$prop.Name }
        }
        foreach ($name in $enableNames) {
            $esc = $name.Replace("'", "''")
            $adminLines += "Enable-NetAdapterBinding -Name '$esc' -ComponentID ms_tcpip6 -Confirm:`$false -ErrorAction SilentlyContinue"
        }
        foreach ($name in $disableNames) {
            $esc = $name.Replace("'", "''")
            $adminLines += "Disable-NetAdapterBinding -Name '$esc' -ComponentID ms_tcpip6 -Confirm:`$false -ErrorAction SilentlyContinue"
        }
        $ipv6Summary = if ($disableNames.Count -gt 0) {
            "已按原记录恢复 $($enableNames.Count) 块网卡的 IPv6(其余 $($disableNames.Count) 块保持原关闭)"
        } else {
            "已按原记录恢复 $($enableNames.Count) 块网卡的 IPv6"
        }
    } else {
        'WARN:未找到护盾开启前的网卡 IPv6 记录(ipv6-state.json),将对当前所有网卡启用 IPv6'
        $adminLines += "Get-NetAdapterBinding -ComponentID ms_tcpip6 -ErrorAction SilentlyContinue | Where-Object { -not `$_.Enabled } | Enable-NetAdapterBinding -Confirm:`$false"
        $ipv6Summary = '已对全部网卡启用 IPv6(无原状态记录)'
    }
    $currentTz = (Get-TimeZone).Id
    $tzSummary = ''
    if ($restoreTz) {
        if ($currentTz -ne $restoreTz) {
            $tzEsc = $restoreTz.Replace("'", "''")
            $adminLines += "Set-TimeZone -Id '$tzEsc'"
            $adminLines += "if ((Get-TimeZone).Id -ne '$tzEsc') { exit 1 }"
            $tzSummary = "已还原时区为 $restoreTz"
        } else {
            $tzSummary = "时区已是原记录($restoreTz)"
        }
    } else {
        "WARN:未找到原时区记录(home-tz.txt,可能由旧版安装器修改),未改动当前时区($currentTz),如需恢复请在系统设置手动调整"
        $tzSummary = "无原时区记录(保持 $currentTz)"
    }
    $adminLines += "Clear-DnsClientCache"
    $tmp = "$env:TEMP\claude-restore-$([guid]::NewGuid().Guid).ps1"
    try {
        [System.IO.File]::WriteAllText($tmp, ($adminLines -join "`r`n"), (New-Object System.Text.UTF8Encoding($true)))
        $proc = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$tmp`""
        if (-not $proc -or $proc.ExitCode -ne 0) {
            $script:exitCode = 1
            'FAIL:管理员授权未完成,日常网络设置未完全恢复'
            return
        }
    } catch {
        $script:exitCode = 1
        'FAIL:管理员授权被取消,日常网络设置未恢复'
        return
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $savedAdapters) {
        Remove-Item -LiteralPath $ipv6StateFile -Force -ErrorAction SilentlyContinue
    }
    Log "已退出护盾($ipv6Summary;$tzSummary)"
    "OK:已暂停保镖，$ipv6Summary，$tzSummary"
}

function Invoke-UiState([bool]$Fresh = $false) {
    $checkLines = @()
    if ($Fresh) {
        $checkLines = @(Invoke-CheckDetail)
        try { $checkLines | Out-File -Encoding utf8 "$BASE\last-check.txt" } catch {}
    } elseif (Test-Path -LiteralPath "$BASE\last-check.txt") {
        try { $checkLines = @([System.IO.File]::ReadAllLines("$BASE\last-check.txt", [System.Text.Encoding]::UTF8)) } catch {}
    }
    $checks = @()
    foreach ($line in $checkLines) {
        if ($line -match '^([^=]+)=(ok|warn|fail|skip)(?::(.*))?$') {
            $checks += [pscustomobject]@{
                name   = $Matches[1]
                state  = $Matches[2]
                detail = $(if ($Matches[3]) { $Matches[3] } else { '' })
            }
        }
    }
    $cfg = try { Get-CheckConfig } catch { @{ disabled = @(); allowOptionalFailures = $false; logging = $true } }
    $override = Get-FlClashControllerOverride
    $portSetting = 'auto'
    if (-not [string]::IsNullOrWhiteSpace($override)) {
        $chosen = ConvertTo-LoopbackClashEndpoint $override '' 'flclash-override'
        if ($chosen) { $portSetting = ($chosen.base -replace '^http://', '') }
    }
    $bg = Get-BodyguardHealth
    $ianaTz = ConvertFrom-WindowsTimeZone $TZ_TARGET $COUNTRY_TARGET
    $stateMap = @{}
    foreach ($c in $checks) { $stateMap[$c.name] = $c.state }
    $ready = ($checks.Count -eq 14) -and
        (-not @($checks | Where-Object { $_.state -eq 'fail' }).Count) -and
        (-not @($HARD_CHECKS | Where-Object { $stateMap[$_] -ne 'ok' }).Count)
    $stateObj = [ordered]@{
        schema_version = 1
        version        = (Get-AppVersion)
        ready          = [bool]$ready
        hard_checks    = $HARD_CHECKS
        proxy_mode     = $PROXY_MODE
        clients        = @('clash-verge','flclash','sing-box','clashx','surge','wireguard','other','auto')
        flclash_port   = $portSetting
        country_target = $COUNTRY_TARGET
        tz_target      = $TZ_TARGET
        iana_tz_target = $ianaTz
        locale_target  = $LOCALE_TARGET
        system_tz      = (Get-TimeZone).Id
        system_locale  = [System.Globalization.CultureInfo]::CurrentCulture.Name
        bodyguard      = [ordered]@{
            installed     = [bool]$bg.installed
            running       = [bool]$bg.running
            legacy_policy = [bool]$bg.legacyPolicy
            state         = [string]$bg.state
            detail        = [string]$bg.detail
        }
        check_config   = [ordered]@{
            disabled              = @($cfg.disabled)
            allowOptionalFailures = [bool]$cfg.allowOptionalFailures
            logging               = [bool]$cfg.logging
        }
        checks         = $checks
    }
    ([pscustomobject]$stateObj) | ConvertTo-Json -Depth 6 -Compress
}

# 本引擎已合并到的上游 YOURSJEYO/claude-switcher main 提交;每次合并 upstream 分支后同步更新
$UPSTREAM_MERGED = 'cf474a23bd4c7467163721399082568f0cb95cf7'

# 只读:读上游 main 的提交订阅(github.com 网页端,不受 API 按出口 IP 限流影响),列出尚未合并的提交
function Invoke-UpstreamStatus {
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $response = Invoke-WebRequest -Uri 'https://github.com/YOURSJEYO/claude-switcher/commits/main.atom' -UserAgent 'EnvGuard' -UseBasicParsing -TimeoutSec 10 -ErrorAction Stop
        $entries = @(([xml]$response.Content).feed.entry)
        if ($entries.Count -eq 0) { throw '订阅为空' }
    } catch {
        $script:exitCode = 1
        'FAIL:无法读取上游提交记录(GitHub 不可达或被限流),请稍后再试'
        return
    }
    $newer = @()
    $reached = $false
    foreach ($entry in $entries) {
        $sha = ([string]$entry.id) -replace '^.*/', ''
        if ($sha -eq $UPSTREAM_MERGED) { $reached = $true; break }
        $title = $entry.title
        if ($title -isnot [string]) { $title = [string]$title.'#text' }
        $date = ([string]$entry.updated) -replace 'T.*$', ''
        $newer += [pscustomobject]@{ Line = "  $date $($sha.Substring(0, [Math]::Min(7, $sha.Length))) $($title.Trim())"; Windows = ($title -match '(?i)windows') }
    }
    $merged = $UPSTREAM_MERGED.Substring(0, 7)
    if ($reached -and $newer.Count -eq 0) {
        "OK:引擎已合并上游 main@$merged,上游暂无新提交"
        return
    }
    $count = if ($reached) { "有 $($newer.Count) 个" } else { "至少有 $($newer.Count) 个" }
    $windows = @($newer | Where-Object { $_.Windows }).Count
    "WARN:上游 main $($count)新提交未合并(其中 $windows 个标题提及 Windows),引擎停在 main@$merged"
    $newer | Select-Object -First 10 | ForEach-Object { $_.Line }
    'INFO:合并方法见 desktop\README.md 的「跟进上游」'
}

if ($MyInvocation.InvocationName -eq '.') { return }

$command = if ($args.Count -gt 0) { [string]$args[0] } else { '' }
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 0
$summary = ''
try {
    switch ($command) {
        'checkdetail' {
            $r = @(Invoke-CheckDetail)
            $r | Out-File -Encoding utf8 "$BASE\last-check.txt"
            $summary = (($r | ForEach-Object {
                if ($_ -match '^([^=]+)=(ok|warn|fail|skip)(?::.*)?$') { "$($Matches[1])=$($Matches[2])" }
            }) -join ' ')
            $r
        }
        'check'        { Invoke-Check }
        'status'       { Invoke-Status }
        'fix' {
            $r = @(Invoke-Fix)
            $r | Out-File -Encoding utf8 "$BASE\last-fix.txt"
            $r
        }
        'switch'       { Invoke-Switch $args[1] }
        'set-region'   { Invoke-SetRegion }
        'set-client'   { Set-ClientMode $args[1] }
        'get-client'   { Get-Client }
        'get-flclash-controller' { Get-FlClashControllerSetting }
        'set-flclash-controller' { Set-FlClashControllerSetting $args[1] }
        'fingerprint'  { Get-Fingerprint }
        'cli-info'     { Get-CliInfo }
        'open-safe'    { Open-SafeUrl $args[1] }
        'snapshot-export' { Export-EncryptedSnapshot $args[1] $args[2] $args[3] }
        'snapshot-import' { Import-EncryptedSnapshot $args[1] $args[2] $args[3] }
        'diagnose-proxy' {
            $diagnosticArgs = if ($args.Count -gt 1) { @($args[1..($args.Count - 1)]) } else { @() }
            Invoke-ProxyDiagnostics $diagnosticArgs
        }
        'check-update' { Invoke-CheckUpdate }
        'update-now'   { Invoke-UpdateNow }
        'rules-status' { Invoke-RulesStatus }
        'launch' {
            $launchTarget = if ($args.Count -gt 1) { [string]$args[1] } else { 'desktop' }
            $launchExtra  = if ($args.Count -gt 2) { @($args[2..($args.Count - 1)]) } else { @() }
            Invoke-SafeLaunch $launchTarget $launchExtra
        }
        'shield-restore' { Invoke-ShieldRestore }
        'upstream-status' { Invoke-UpstreamStatus }
        'uistate' {
            $fresh = ($args.Count -gt 1 -and [string]$args[1] -eq '--fresh')
            Invoke-UiState $fresh
        }
        'get-check-config' {
            $config = Get-CheckConfig
            [pscustomobject]@{ disabled = @($config.disabled); allowOptionalFailures = [bool]$config.allowOptionalFailures; logging = [bool]$config.logging } | ConvertTo-Json -Depth 3 -Compress
        }
        'set-check-config' {
            # 标准输入按 UTF-8 读:无窗口子进程的控制台代码页(如 936)会把中文检查项名解成乱码
            $jsonArg = if ($args.Count -gt 1 -and [string]$args[1] -eq '--stdin') {
                (New-Object System.IO.StreamReader([Console]::OpenStandardInput(), (New-Object System.Text.UTF8Encoding($false)))).ReadToEnd()
            } else { [string]$args[1] }
            Set-CheckConfigFromJson $jsonArg
            'OK:检查设置已保存'
        }
        'export-diagnostics' { Export-Diagnostics $args[1] }
        default        {
            $exitCode = 1
            "用法: claude-switch.ps1 checkdetail|check|fix|launch [desktop|cli]|shield-restore|uistate [--fresh]|set-region|set-client|get-client|rules-status|get-check-config|set-check-config|fingerprint|open-safe|check-update|update-now|get-flclash-controller|set-flclash-controller"
        }
    }
} catch {
    $exitCode = 1
    Write-Error $_
} finally {
    $stopwatch.Stop()
    if ($command -ne 'diagnose-proxy') {
        Write-ClientLog $command $exitCode $stopwatch.ElapsedMilliseconds $summary
    }
}
exit $exitCode
