mod http;
#[cfg(test)]
mod tests;
use crate::install_cleanup;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const INTERVAL: u64 = 600;
const API: &str = "https://api.github.com/repos/kucharskill/mcpLazyProxy/releases/latest";
fn state_path(directory: &Path) -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let filename = executable
        .file_name()
        .ok_or_else(|| io::Error::other("missing executable filename"))?;
    Ok(directory.join(filename).with_extension("json"))
}

pub struct Guard(Arc<Mutex<bool>>);
impl Drop for Guard {
    fn drop(&mut self) {
        // A normal proxy exit can cancel downloads, but waits for the brief file-swap section.
        if let Ok(mut stopped) = self.0.lock() {
            *stopped = true;
        }
    }
}

pub fn start() -> Option<Guard> {
    let executable = std::env::current_exe().ok()?;
    if executable.file_name()? != "mcp-lazy-proxy.exe" {
        return None;
    }
    let directory = executable.parent()?.to_path_buf();
    let stopped = Arc::new(Mutex::new(false));
    let cancel = stopped.clone();
    std::thread::Builder::new()
        .name("mcp-update".into())
        .spawn(move || {
            let _ = attempt(&directory, now(), &cancel, |url, output, limit| {
                http::get(url, output, limit)
            });
        })
        .ok()?;
    Some(Guard(stopped))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn digest(path: &Path) -> io::Result<String> {
    Ok(format!("sha256:{:x}", Sha256::digest(fs::read(path)?)))
}
fn state(directory: &Path) -> Value {
    state_path(directory)
        .ok()
        .and_then(|path| fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}
fn recent(modified: Option<SystemTime>, time: u64) -> bool {
    modified
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|m| time.saturating_sub(m.as_secs()) < INTERVAL)
}
fn eligible(directory: &Path, state: &Value, time: u64) -> bool {
    state.get("updatePath").is_some()
        || modified(&directory.join("mcp-lazy-proxy.exe")).is_some_and(|m| !recent(Some(m), time))
}
struct Incoming(PathBuf);
impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn attempt(
    directory: &Path,
    time: u64,
    stopped: &Mutex<bool>,
    mut get: impl FnMut(&str, &mut dyn io::Write, usize) -> io::Result<()>,
) -> io::Result<()> {
    let local = state(directory).get("updatePath").is_some();
    if !eligible(directory, &state(directory), time)
        || (!local && recent(modified(&directory.join(install_cleanup::LOCK_FILE)), time))
    {
        return Ok(());
    }
    // Acquisition never waits. Re-read state after taking the installer's shared lock.
    let lock = install_cleanup::lock(directory)?;
    let state = state(directory);
    let local = state.get("updatePath").is_some();
    if !eligible(directory, &state, time)
        || (!local && recent(lock.metadata()?.modified().ok(), time))
    {
        return Ok(());
    }
    if !local {
        lock.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(time)),
        )?;
    }
    if let Some(path) = state.get("updatePath").cloned() {
        let source = PathBuf::from(
            path.as_str()
                .ok_or_else(|| io::Error::other("invalid updatePath"))?,
        );
        if !source.is_absolute() {
            return Err(io::Error::other("updatePath must be absolute"));
        }
        let metadata = fs::metadata(&source)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 32 * 1024 * 1024 {
            return Err(io::Error::other("invalid local update file"));
        }
        let source_digest = digest(&source)?;
        if source_digest == digest(&directory.join("mcp-lazy-proxy.exe"))? {
            return Ok(());
        }
        let (id, incoming) = staging(directory);
        fs::copy(&source, &incoming.0)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&incoming.0)?
            .sync_all()?;
        if digest(&incoming.0)? != source_digest {
            return Err(io::Error::other("local update changed during copy"));
        }
        return replace(directory, incoming, &id, stopped);
    }
    let mut bytes = Vec::new();
    get(API, &mut bytes, 2 * 1024 * 1024)?;
    let release: Value = serde_json::from_slice(&bytes)?;
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| io::Error::other("missing release assets"))?;
    let mut assets = assets
        .iter()
        .filter(|asset| asset["name"] == "mcp-lazy-proxy.exe");
    let asset = assets
        .next()
        .ok_or_else(|| io::Error::other("missing executable"))?;
    if assets.next().is_some() {
        return Err(io::Error::other("ambiguous executable"));
    }
    let expected = asset["digest"]
        .as_str()
        .filter(|value| {
            value
                .strip_prefix("sha256:")
                .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .ok_or_else(|| io::Error::other("missing asset SHA-256"))?
        .to_ascii_lowercase();
    if expected == digest(&directory.join("mcp-lazy-proxy.exe"))? {
        return Ok(());
    }
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing download URL"))?;
    if !url.starts_with("https://github.com/kucharskill/mcpLazyProxy/releases/download/") {
        return Err(io::Error::other("unexpected release URL"));
    }
    let (id, incoming) = staging(directory);
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&incoming.0)?;
        get(url, &mut file, 32 * 1024 * 1024)?;
        file.sync_all()?;
    }
    if digest(&incoming.0)? != expected {
        return Err(io::Error::other("asset SHA-256 mismatch"));
    }
    replace(directory, incoming, &id, stopped)
}

fn staging(directory: &Path) -> (String, Incoming) {
    let id = format!(
        "{:032x}",
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            << 32)
            | u128::from(std::process::id())
    );
    let incoming = Incoming(directory.join(format!("mcp-lazy-proxy.new-{id}.exe")));
    (id, incoming)
}

fn replace(
    directory: &Path,
    incoming: Incoming,
    id: &str,
    stopped: &Mutex<bool>,
) -> io::Result<()> {
    let cancelled = stopped
        .lock()
        .map_err(|_| io::Error::other("updater stopped"))?;
    if *cancelled {
        return Ok(());
    }
    let target = directory.join("mcp-lazy-proxy.exe");
    let backup = directory.join(format!("mcp-lazy-proxy.old-{id}.exe"));
    fs::OpenOptions::new()
        .write(true)
        .open(&incoming.0)?
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))?;
    fs::rename(&target, &backup)?;
    if let Err(error) = fs::rename(&incoming.0, &target) {
        let _ = fs::rename(&backup, &target);
        return Err(error);
    }
    Ok(())
}
