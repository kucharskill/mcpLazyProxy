param([Parameter(Mandatory=$true)][string]$ReleaseDirectory)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$installer = Join-Path $root 'install.ps1'
$testRoot = Join-Path $root ('.tmp/installer-test-' + [Guid]::NewGuid().ToString('N'))
$installDir = Join-Path $testRoot 'installed'
$tempDir = Join-Path $testRoot 'temp'
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
$oldTemp = $env:TEMP
$oldTmp = $env:TMP
try {
    $env:TEMP = $tempDir
    $env:TMP = $tempDir
    & $installer -InstallDir $installDir -ReleaseDirectory $ReleaseDirectory | Out-Null
    $target = Join-Path $installDir 'mcp-lazy-proxy.exe'
    if (-not (Test-Path -LiteralPath $target)) { throw 'Initial installation did not create EXE.' }
    $state = Get-Content -LiteralPath (Join-Path $installDir 'installed-release.json') -Raw | ConvertFrom-Json
    if (([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - $state.installedAtUnix) -gt 60) { throw 'Installer did not record a fresh installation time.' }
    $expected = (Get-FileHash -LiteralPath $target).Hash
    & $installer -InstallDir $installDir -ReleaseDirectory $ReleaseDirectory | Out-Null
    if ((Get-FileHash -LiteralPath $target).Hash -ne $expected) { throw 'Update changed executable contents.' }
    $backups = @(Get-ChildItem -LiteralPath $installDir -Filter 'mcp-lazy-proxy.old-*.exe')
    if ($backups.Count -ne 1) { throw 'Update did not create an identifiable backup.' }
    $held = [IO.File]::Open($target, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $failed = $false
        try { & $installer -InstallDir $installDir -ReleaseDirectory $ReleaseDirectory | Out-Null } catch { $failed = $true }
        if (-not $failed) { throw 'Expected a sharing violation for the deliberate file lock.' }
    } finally { $held.Dispose() }
    if ((Get-FileHash -LiteralPath $target).Hash -ne $expected) { throw 'Blocked update damaged the installed EXE.' }
    $bad = Join-Path $testRoot 'bad'
    New-Item -ItemType Directory -Path $bad | Out-Null
    $failed = $false
    try { & $installer -InstallDir $installDir -ReleaseDirectory $bad | Out-Null } catch { $failed = $true }
    if (-not $failed) { throw 'Missing release executable was accepted.' }
    if ((Get-FileHash -LiteralPath $target).Hash -ne $expected) { throw 'Missing release changed the installed EXE.' }
    if (@(Get-ChildItem -LiteralPath $installDir -Filter 'mcp-lazy-proxy.new-*.exe').Count -ne 0) { throw 'Incoming EXE was not cleaned after failure.' }
    if (@(Get-ChildItem -LiteralPath $tempDir).Count -ne 0) { throw 'Installer staging files remain.' }
    Write-Output 'Installer tests passed: install, update, locked target, missing download, cleanup.'
} finally {
    $env:TEMP = $oldTemp
    $env:TMP = $oldTmp
}
