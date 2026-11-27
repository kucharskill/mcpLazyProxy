"""Read-only smoke test using existing Codex MCP launch configurations.

No config modifications, email access, document edits or raw secret output.
Run: uv run --no-project --with psutil tests/real.py [binary]
"""
import json
import os
import queue
import subprocess
import sys
import threading
import tempfile
import time
import tomllib
from pathlib import Path
import psutil

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / "target/release/mcp-lazy-proxy.exe"
CONFIG = tomllib.loads((Path.home() / ".codex/config.toml").read_text(encoding="utf-8"))
REPORT = []
CACHE_ROOT = Path(tempfile.mkdtemp(prefix="real-cache-", dir=ROOT / ".tmp"))

class Client:
    def __init__(self, name, backend):
        self.seq = 0
        self.queue = queue.Queue()
        self.log = []
        self.process = subprocess.Popen([
            str(BINARY), "--cache-dir", str(CACHE_ROOT / name),
            "--discovery-idle-ms", "400", "--idle-ms", "600", "--shutdown-ms", "500",
            "--", backend["command"], *backend.get("args", [])],
            cwd=backend.get("cwd", ROOT), env={**os.environ, **backend.get("env", {})},
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        threading.Thread(target=self.read, daemon=True).start()
        threading.Thread(target=lambda: self.log.extend(self.process.stderr), daemon=True).start()

    def read(self):
        for line in self.process.stdout:
            value=json.loads(line)
            if "method" in value and "id" in value:
                if value["method"]=="ping":self.send({"id":value["id"],"result":{}})
                else:self.send({"id":value["id"],"error":{"code":-32601,"message":"Unsupported by smoke test client"}})
            else:self.queue.put(value)
        self.queue.put(None)

    def send(self, value):
        self.process.stdin.write(json.dumps({"jsonrpc":"2.0", **value}) + "\n")
        self.process.stdin.flush()

    def request(self, method, params):
        self.seq += 1
        self.send({"id":self.seq, "method":method, "params":params})
        deadline = time.monotonic()+45
        while True:
            value=self.queue.get(timeout=max(0.01,deadline-time.monotonic()))
            if value is None:
                raise RuntimeError("transport closed")
            if value.get("id")==self.seq and "method" not in value:
                if "error" in value: raise RuntimeError(f"{method} error code {value['error']['code']}")
                return value["result"]
            if "id" in value and "method" in value:
                self.send({"id":value["id"],"error":{"code":-32601,"message":"Smoke test client has no extra capabilities"}})

    def close(self):
        self.process.stdin.close()
        try:self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill();self.process.wait(timeout=5)
        assert self.process.returncode==0, "proxy exited with an error"

def alive(identity):
    pid,created=identity
    try:return psutil.Process(pid).create_time()==created
    except psutil.NoSuchProcess:return False

for name in ["emclient", "anicraft", "wordcraft"]:
    backend=CONFIG["mcp_servers"][name]
    expected=None
    for phase in ["cold", "warm"]:
        client=Client(name,backend)
        try:
            t0=time.perf_counter()
            initialized=client.request("initialize", {"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mcp-lazy-smoke","version":"1"}})
            client.send({"method":"notifications/initialized"})
            tools=client.request("tools/list", {})
            elapsed=(time.perf_counter()-t0)*1000
            if expected is None:expected=(initialized,tools)
            else:assert expected==(initialized,tools), "warm result differs"
            proxy=psutil.Process(client.process.pid)
            children=[(p.pid,p.create_time()) for p in proxy.children(recursive=True)]
            time.sleep(1.3)
            remaining=[{"pid":pid,"name":psutil.Process(pid).name(),"created":created} for pid,created in children if alive((pid,created))]
            assert not remaining, f"backend descendants survived idle timeout: {remaining}; proxy log: {[s.strip() for s in client.log if s.startswith('mcp-lazy:')]}"
            memory=proxy.memory_info()
            REPORT.append({"server":name,"phase":phase,"tools":len(tools["tools"]),"discoveryMs":round(elapsed,1),
                "backendStarts":sum("backend started" in s for s in client.log),"descendantsObserved":len(children),
                "descendantsAfterIdle":len(proxy.children(recursive=True)),
                "proxyWorkingSetMiB":round(memory.rss/1048576,2),"proxyPrivateMiB":round(memory.private/1048576,2)})
            # A deliberately unknown method tests wake/reinitialization without business effects.
            try:client.request("lazy-proxy-test/unknown", {})
            except RuntimeError as error:assert str(error).endswith("error code -32601"), str(error)
            else:raise AssertionError("backend accepted unknown test method")
            time.sleep(1.3)
            assert not proxy.children(recursive=True), "backend survived second idle cycle"
            REPORT[-1]["wakeAndSecondIdlePassed"] = True
            print(json.dumps(REPORT[-1]),flush=True)
        finally:client.close()

(ROOT/".tmp/real-smoke.json").write_text(json.dumps(REPORT,indent=2),encoding="utf-8")
