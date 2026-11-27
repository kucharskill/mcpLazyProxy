param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs/mcp-lazy-proxy'),
    [string]$ReleaseDirectory = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$lock = [IO.File]::Open((Join-Path $InstallDir '.mcp-lazy-proxy-install.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Write, [IO.FileShare]::None)
$id = [Guid]::NewGuid().ToString('N')
$incoming = Join-Path $InstallDir "mcp-lazy-proxy.new-$id.exe"
$target = Join-Path $InstallDir 'mcp-lazy-proxy.exe'
$backup = Join-Path $InstallDir "mcp-lazy-proxy.old-$id.exe"
try {
    if ($ReleaseDirectory) {
        Copy-Item -LiteralPath (Join-Path $ReleaseDirectory 'mcp-lazy-proxy.exe') -Destination $incoming
    } else {
        $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/kucharskill/mcpLazyProxy/releases/latest' -Headers @{ 'User-Agent' = 'mcp-lazy-proxy-installer' }
        $asset = @($release.assets | Where-Object name -EQ 'mcp-lazy-proxy.exe')
        if ($asset.Count -ne 1) { throw 'The latest release has no unique executable asset.' }
        Invoke-WebRequest -UseBasicParsing -Uri $asset[0].browser_download_url -OutFile $incoming
    }
    if (Test-Path -LiteralPath $target) {
        try { [IO.File]::Move($target, $backup) }
        catch { throw 'Windows blocked renaming the installed executable. Close the application holding it and retry. No process was terminated.' }
    }
    try { [IO.File]::Move($incoming, $target) }
    catch {
        if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $target)) { [IO.File]::Move($backup, $target) }
        throw
    }
    Write-Output "Installed: $target"
    Write-Output 'Existing sessions keep their current version. New sessions use the installed version.'
} finally {
    try { if (Test-Path -LiteralPath $incoming) { Remove-Item -LiteralPath $incoming -Force } }
    finally { $lock.Dispose() }
}
