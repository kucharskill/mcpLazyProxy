param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../.tmp/release'),
    [string]$ReleaseVersion = ''
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifest = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
$version = [regex]::Match($manifest, '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"$').Groups[1].Value
if (-not $version) { throw 'Expected a semantic version in Cargo.toml.' }
if ($ReleaseVersion) {
    if ($ReleaseVersion -notmatch ('^' + [regex]::Escape($version) + '-build\.[0-9]+$')) { throw 'Release version must match the package version plus -build.N.' }
    $version = $ReleaseVersion
}
$binary = Join-Path $root 'target/release/mcp-lazy-proxy.exe'
if (-not (Test-Path -LiteralPath $binary)) { throw 'Build the release binary before packaging.' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null
$archive = Join-Path $output "mcp-lazy-proxy-$version-windows-x64.zip"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$stream = [IO.File]::Open($archive, [IO.FileMode]::Create)
try {
    $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
    try {
        foreach ($name in @('mcp-lazy-proxy.exe', 'README.md', 'LICENSE')) {
            $source = if ($name -eq 'mcp-lazy-proxy.exe') { $binary } else { Join-Path $root $name }
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $source, $name, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally { $zip.Dispose() }
} finally { $stream.Dispose() }
$check = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    $actual = @($check.Entries | ForEach-Object FullName | Sort-Object)
    $expected = @('LICENSE', 'README.md', 'mcp-lazy-proxy.exe') | Sort-Object
    if (Compare-Object $actual $expected) { throw 'Archive allowlist mismatch.' }
} finally { $check.Dispose() }
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $output 'SHA256SUMS.txt'), "$hash  $([IO.Path]::GetFileName($archive))`n", [Text.UTF8Encoding]::new($false))
Write-Output $archive
