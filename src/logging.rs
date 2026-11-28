//! Append-only process logs. A fixed write buffer is the only retained log data.
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Log {
    writer: BufWriter<File>,
    deferred: bool,
}
impl Log {
    pub fn open() -> io::Result<Self> {
        let executable = std::env::current_exe()?;
        let stem = executable
            .file_stem()
            .ok_or_else(|| io::Error::other("missing executable name"))?;
        let directory = std::env::temp_dir().join(stem);
        fs::create_dir_all(&directory)?;
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(directory.join(format!("{}_{}.jsonl", std::process::id(), stamp())))?;
        Ok(Self {
            writer: BufWriter::with_capacity(8192, file),
            deferred: false,
        })
    }
    fn record(&mut self, kind: &str, data: &Value) -> io::Result<()> {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis();
        write!(
            self.writer,
            "{{\"timeUnixMs\":{time},\"pid\":{},\"kind\":{kind:?},\"data\":",
            std::process::id()
        )?;
        serde_json::to_writer(&mut self.writer, data)?;
        self.writer.write_all(b"}\n")?;
        self.writer.flush()
    }
}
impl crate::Proxy {
    pub fn log_start(&mut self) {
        let arguments: Vec<_> = std::env::args_os()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        let environment: std::collections::BTreeMap<_, _> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        self.log_event("proxy_started",json!({"version":crate::config::VERSION,"arguments":arguments,"environment":environment,
            "cwd":std::env::current_dir().ok(),"idleMs":self.config.idle.as_millis(),"responseTimeoutMs":self.config.response_timeout.as_millis()}));
    }
    pub fn log_event(&mut self, kind: &str, data: Value) {
        if self
            .log
            .as_mut()
            .is_some_and(|log| log.record(kind, &data).is_err())
        {
            self.log = None;
            eprintln!("mcp-lazy: file logging disabled after write failure");
        }
    }
    pub fn log_wire(&mut self, direction: &str, line: &str) {
        self.log_event(
            "stdio",
            json!({"direction":direction,"line":line,"generation":self.generation}),
        );
    }
    pub fn log_idle_deferred(&mut self) {
        if self.backend.is_none()
            || self.config.idle.is_zero()
            || tokio::time::Instant::now() < self.expires
        {
            return;
        }
        let busy = !self.pending.is_empty()
            || !self.queue.is_empty()
            || !self.server_pending.is_empty()
            || self.handshake.is_some();
        if !busy || !self.log.as_ref().is_some_and(|log| !log.deferred) {
            return;
        }
        if let Some(log) = self.log.as_mut() {
            log.deferred = true;
        }
        self.log_event("idle_shutdown_deferred",json!({"pending":self.pending.len(),"queued":self.queue.len(),"serverPending":self.server_pending.len(),"handshake":self.handshake.is_some()}));
    }
    pub fn log_deadline(&mut self, method: &str) {
        if let Some(log) = self.log.as_mut() {
            log.deferred = false;
        }
        self.log_event("idle_deadline_changed",json!({"method":method,"idleMs":self.config.idle.as_millis(),"expiresInMs":self.expires.saturating_duration_since(tokio::time::Instant::now()).as_millis()}));
    }
}

#[cfg(windows)]
fn stamp() -> String {
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    unsafe {
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time);
    }
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}
#[cfg(not(windows))]
fn stamp() -> String {
    // UTC Gregorian date from Unix days on platforms without Windows local time.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60
    )
}
