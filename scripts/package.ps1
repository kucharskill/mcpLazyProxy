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
$output = Join-Path ([IO.Path]::GetFullPath($OutputDirectory)) $version
New-Item -ItemType Directory -Force -Path $output | Out-Null
$asset = Join-Path $output 'mcp-lazy-proxy.exe'
Copy-Item -LiteralPath $binary -Destination $asset -Force
Write-Output $asset
