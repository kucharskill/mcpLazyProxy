#![cfg_attr(windows, windows_subsystem = "windows")]

mod backend;
mod cache;
mod config;
mod diagnostics;
mod install_cleanup;
mod logging;
#[cfg(windows)]
mod update;

use backend::{Backend, Event};
use cache::{Cache, cacheable, discovery};
use config::Config;
use serde_json::{Value, json};
use std::{collections::HashMap, io, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
    time::{Instant, interval},
};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

struct Pending {
    id: Value,
    method: String,
    cache_key: String,
    started: Instant,
}
struct Proxy {
    config: Config,
    cache: Cache,
    backend: Option<Backend>,
    operational: bool,
    events: mpsc::Sender<Event>,
    generation: u64,
    sequence: u64,
    initialize: Value,
    initialized: bool,
    handshake: Option<(String, Instant)>,
    queue: Vec<Value>,
    pending: HashMap<String, Pending>,
    server_pending: HashMap<String, Instant>,
    expires: Instant,
    output: tokio::io::Stdout,
    diagnostics: Option<diagnostics::Control>,
    log: Option<logging::Log>,
}
impl Proxy {
    async fn out(&mut self, message: &Value) -> io::Result<()> {
        self.diagnostic_observe("client-out", message);
        let mut data = serde_json::to_vec(message)?;
        data.push(b'\n');
        self.log_wire(
            "proxy-to-client",
            std::str::from_utf8(&data[..data.len() - 1]).unwrap_or(""),
        );
        self.output.write_all(&data).await?;
        self.output.flush().await
    }
    async fn error(&mut self, id: Value, code: i32, text: &str) -> io::Result<()> {
        self.out(&json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":text}}))
            .await
    }
    fn touch(&mut self, method: &str) {
        if !discovery(method) {
            self.expires = self.expires.max(Instant::now() + self.config.idle);
            self.log_deadline(method);
        }
    }
    async fn start(&mut self, replay: bool) -> io::Result<()> {
        self.operational = false;
        self.generation += 1;
        self.backend = Some(Backend::spawn(
            &self.config,
            self.generation,
            self.events.clone(),
        )?);
        self.log_event("backend_started", json!({"generation":self.generation,"backendPid":self.backend.as_ref().and_then(|b|b.child.id())}));
        self.expires = Instant::now();
        if replay && !self.initialize.is_null() {
            let id = format!("lazy-init-{}", self.generation);
            self.handshake = Some((id.clone(), Instant::now() + INITIALIZE_TIMEOUT));
            self.send(
                &json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":self.initialize}),
            )
            .await?;
        }
        Ok(())
    }
    async fn send(&mut self, message: &Value) -> io::Result<()> {
        self.diagnostic_observe("backend-out", message);
        self.log_wire("proxy-to-backend", &serde_json::to_string(message)?);
        self.backend
            .as_mut()
            .ok_or_else(|| io::Error::other("backend unavailable"))?
            .send(message)
            .await
    }
    async fn dispatch(&mut self, mut message: Value) -> io::Result<()> {
        let method = message["method"].as_str().unwrap_or("").to_owned();
        self.operational |= !discovery(&method);
        if let Some(id) = message.get("id").cloned() {
            self.sequence += 1;
            let wire = format!("lazy-request-{}", self.sequence);
            let cache_key = Cache::key(&method, &message["params"], &self.initialize);
            self.pending.insert(
                wire.clone(),
                Pending {
                    id,
                    method: method.clone(),
                    cache_key,
                    started: Instant::now(),
                },
            );
            message["id"] = json!(wire);
            self.touch(&method);
        }
        self.send(&message).await
    }
    async fn client(&mut self, mut message: Value) -> io::Result<()> {
        if self.diagnostic_input("client-in", &message)? {
            return Ok(());
        }
        let Some(method) = message["method"].as_str().map(str::to_owned) else {
            if let Some(id) = message.get("id") {
                self.server_pending.remove(&id.to_string());
                if self.backend.is_some() {
                    self.send(&message).await?;
                }
            }
            return Ok(());
        };
        let Some(id) = message.get("id").cloned() else {
            if method == "notifications/initialized" {
                self.initialized = true;
                if self.backend.is_some() && self.handshake.is_none() {
                    self.send(&message).await?;
                }
            } else if self.backend.is_some() {
                if method == "notifications/cancelled" {
                    if let Some(index) = self
                        .queue
                        .iter()
                        .position(|m| m["id"] == message["params"]["requestId"])
                    {
                        let queued = self.queue.remove(index);
                        return self
                            .error(
                                queued["id"].clone(),
                                -32800,
                                "Cancelled before backend was ready",
                            )
                            .await;
                    }
                    if let Some((wire, _)) = self
                        .pending
                        .iter()
                        .find(|(_, p)| p.id == message["params"]["requestId"])
                    {
                        message["params"]["requestId"] = json!(wire);
                    }
                }
                self.send(&message).await?;
            }
            return Ok(());
        };
        if method == "ping" {
            return self
                .out(&json!({"jsonrpc":"2.0","id":id,"result":{}}))
                .await;
        }
        if method == "initialize" {
            self.initialize = message["params"].clone();
        }
        let key = Cache::key(&method, &message["params"], &self.initialize);
        if self.backend.is_none() && cacheable(&method) {
            if let Some(result) = self.cache.get(&key) {
                eprintln!("mcp-lazy: cache hit {method}");
                self.log_event("cache_hit", json!({"method":method,"key":key}));
                return self
                    .out(&json!({"jsonrpc":"2.0","id":id,"result":result}))
                    .await;
            }
            eprintln!("mcp-lazy: cache miss {method}");
            self.log_event("cache_miss", json!({"method":method,"key":key}));
        }
        if self.pending.len() + self.queue.len() >= 256 {
            return self
                .error(id, -32000, "Too many outstanding MCP requests")
                .await;
        }
        if self.backend.is_none()
            && self
                .start(method != "initialize" && self.initialized)
                .await
                .is_err()
        {
            self.stop("start_failed").await;
            return self
                .error(
                    id,
                    -32000,
                    "Could not start MCP backend; check executable and arguments",
                )
                .await;
        }
        if self.handshake.is_some() {
            self.queue.push(message);
            Ok(())
        } else {
            self.dispatch(message).await
        }
    }
    async fn upstream(&mut self, mut message: Value) -> io::Result<()> {
        if self.diagnostic_input("backend-in", &message)? {
            return Ok(());
        }
        if message["method"] == "ping" && message.get("id").is_some() {
            return self
                .send(&json!({"jsonrpc":"2.0","id":message["id"],"result":{}}))
                .await;
        }
        if message.get("method").is_none() {
            if let Some(wire) = message["id"].as_str().map(str::to_owned) {
                if self.handshake.as_ref().is_some_and(|(id, _)| *id == wire) {
                    self.handshake = None;
                    if message.get("result").is_none() {
                        return self.fail("MCP backend initialization failed").await;
                    }
                    let key = Cache::key("initialize", &self.initialize, &self.initialize);
                    self.cache.put(&key, &message["result"]);
                    self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                        .await?;
                    for queued in std::mem::take(&mut self.queue) {
                        self.dispatch(queued).await?;
                    }
                    return Ok(());
                }
                if let Some(pending) = self.pending.remove(&wire) {
                    self.touch(&pending.method);
                    if cacheable(&pending.method)
                        && let Some(result) = message.get("result")
                    {
                        self.cache.put(&pending.cache_key, result);
                    }
                    message["id"] = pending.id;
                    return self.out(&message).await;
                }
            }
            return Ok(());
        }
        if message["method"] == "notifications/tools/list_changed" {
            self.cache.invalidate_tools();
        }
        if let Some(id) = message.get("id") {
            self.server_pending.insert(id.to_string(), Instant::now());
        }
        self.out(&message).await
    }
    async fn stop(&mut self, reason: &str) {
        self.handshake = None;
        self.server_pending.clear();
        if let Some(mut backend) = self.backend.take() {
            self.log_event("backend_stopping", json!({"reason":reason,"generation":self.generation,"backendPid":backend.child.id()}));
            backend.stop(self.config.shutdown).await;
            self.log_event(
                "backend_stopped",
                json!({"reason":reason,"generation":self.generation}),
            );
        }
    }
    async fn stop_discovery(&mut self) {
        if !self.operational
            && self.backend.is_some()
            && self.pending.is_empty()
            && self.queue.is_empty()
            && self.server_pending.is_empty()
            && self.handshake.is_none()
        {
            self.stop("discovery_complete").await;
        }
    }
    async fn fail(&mut self, reason: &str) -> io::Result<()> {
        self.stop(reason).await;
        for (_, pending) in std::mem::take(&mut self.pending) {
            self.error(pending.id, -32000, reason).await?;
        }
        for queued in std::mem::take(&mut self.queue) {
            if let Some(id) = queued.get("id") {
                self.error(id.clone(), -32000, reason).await?;
            }
        }
        Ok(())
    }
}

async fn termination() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result, _ = terminate.recv() => Ok(()) }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

async fn run(config: Config) -> io::Result<()> {
    let (tx, mut rx) = mpsc::channel(256);
    backend::watch_parent(tx.clone())?;
    let log = logging::Log::open().ok();
    let diagnostics = diagnostics::Control::open(&config.cache_dir.join("diagnostics")).ok();
    let mut proxy = Proxy {
        cache: Cache::new(&config),
        config,
        backend: None,
        operational: false,
        events: tx,
        generation: 0,
        sequence: 0,
        initialize: Value::Null,
        initialized: false,
        handshake: None,
        queue: Vec::new(),
        pending: HashMap::new(),
        server_pending: HashMap::new(),
        expires: Instant::now(),
        output: tokio::io::stdout(),
        diagnostics,
        log,
    };
    proxy.log_start();
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut tick = interval(Duration::from_millis(50));
    let interrupt = termination();
    tokio::pin!(interrupt);
    let result = loop {
        let action: io::Result<bool> = tokio::select! {
            line = input.next_line() => {
                if let Ok(Some(ref raw))=line {proxy.log_wire("client-to-proxy",raw);}
                match line {
                Ok(Some(line)) if line.trim().is_empty() => Ok(true),
                Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(message) if message.is_object() && message["jsonrpc"] == "2.0" => proxy.client(message).await.map(|_| true),
                    _ => proxy.error(Value::Null, -32700, "Expected a JSON-RPC 2.0 object").await.map(|_| true),
                },
                Ok(None) => Ok(false), Err(e) => Err(e),
                }
            },
            Some(event) = rx.recv() => match event {
                Event::Line(generation, line) if generation == proxy.generation && proxy.backend.is_some() => {
                    proxy.log_wire("backend-to-proxy", &line);
                    if line.trim().is_empty() { Ok(true) } else {
                        match serde_json::from_str::<Value>(&line) {
                            Ok(message) if message.is_object() && message["jsonrpc"] == "2.0" => proxy.upstream(message).await.map(|_| true),
                            _ => proxy.fail("MCP backend sent invalid JSON-RPC").await.map(|_| true),
                        }
                    }
                },
                Event::Closed(generation) if generation == proxy.generation && proxy.backend.is_some() => {
                    proxy.fail("MCP backend exited; request outcome may be unknown; no automatic retry").await.map(|_| true)
                },
                Event::Stderr(generation, line) => {
                    proxy.log_event("backend_stderr",json!({"generation":generation,"line":line}));
                    eprintln!("{line}"); Ok(true)
                },
                Event::ParentGone => Ok(false), _ => Ok(true),
            },
            _ = tick.tick() => {
                proxy.log_idle_deferred();
                if proxy.handshake.as_ref().is_some_and(|(_, deadline)| Instant::now() >= *deadline)
                    || proxy.pending.values().any(|p| p.method == "initialize" && p.started.elapsed() >= INITIALIZE_TIMEOUT) {
                    proxy.fail("MCP backend initialization timed out").await.map(|_| true)
                } else if proxy.pending.values().any(|p| p.started.elapsed() >= proxy.config.response_timeout)
                    || proxy.server_pending.values().any(|started| started.elapsed() >= proxy.config.response_timeout) {
                    proxy.fail("MCP response timeout; backend stopped; operation outcome may be unknown; no automatic retry").await.map(|_| true)
                } else {
                    if !proxy.config.idle.is_zero() && proxy.backend.is_some() && proxy.pending.is_empty() && proxy.queue.is_empty()
                        && proxy.server_pending.is_empty() && proxy.handshake.is_none() && Instant::now() >= proxy.expires {
                        proxy.stop("idle_timeout").await;
                    }
                    Ok(true)
                }
            },
            _ = &mut interrupt => Ok(false),
        };
        match proxy.diagnostic_pump().await {
            Some(diagnostics::Command::Exit) => break Ok(()),
            Some(diagnostics::Command::Fail) => break Err(io::Error::other("diagnostic exit")),
            _ => (),
        }
        match action {
            Ok(true) => proxy.stop_discovery().await,
            Ok(false) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    proxy.stop("proxy_exit").await;
    proxy.log_event(
        "proxy_exited",
        json!({"error":result.as_ref().err().map(|e|e.to_string())}),
    );
    result
}

fn main() {
    install_cleanup::run();
    let result = Config::parse().and_then(|config| {
        let Some(config) = config else {
            return Ok(());
        };
        #[cfg(windows)]
        let _update = update::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(run(config));
        // stdin uses a blocking OS read; it must not delay shutdown after a signal/parent exit.
        runtime.shutdown_background();
        result
    });
    if let Err(error) = result {
        eprintln!("mcp-lazy: {error}");
        std::process::exit(1);
    }
}
