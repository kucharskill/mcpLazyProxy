# mcp-lazy-proxy

A small native stdio MCP proxy with discovery caching, lazy backend startup, and idle process-tree cleanup. One executable, no daemon or HTTP service.

## Install and run

Install or update to the latest GitHub Release in `%LOCALAPPDATA%\Programs\mcp-lazy-proxy`:

```powershell
irm https://raw.githubusercontent.com/kucharskill/mcpLazyProxy/main/install.ps1 | iex
```

The installer verifies SHA-256 and the ZIP allowlist, then installs the EXE without running it. It does not change PATH or MCP configuration and does not require administrator privileges. Updates rename the previous image to `mcp-lazy-proxy.old-<32-hex-ID>.exe` before replacing the stable executable path. Existing processes keep running their old image; new processes use the updated file. Each proxy startup silently removes unused backups matching that exact pattern in its own directory. Cleanup errors are ignored; locked backups are retained for a later startup without logging or blocking the proxy. Install/update and cleanup share a local file lock so cleanup cannot remove an update's rollback copy. If another application denies renaming the target, installation fails without terminating processes or replacing the target. Close the application holding the file and retry.

Download the Windows x64 ZIP from [GitHub Releases](https://github.com/kucharskill/mcpLazyProxy/releases), verify its checksum against `SHA256SUMS.txt`, and extract it. The proxy needs no Rust, Node.js, or .NET runtime. Your chosen backend still needs its own runtime and dependencies. The Windows executable is unsigned.

```powershell
.\mcp-lazy-proxy.exe -- node server.mjs
.\mcp-lazy-proxy.exe -- uvx example-mcp
```

For an MCP client, replace the original command with the proxy executable and put the original command and arguments after `--`. Keep the original environment and working directory. Example configuration:

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

On Windows, invoke `.cmd` launchers explicitly through `cmd.exe /d /s /c` with CMD quoting appropriate for the backend. The proxy forwards an argument vector; it does not assemble a shell script. It makes no external network requests itself. Backend services may have their own network access or costs.

## Behavior

- Answer `ping` locally in either direction without starting or keeping the backend alive.
- Cache successful `initialize`, `server/discover`, and `tools/list` results for 10 minutes. Use the cache while the backend is stopped; a cold cache requires a backend start.
- Preserve the entire result, including unknown fields, schemas, numeric precision, and original metadata such as `ttlMs`. The local cache TTL is a separate policy. Do not cache errors or tool execution results.
- Key cache entries by command, arguments, working directory, sorted environment, request parameters, and initialization parameters. Exclude `PATH`, `CODEX_TOOL_CALL_ID`, `INIT_*`, and `NPM_*`, case-insensitively. Raw environment values and launch arguments are not written to disk. Because PATH is excluded, clear the cache when executable resolution changes.
- Write cache entries atomically. Concurrent cold starts may temporarily run separate backends. There is no global lock or shared backend transport.
- Invalidate cached tool-list pages on `notifications/tools/list_changed`. Otherwise, a stopped backend's catalog may remain stale until the cache expires.
- Replay legacy `initialize` and `notifications/initialized` before forwarding operations to a restarted backend. Forward and cache `server/discover` as received rather than synthesizing it from a legacy handshake.
- Maintain one idle deadline: `expires = max(expires, now + duration_for_method)`. Discovery requests grant 1 minute; other requests grant 30 minutes. Sending and completing a request update the deadline. Cache hits and pings do not. Pending requests in either direction prevent idle shutdown.
- Close backend stdin on shutdown, allow a grace period, then terminate the owned process tree. Client EOF or a termination signal also shuts down the proxy.
- On Windows, start the backend suspended and assign it to a Job Object before resuming it. Kill-on-close covers the tree even if the proxy is forcibly terminated. Failure to establish containment fails startup. A parent process handle detects parent death even if another process keeps stdin open. The proxy does not create its own console window.
- On Unix, use a process group and SIGINT/SIGTERM handling. A hard SIGKILL of the proxy does not provide the same descendant-cleanup guarantee as Windows Job Objects. Unix support has not yet been validated.
- If the backend fails, return errors for outstanding requests without automatically retrying operations of unknown outcome. A later explicit request can start a new backend.

## Options

| Option | Default | Purpose |
| --- | --- | --- |
| `--cache-dir DIR` | `<OS temp>/mcp-lazy-proxy` | Windows: `%TEMP%\mcp-lazy-proxy`; results may contain server instructions or tool descriptions |
| `--cache-ttl-ms N` | `600000` | TTL from write time; `0` disables caching |
| `--idle-ms N` | `1800000` | Idle allowance after operations; `0` disables all automatic idle shutdown |
| `--discovery-idle-ms N` | `60000` | Idle allowance after discovery requests |
| `--shutdown-ms N` | `2000` | Graceful shutdown allowance |
| `--response-timeout-ms N` | `3600000` | Independent response deadline, default 1 hour |

Durations are milliseconds, at most 86400000. Only cache TTL and idle may be zero. Backend initialization and writes to its stdin have 30-second limits.

Each forwarded request has its own response deadline, including server requests to the client. Other responses, pings, and notifications do not renew it. It applies even with idle shutdown disabled. If a deadline expires, stop the entire backend and return error `-32000` to outstanding client requests. This also interrupts concurrent operations, whose outcomes may be unknown. The proxy stays available and does not retry automatically. Deadlines are checked approximately every 50 ms; process cleanup adds the shutdown grace period. Requests queued during handshake use its separate initialization deadline.

Up to 256 client requests may be outstanding. Proxy diagnostics go to stderr and contain event types, methods, and PIDs, without parameters. Backend stderr is inherited unchanged. Cache files and test reports are local outputs, not release assets.

## State and compatibility

Restarting a backend replays the handshake, **not application state**. Use `--idle-ms 0` for backends holding unsaved documents, active subscriptions, tasks that outlive their responses, or process-bound session identifiers. If the handshake or catalog itself contains transient session data, also use `--cache-ttl-ms 0`.

This matters for headless document editors: discovery tests do not establish that an edited document survives backend shutdown. Bridges to an independently running application need their own reconnection tests. Cleanup targets only the proxy's owned process tree; it does not search for applications by name.

The initial scope is JSON-RPC 2.0 over JSON Lines, without batches or legacy Content-Length framing. Protocol extensions and client-specific session contexts need compatibility tests.

## Build and test

Build prerequisites: a stable Rust toolchain with the native platform linker, rustfmt, and Clippy. The integration tests use Node.js 24 and no npm dependencies.

```powershell
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
$env:LAZY_BINARY = "$PWD/target/release/mcp-lazy-proxy.exe"
node --test --test-concurrency=1 tests/proxy.test.mjs
./scripts/package.ps1
./tests/install.test.ps1 -ReleaseDirectory .tmp/release
```

Tests use controlled process trees in `.tmp/tests`. They cover cross-process cache reuse, TTL, environment filtering, catalog pagination and invalidation, long and concurrent requests, server requests, cancellation, independent response deadlines, reconnects, backend crashes, proxy hard termination, and parent death while stdin remains open.

Optional Windows compatibility checks use existing local MCP launch configurations for eM Client, AniCraft, and WordCraft:

```powershell
uv run --no-project --with psutil tests/real.py
```

This test reads the MCP client configuration, performs handshake and tool discovery, compares cold and warm caches, measures memory, and sends an intentionally unknown method to test wake-up and a second idle cycle. It does not change configuration, messages, or documents. Each run creates a fresh cache and writes `.tmp/real-smoke.json`. These local integrations are not required for CI.

## Releases

Every push to main runs Windows checks, tests an extracted package, and automatically publishes its executable ZIP and SHA-256 checksum to GitHub Releases. No manual tag or release step is needed. Releases use the Cargo version plus the workflow run number, for example `v0.1.0-build.1`, and point to the exact tested commit. Pull requests run the same checks without publishing. The ZIP contains only `mcp-lazy-proxy.exe`, this README, and the MIT license. Compiled executables are distributed through releases rather than committed to source control.

## Implementation and references

Four Rust modules: `main.rs` handles routing and session lifecycle, `backend.rs` manages processes, `cache.rs` handles disk caching, and `config.rs` handles CLI options and launch identity. Tokio provides asynchronous I/O, `process-wrap` provides OS process-tree management, and `serde_json` preserves extensible JSON messages. A full typed MCP SDK is not needed for this forwarding scope.

Design references: [tkukushkin/lazy-mcp](https://github.com/tkukushkin/lazy-mcp), [xz-dev/dsh-lazy-mcp](https://github.com/xz-dev/dsh-lazy-mcp), and [sleticalboy/lazy-mcp-wrapper](https://github.com/sleticalboy/lazy-mcp-wrapper). This implementation does not copy their source. Process management references: [process-wrap](https://docs.rs/process-wrap/latest/process_wrap/) and [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).
