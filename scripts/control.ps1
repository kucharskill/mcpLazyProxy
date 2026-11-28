param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [ValidateSet('status', 'stop-backend', 'exit', 'fail', 'observe', 'hold', 'resume', 'sendToClient', 'sendToBackend')][string]$Command = 'status',
    [string]$Json,
    [ValidateSet('client-in', 'backend-in')][string]$Direction = 'client-in',
    [switch]$DisableObservation,
    [string]$CacheDirectory = (Join-Path ([IO.Path]::GetTempPath()) 'mcp-lazy-proxy')
)
$ErrorActionPreference = 'Stop'
$process = Get-Process -Id $ProcessId
if ($process.ProcessName -ne 'mcp-lazy-proxy') { throw 'The selected PID is not mcp-lazy-proxy.' }
$root = Join-Path $CacheDirectory 'diagnostics'
$candidates = @(Get-ChildItem -LiteralPath $root -Directory -Filter "$ProcessId-*" | Where-Object {
    $_.CreationTimeUtc -ge $process.StartTime.ToUniversalTime().AddSeconds(-2)
})
if ($candidates.Count -ne 1) { throw 'Expected one control directory for this running process. It may use an older binary or a different cache directory.' }
$directory = $candidates[0].FullName
$staged = Join-Path $directory ('command-' + [guid]::NewGuid().ToString('N') + '.part')
try {
    $request = @{ op = $Command }
    if ($Command -in @('sendToClient', 'sendToBackend')) {
        if (-not $Json) { throw 'Pass -Json with a JSON-RPC message.' }
        $request.message = $Json | ConvertFrom-Json
    }
    if ($Command -eq 'hold') { $request.direction = $Direction }
    if ($Command -eq 'observe') { $request.enabled = -not $DisableObservation }
    [IO.File]::WriteAllText($staged, ($request | ConvertTo-Json -Depth 100 -Compress))
    [IO.File]::Move($staged, (Join-Path $directory 'command'))
} finally {
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
}
Write-Output "Sent '$Command' to PID $ProcessId."
if ($Command -eq 'status') {
    $deadline = (Get-Date).AddSeconds(5)
    while ((Test-Path -LiteralPath (Join-Path $directory 'command')) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    Get-Content -LiteralPath (Join-Path $directory 'status.json') -Raw
}
