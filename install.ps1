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
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$temporary = Join-Path $tempRoot "mcp-lazy-proxy-install-$id"
$incoming = Join-Path $InstallDir "mcp-lazy-proxy.new-$id.exe"
$target = Join-Path $InstallDir 'mcp-lazy-proxy.exe'
$backup = Join-Path $InstallDir "mcp-lazy-proxy.old-$id.exe"
try {
    New-Item -ItemType Directory -Path $temporary | Out-Null
    if ($ReleaseDirectory) {
        $checksumFile = Join-Path ([IO.Path]::GetFullPath($ReleaseDirectory)) 'SHA256SUMS.txt'
    } else {
        $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/kucharskill/mcpLazyProxy/releases/latest' -Headers @{ 'User-Agent' = 'mcp-lazy-proxy-installer' }
        $checksums = @($release.assets | Where-Object name -EQ 'SHA256SUMS.txt')
        if ($checksums.Count -ne 1) { throw 'The latest release has no unique checksum asset.' }
        $checksumFile = Join-Path $temporary 'SHA256SUMS.txt'
        Invoke-WebRequest -UseBasicParsing -Uri $checksums[0].browser_download_url -OutFile $checksumFile
    }
    $checksum = (Get-Content -LiteralPath $checksumFile -Raw).Trim()
    if ($checksum -notmatch '^([a-fA-F0-9]{64})\s+(mcp-lazy-proxy-[0-9]+\.[0-9]+\.[0-9]+(?:-build\.[0-9]+)?-windows-x64\.zip)$') { throw 'Invalid release checksum manifest.' }
    $expectedHash = $Matches[1]
    $archiveName = $Matches[2]
    $archive = Join-Path $temporary $archiveName
    if ($ReleaseDirectory) {
        Copy-Item -LiteralPath (Join-Path $ReleaseDirectory $archiveName) -Destination $archive
    } else {
        $asset = @($release.assets | Where-Object name -EQ $archiveName)
        if ($asset.Count -ne 1) { throw 'The latest release has no matching Windows x64 archive.' }
        Invoke-WebRequest -UseBasicParsing -Uri $asset[0].browser_download_url -OutFile $archive
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expectedHash) { throw 'Release checksum mismatch; installation was not changed.' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $names = @($zip.Entries | ForEach-Object FullName | Sort-Object)
        if (($names.Count -ne 3) -or (Compare-Object $names @('LICENSE', 'README.md', 'mcp-lazy-proxy.exe'))) { throw 'Unexpected release archive contents.' }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry('mcp-lazy-proxy.exe'), $incoming, $false)
    } finally { $zip.Dispose() }
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
    if (Test-Path -LiteralPath $incoming) { Remove-Item -LiteralPath $incoming -Force }
    $lock.Dispose()
    $resolved = [IO.Path]::GetFullPath($temporary)
    $boundary = $tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if ($resolved.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase) -and ([IO.Path]::GetFileName($resolved) -eq "mcp-lazy-proxy-install-$id") -and (Test-Path -LiteralPath $resolved)) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
