# mcp-lazy-proxy

Keep MCP tools available while releasing their backend processes between uses. This small native Rust proxy caches discovery responses, starts servers for live requests, and cleans up their process trees when work is finished.

## Why this exists

Opening chats, windows, and subagents can multiply MCP runtimes. Servers started to list their tools may then stay resident for the rest of the session. A separate cleanup problem appears when a launcher exits while its children keep running, consuming memory and sometimes holding ports or database locks.

These problems have been reported across popular agent applications:

| Application | Reported behavior | Source |
| --- | --- | --- |
| Codex | Duplicate MCP process trees accumulate across sessions; terminating a launcher misses descendants on Windows | [openai/codex #34614](https://github.com/openai/codex/issues/34614) |
| Codex | Local stdio MCP servers are repeatedly spawned and retained within one task | [openai/codex #38754](https://github.com/openai/codex/issues/38754) |
| Cursor | Reloading or restarting leaves orphan MCP processes, increasing memory use and keeping ports occupied on macOS | [Cursor community report](https://forum.cursor.com/t/mcp-process-leak-orphaned-children-on-restart/156478) |
| Claude Code | MCP children accumulate after session exit on Windows | [anthropics/claude-code #58918](https://github.com/anthropics/claude-code/issues/58918) |
| MCP TypeScript SDK | Closing stdio terminates the immediate child while descendants launched through tools such as npx or uvx survive | [typescript-sdk #2023](https://github.com/modelcontextprotocol/typescript-sdk/issues/2023) |

The reports describe particular versions and environments. They provide concrete examples of the two lifecycle problems this project addresses: **keeping unused backends resident** and **leaving descendants behind during shutdown**. Windows x64 is the tested release target; the Cursor report illustrates the same broader cleanup problem on macOS.

## What changes with the proxy

| Situation | Proxy behavior |
| --- | --- |
| A client asks for a catalog already fetched by another process | Serve the cached response from a shared directory in the user's temporary folder |
| A cold discovery request starts a backend | Fetch its response, cache successful metadata, and close the discovery-only backend as soon as outstanding requests finish |
| Client or server sends a ping | Answer locally, leaving the backend lifecycle unchanged |
| A tool is called | Start the backend, complete its handshake, and forward the live request |
| Work finishes | Keep the backend for the configured idle period, then close its process tree |
| Client exits or the proxy is killed | Clean up the owned backend tree using Windows Job Objects |
| A request stops responding | Apply an independent response deadline, return an error, and clean up the backend |

The client keeps its connection to the lightweight proxy while the heavier backend comes and goes. Each proxy instance owns its backend processes; discovery responses are shared through the disk cache. Concurrent cold misses may briefly start separate backends.

## Install and run

Install or update the latest release to `%LOCALAPPDATA%\Programs\mcp-lazy-proxy`:

```powershell
irm https://raw.githubusercontent.com/kucharskill/mcpLazyProxy/main/install.ps1 | iex
```

Or [download the Windows x64 executable](https://github.com/kucharskill/mcpLazyProxy/releases/latest/download/mcp-lazy-proxy.exe) directly. It is a standalone native binary; install the runtime required by your chosen backend, such as Node.js or Python, separately.

```powershell
.\mcp-lazy-proxy.exe -- node server.mjs
.\mcp-lazy-proxy.exe -- uvx example-mcp
.\mcp-lazy-proxy.exe --version
```

Put the proxy in the MCP client's `command` field and move the original command and arguments after `--`. Retain the server's environment and working directory:

```json
{
  "mcpServers": {
    "example": {
      "command": "C:/tools/mcp-lazy-proxy.exe",
      "args": ["--", "node", "C:/servers/example/server.mjs"]
    }
  }
}
```

For Windows `.cmd` launchers, use `cmd.exe /d /s /c` with quoting appropriate for the backend command.

The installer downloads the EXE and replaces the stable executable path. Running sessions keep their current image; later starts use the updated version. Retired files use `mcp-lazy-proxy.old-<32-hex-ID>.exe`, and startup silently removes those that Windows allows it to delete. Installation and cleanup coordinate through an exclusive file lock. If another application holds a file lock that blocks replacement, close that application and retry the installation.

### Automatic updates on Windows

Each proxy start makes one background update attempt. An installation younger than 10 minutes is kept as-is. A timestamp shared by the installed copies also spaces attempts at least 10 minutes apart across process starts. A long-running proxy performs this check once, at startup.

The updater uses Windows WinHTTP to query the latest GitHub Release and downloads `mcp-lazy-proxy.exe` when the asset SHA-256 differs from the installed file. GitHub supplies the digest in release asset metadata. Downloads go to a temporary filename beside the installed EXE, followed by the same rename-and-replace procedure used by the installer. Running sessions continue on their existing image; new sessions use the installed update.

The installer, startup cleanup, and updater share an exclusive file lock. A process that finds the lock busy immediately continues serving MCP. Update errors are silent. The adjacent JSON contains only optional settings. The EXE modification time provides the ten-minute installation-age gate; the lock file modification time records the last GitHub check attempt. Cleanup and ordinary lock acquisition preserve that timestamp. Successful replacements set a fresh EXE modification time.

## Cache and lifetime

Successful `initialize`, `server/discover`, and `tools/list` results are cached for **10 minutes**. The cache preserves their full results, including schemas, extension fields, numeric precision, and server metadata such as `ttlMs`. The local cache TTL is configured independently. Tool execution requests go to the live backend.

Cache identity includes the backend command, arguments, working directory, sorted environment, request parameters, and initialization parameters. Session noise is filtered case-insensitively: `PATH`, `CODEX_TOOL_CALL_ID`, `INIT_*`, and `NPM_*`. Cache filenames use hashes of the launch context; files contain the metadata responses. Clear the cache when a PATH change resolves the same command to a different executable. A server's `notifications/tools/list_changed` invalidates cached catalog pages.

**Discovery-only backends close immediately after their outstanding requests finish.** This applies with any cache TTL and also with `--idle-ms 0`. Separate cold legacy initialization and tool-list requests may each start a backend; the proxy replays the handshake as needed.

After a real client operation, the default idle allowance is **30 minutes**, renewed when the request is sent and when its response arrives. Discovery, pings, and background server callbacks leave that deadline unchanged. Pending requests in either direction protect the process until their responses or deadlines arrive.

Shutdown closes backend stdin, allows a grace period, and then terminates the process tree. On Windows, the backend joins a Job Object before it starts executing. Closing the last job handle cleans up its descendants, including after abrupt proxy termination. A handle to the parent process also detects client death when inherited pipes remain open elsewhere.

Each request has a separate **1-hour response timeout**. A timeout ends the backend and returns error `-32000` to outstanding client requests, including concurrent operations. Their effects may already have happened, so the client receives the error and decides how to proceed. A later request can start a fresh backend.

## Options

| Option | Default | Purpose |
| --- | --- | --- |
| `--cache-dir DIR` | `<OS temp>/mcp-lazy-proxy` | Discovery cache location; Windows uses the user's `%TEMP%` |
| `--cache-ttl-ms N` | `600000` | Metadata lifetime from write time; `0` requests fresh metadata each time |
| `--idle-ms N` | `1800000` | Backend idle allowance after real operations; `0` retains it until the client exits or a failure occurs |
| `--shutdown-ms N` | `2000` | Graceful shutdown allowance before forced tree cleanup |
| `--response-timeout-ms N` | `3600000` | Independent deadline for each forwarded request |

Durations are milliseconds, up to 86400000. Cache TTL and idle accept zero. Initialization and writes to backend stdin have 30-second limits. Response deadlines are checked approximately every 50 ms, with process cleanup time added before errors are returned. Proxy diagnostics use stderr; backend stderr is inherited.

### Choosing an idle period for stateful tools

Application state held in backend RAM lasts for that process's lifetime. For an editor or another workflow that relies on an in-memory session, choose a retention period that fits the work and save persistent state before it expires. For example, `--idle-ms 10800000` keeps the backend for three hours after the last real response. Use `--idle-ms 0` when the session should last until the client disconnects. Discovery-only processes still close immediately.

For process-bound identifiers inside a handshake or tool catalog, use `--cache-ttl-ms 0` to retrieve fresh metadata. Bridges to a separate desktop application should be checked for successful reconnection to that application's existing state.

## Build and test

The tested setup is Windows x64, a stable Rust toolchain with its native linker, rustfmt, Clippy, and Node.js 24 for the integration suite. The transport uses JSON-RPC 2.0 over JSON Lines. The Unix process-group implementation is experimental.

```powershell
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
$env:LAZY_BINARY = "$PWD/target/release/mcp-lazy-proxy.exe"
node --test --test-concurrency=1 tests/proxy.test.mjs
$asset = ./scripts/package.ps1
./tests/install.test.ps1 -ReleaseDirectory (Split-Path $asset)
```

The controlled tests cover cache reuse across replicas, cold discovery cleanup, catalog invalidation, idle deadlines, overlapping requests, cancellation, server callbacks, response timeouts, reconnects, hot executable replacement, parent death, and process-tree cleanup after forced termination. Local integration checks have also exercised discovery against eM Client, AniCraft, EffectCraft, PhotoCraft, and WordCraft.

## Releases and implementation

Every push to `main` runs Windows checks, tests the staged executable from a separate directory, and publishes it automatically to [GitHub Releases](https://github.com/kucharskill/mcpLazyProxy/releases). Release names combine the Cargo version with the workflow run number, such as `v0.1.4-build.5`, and identify the tested commit. The installer always selects the latest published release. Documentation and the MIT license live in this repository.

Rust and Tokio handle the proxy and asynchronous I/O. `process-wrap` supplies process-tree management, and `serde_json` preserves extensible JSON messages. Design references include [tkukushkin/lazy-mcp](https://github.com/tkukushkin/lazy-mcp), [xz-dev/dsh-lazy-mcp](https://github.com/xz-dev/dsh-lazy-mcp), and [sleticalboy/lazy-mcp-wrapper](https://github.com/sleticalboy/lazy-mcp-wrapper). Process management references: [process-wrap](https://docs.rs/process-wrap/latest/process_wrap/) and [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).


## Process control diagnostics

Diagnostics are disabled by default. Set `"debug": true` in the
`mcp-lazy-proxy.json` beside the EXE to enable them on subsequent process starts.
Set it to `false` to disable them again. Isolated test processes can override this
with `MCP_LAZY_DIAGNOSTICS=1` (enable) or `0` (disable).

An enabled proxy publishes a small control directory under its cache directory:
`OS_TEMP/mcp-lazy-proxy/diagnostics/<pid>-<start-token>/`. The separate diagnostics
module checks for a command once per second using the existing event loop, with
no extra thread or network listener. Startup tokens distinguish reused PIDs.
The files contain process status and the version, without arguments or environment data.
The control directory is removed on normal exit. Access follows the cache directory's
filesystem permissions; keep that directory private to your user account.

Use the repository's PowerShell helper to select one running proxy:

```powershell
./scripts/control.ps1 -ProcessId 12345 -Command status
./scripts/control.ps1 -ProcessId 12345 -Command stop-backend
./scripts/control.ps1 -ProcessId 12345 -Command exit
./scripts/control.ps1 -ProcessId 12345 -Command fail
```

`stop-backend` releases the backend while preserving the client transport.
`exit` closes the proxy with code 0; `fail` closes it with code 1 to test client
recovery. Lifecycle commands wait for outstanding MCP requests to finish. Status
is sampled when requested. Pass `-CacheDirectory` when the proxy uses a custom
cache path. Newly started proxies expose control when debug is enabled; the client
configuration stays unchanged.


For local development, add an absolute `updatePath` to the installed
`mcp-lazy-proxy.json`, pointing to a locally built EXE. Startup updates then
use that file exclusively. A different source SHA-256 triggers a copy
through the same exclusive lock and image replacement mechanism. Local sources are checked at every process startup; the ten-minute
installation-age and attempt cooldown apply only to GitHub. A missing or invalid local
source leaves the installed image in place. Remove `updatePath` to resume GitHub
Release updates. Keep the source and installed executable at separate paths.

GitHub updates compare the release asset's built-in `digest` with the installed
EXE's SHA-256. Local updates compute the same digest from `updatePath`. Identical
files are skipped regardless of version labels. The downloaded or staged file
must match the selected digest before replacement.


The diagnostics bridge accepts JSON commands through the same per-process inbox:

```powershell
./scripts/control.ps1 -ProcessId 12345 -Command observe
./scripts/control.ps1 -ProcessId 12345 -Command sendToClient -Json '{"jsonrpc":"2.0","id":1,"method":"roots/list"}'
./scripts/control.ps1 -ProcessId 12345 -Command sendToBackend -Json '{"jsonrpc":"2.0","id":1,"method":"ping"}'
./scripts/control.ps1 -ProcessId 12345 -Command hold -Direction backend-in
./scripts/control.ps1 -ProcessId 12345 -Command resume
```

Observation records `client-in`, `client-out`, `backend-in`, `backend-out` and
injection reports in `events.jsonl`, capped at approximately 4 MiB. It starts only
when requested and can be disabled with `observe -DisableObservation`. Captured
messages can contain private data; the capture stays in the local control directory
after exit for analysis. Each injection also writes `last-send.json` with its actual
wire message and any send error. Injected requests receive unique IDs; their replies
are captured and consumed by diagnostics instead of entering normal proxy routing.
Supported requests depend on the peer's negotiated MCP capabilities. Sending to a
backend requires an already running backend.

`hold` buffers one incoming direction (`client-in` or `backend-in`); `resume` replays
the buffered messages in arrival order. The limit is 256 held messages; exceeding
it ends the test connection. Normal response timeouts continue during a hold.
All experiment logic and the proxy extension methods live in `src/diagnostics.rs`;
`scripts/control.ps1` is the external driver. The main proxy contains only routing
hooks, the optional control field and lifecycle dispatch.

The settings filename is derived from the running executable by replacing its
extension with `.json`: `mcp-lazy-proxy.exe` reads `mcp-lazy-proxy.json`.
The optional `debug` field enables diagnostics only when it is `true`.
Update attempts leave the JSON unchanged.
The installer preserves `debug` and `updatePath` when replacing the executable.
