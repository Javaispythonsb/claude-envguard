# 依次在独立的 Windows PowerShell 进程中运行 tests\windows-*.ps1；任一失败则以退出码 1 结束。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\run-windows-tests.ps1

$ErrorActionPreference = 'Continue'
$failed = @()
foreach ($test in Get-ChildItem -LiteralPath $PSScriptRoot -Filter 'windows-*.ps1' | Sort-Object Name) {
    $lines = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $test.FullName 2>&1 | ForEach-Object { "$_" })
    if ($LASTEXITCODE -eq 0) {
        "PASS  $($test.Name)"
        continue
    }
    "FAIL  $($test.Name)"
    @($lines | Where-Object { $_ -match '^FAIL ' }) + @($lines | Select-Object -Last 3) | ForEach-Object { "      $_" }
    $failed += $test.Name
}

if ($failed.Count -gt 0) {
    "FAILED: $($failed.Count) 个测试文件"
    exit 1
}
'OK: 全部测试文件通过'
