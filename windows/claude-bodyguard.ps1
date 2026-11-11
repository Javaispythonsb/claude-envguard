# Claude 保镖 (Windows) —— 代理离线时自动关闭 Claude
# 由安装器注册为计划任务,登录时启动,隐藏窗口常驻
$ErrorActionPreference = 'SilentlyContinue'
$BASE = Join-Path $env:USERPROFILE ".claude-switcher"
$LOG = Join-Path $BASE "bodyguard.log"
$HEARTBEAT = Join-Path $BASE "bodyguard-heartbeat.txt"
New-Item -ItemType Directory -Force -Path $BASE | Out-Null
$MY_SESSION = try { (Get-Process -Id $PID -ErrorAction Stop).SessionId } catch { -1 }

# 代理模式:优先读脚本同目录,其次读安装器写入的 proxy-mode.txt;缺失时按旧版 Clash Verge 行为
$modeFile = if ($PSScriptRoot) { Join-Path $PSScriptRoot "proxy-mode.txt" } else { '' }
if (-not $modeFile -or -not (Test-Path $modeFile)) { $modeFile = Join-Path $BASE "proxy-mode.txt" }
$PROXY_MODE = if (Test-Path $modeFile) { Get-Content $modeFile -Raw } else { '' }
if ($PROXY_MODE) { $PROXY_MODE = $PROXY_MODE.Trim() }
if (-not $PROXY_MODE) { $PROXY_MODE = 'clash-verge' }

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

# 客户端进程/服务在线判定:auto 与 other 只依赖 TUN,不要求特定进程(避免误杀)
function Test-ClientAlive {
    switch ($PROXY_MODE) {
        'clash-verge' { return [bool](Get-Process -Name 'verge-mihomo' -ErrorAction SilentlyContinue) }
        'flclash'     { return [bool](Get-Process -Name 'FlClashCore','FlClash' -ErrorAction SilentlyContinue) }
        'sing-box'    { return [bool](Get-Process -Name 'sing-box' -ErrorAction SilentlyContinue) }
        'clashx'      { return [bool](Get-Process -Name 'ClashX' -ErrorAction SilentlyContinue) }
        'surge'       { return [bool](Get-Process -Name 'Surge' -ErrorAction SilentlyContinue) }
        'wireguard'   { return [bool](Get-Service -Name 'WireGuardTunnel*' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running') }
        'auto'        { return $true }
        'other'       { return $true }
        default       { return [bool](Get-Process -Name 'verge-mihomo' -ErrorAction SilentlyContinue) }
    }
}

# 终止当前用户会话下的受保护进程(Claude 桌面版 + native claude.exe + npm/bun Claude Code CLI)
function Stop-ProtectedProcesses {
    $killedDesktopOrNative = $false
    $claudeProcs = @(Get-Process -Name 'Claude' -ErrorAction SilentlyContinue | Where-Object { $MY_SESSION -lt 0 -or $_.SessionId -eq $MY_SESSION })
    if ($claudeProcs.Count -gt 0) {
        $claudeProcs | Stop-Process -Force -ErrorAction SilentlyContinue
        $killedDesktopOrNative = $true
    }
    $killedScriptCli = $false
    $jsHosts = @(Get-Process -Name 'node','bun' -ErrorAction SilentlyContinue | Where-Object { $MY_SESSION -lt 0 -or $_.SessionId -eq $MY_SESSION })
    if ($jsHosts.Count -gt 0) {
        $hostIds = @($jsHosts | ForEach-Object { [int]$_.Id })
        $cliCim = @(Get-CimInstance Win32_Process -Filter "Name='node.exe' OR Name='bun.exe'" -ErrorAction SilentlyContinue | Where-Object {
            ($hostIds -contains [int]$_.ProcessId) -and ([string]$_.CommandLine -match '@anthropic-ai[\\/]+claude-code')
        })
        foreach ($proc in $cliCim) {
            Stop-Process -Id ([int]$proc.ProcessId) -Force -ErrorAction SilentlyContinue
            $killedScriptCli = $true
        }
    }
    return ($killedDesktopOrNative -or $killedScriptCli)
}

$offline = $false
while ($true) {
    try {
        [System.IO.File]::WriteAllText($HEARTBEAT, "$PID $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())", [System.Text.Encoding]::ASCII)
    } catch {}
    if (Test-Path $modeFile) {
        $currentMode = (Get-Content $modeFile -Raw).Trim()
        if ($currentMode) { $script:PROXY_MODE = $currentMode }
    }
    $tun = Get-TunStatus
    $clientUp = Test-ClientAlive
    $alive = $tun.up -and $clientUp

    if (-not $alive) {
        if (-not $offline) {
            $clientState = if ($PROXY_MODE -eq 'auto' -or $PROXY_MODE -eq 'other') { '跳过(仅TUN)' } elseif ($clientUp) { '在线' } else { '离线' }
            "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') 代理离线(模式=$PROXY_MODE, TUN=$($tun.detail), 客户端=$clientState)" | Out-File -Append -Encoding utf8 $LOG
            $offline = $true
        }
        if (Stop-ProtectedProcesses) {
            "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') 代理离线(内核或TUN丢失),已自动终止受保护的 Claude / CLI 进程" | Out-File -Append -Encoding utf8 $LOG
        }
    } else {
        $offline = $false
    }
    Start-Sleep -Seconds 10
}
