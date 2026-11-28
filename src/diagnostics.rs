//! Removable local diagnostics bridge. Protocol experiments and routing hooks live here.
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub enum Command {
    StopBackend,
    Exit,
    Fail,
}

pub struct Control {
    directory: PathBuf,
    next: Instant,
    pending: Option<Command>,
    capture: bool,
    held_directions: HashSet<String>,
    held: Vec<(String, Value)>,
    injected: HashSet<(String, String)>,
    sequence: u64,
    action: Option<Value>,
}
impl Control {
    pub fn open(root: &Path) -> io::Result<Self> {
        // The environment override is useful for isolated test processes.
        let enabled = std::env::var("MCP_LAZY_DIAGNOSTICS")
            .ok()
            .map(|v| v == "1")
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| fs::read(p.parent()?.join("installed-release.json")).ok())
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|state| state["debugMode"] == true)
            });
        if !enabled {
            return Err(io::Error::other("diagnostics disabled"));
        }
        fs::create_dir_all(root)?;
        let token = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let directory = root.join(format!("{}-{token:x}", std::process::id()));
        fs::create_dir(&directory)?;
        let control = Self {
            directory,
            next: Instant::now(),
            pending: None,
            capture: false,
            held_directions: HashSet::new(),
            held: Vec::new(),
            injected: HashSet::new(),
            sequence: 0,
            action: None,
        };
        control.status(false, false, "ready")?;
        Ok(control)
    }
    fn status(&self, busy: bool, backend: bool, state: &str) -> io::Result<()> {
        let data = serde_json::to_vec(
            &json!({"pid":std::process::id(),"version":crate::config::VERSION,"busy":busy,"backendRunning":backend,"state":state,"capture":self.capture,"heldMessages":self.held.len(),"heldDirections":self.held_directions}),
        )?;
        fs::write(self.directory.join("status.part"), data)?;
        fs::rename(
            self.directory.join("status.part"),
            self.directory.join("status.json"),
        )
    }
    fn record(&self, direction: &str, message: &Value) {
        if !self.capture {
            return;
        }
        let path = self.directory.join("events.jsonl");
        if fs::metadata(&path).is_ok_and(|m| m.len() >= 4 * 1024 * 1024) {
            return;
        }
        if let Ok(mut file) = fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = writeln!(file, "{}", json!({"direction":direction,"message":message}));
        }
    }
    fn input(&mut self, direction: &str, message: &Value) -> io::Result<bool> {
        self.record(direction, message);
        if message.get("method").is_none()
            && let Some(id) = message.get("id")
            && self
                .injected
                .remove(&(direction.to_owned(), id.to_string()))
        {
            return Ok(true);
        }
        if self.held_directions.contains(direction) {
            if self.held.len() >= 256 {
                return Err(io::Error::other("diagnostic hold queue full"));
            }
            self.held.push((direction.to_owned(), message.clone()));
            return Ok(true);
        }
        Ok(false)
    }
    fn poll(&mut self, busy: bool, backend: bool) -> io::Result<Option<Command>> {
        if Instant::now() < self.next {
            return Ok(None);
        }
        self.next = Instant::now() + Duration::from_secs(1);
        match fs::read_to_string(self.directory.join("command")) {
            Ok(text) => {
                fs::remove_file(self.directory.join("command"))?;
                let request = serde_json::from_str::<Value>(&text)
                    .unwrap_or_else(|_| json!({"op":text.trim()}));
                let op = request["op"].as_str().unwrap_or("");
                match op {
                    "status" => (),
                    "stop-backend" => self.pending = Some(Command::StopBackend),
                    "exit" => self.pending = Some(Command::Exit),
                    "fail" => self.pending = Some(Command::Fail),
                    "observe" => self.capture = request["enabled"].as_bool().unwrap_or(true),
                    "hold" => {
                        let direction = request["direction"].as_str().unwrap_or("client-in");
                        if matches!(direction, "client-in" | "backend-in") {
                            self.held_directions.insert(direction.to_owned());
                        }
                    }
                    "resume" => {
                        self.held_directions.clear();
                    }
                    "sendToClient" | "sendToBackend" => self.action = Some(request),
                    _ => {
                        self.status(busy, backend, "unknown-command")?;
                        return Ok(None);
                    }
                }
                self.status(
                    busy,
                    backend,
                    if self.pending.is_some() {
                        "pending"
                    } else {
                        "ready"
                    },
                )?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        if busy {
            return Ok(None);
        }
        if let Some(command) = self.pending.take() {
            self.status(busy, backend, "accepted")?;
            return Ok(Some(command));
        }
        Ok(None)
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        for file in ["status.json", "status.part", "command"] {
            let _ = fs::remove_file(self.directory.join(file));
        }
        // Explicitly captured traffic remains available for analysis after transport exit.
        let _ = fs::remove_dir(&self.directory);
    }
}

impl crate::Proxy {
    pub fn diagnostic_observe(&self, direction: &str, message: &Value) {
        if let Some(control) = self.diagnostics.as_ref() {
            control.record(direction, message)
        }
    }
    pub fn diagnostic_input(&mut self, direction: &str, message: &Value) -> io::Result<bool> {
        self.diagnostics
            .as_mut()
            .map_or(Ok(false), |c| c.input(direction, message))
    }
    pub async fn diagnostic_pump(&mut self) -> Option<Command> {
        let busy = !self.pending.is_empty()
            || !self.queue.is_empty()
            || !self.server_pending.is_empty()
            || self.handshake.is_some();
        let control = self.diagnostics.as_mut()?;
        let lifecycle = control
            .poll(busy || !control.held.is_empty(), self.backend.is_some())
            .unwrap_or(None);
        let action = control.action.take();
        let held = if control.held_directions.is_empty() {
            std::mem::take(&mut control.held)
        } else {
            Vec::new()
        };
        // Replay without logging or re-holding the same messages.
        let diagnostics = self.diagnostics.take();
        for (direction, message) in held {
            let result = if direction == "client-in" {
                self.client(message).await
            } else {
                self.upstream(message).await
            };
            if let Err(e) = result {
                eprintln!("mcp-lazy diagnostic replay: {e}");
            }
        }
        self.diagnostics = diagnostics;
        if let Some(action) = action {
            self.diagnostic_send(action).await;
        }
        if matches!(lifecycle, Some(Command::StopBackend)) {
            self.stop().await;
            return None;
        }
        lifecycle
    }
    async fn diagnostic_send(&mut self, action: Value) {
        let to_client = action["op"] == "sendToClient";
        let mut message = action["message"].clone();
        let result = async {
            if !message.is_object() || message["jsonrpc"] != "2.0" {
                return Err(io::Error::other("expected a JSON-RPC 2.0 object"));
            }
            if !to_client && self.backend.is_none() {
                return Err(io::Error::other("backend is stopped"));
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                let control = self.diagnostics.as_mut().unwrap();
                if control.injected.len() >= 256 {
                    return Err(io::Error::other("too many diagnostic requests"));
                }
                control.sequence += 1;
                let id = format!(
                    "lazy-diagnostic-{}-{}",
                    control.directory.file_name().unwrap().to_string_lossy(),
                    control.sequence
                );
                message["id"] = json!(id);
                control.injected.insert((
                    if to_client { "client-in" } else { "backend-in" }.to_owned(),
                    message["id"].to_string(),
                ));
            }
            if to_client {
                self.out(&message).await
            } else {
                self.send(&message).await
            }
        }
        .await;
        if let Some(control) = self.diagnostics.as_ref() {
            let report = json!({"sent":message,"error":result.err().map(|e|e.to_string())});
            control.record("diagnostic", &report);
            let _ = fs::write(control.directory.join("last-send.json"), report.to_string());
        }
    }
}
