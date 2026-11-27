mod http;
#[cfg(test)]
mod tests;
use crate::install_cleanup;
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const INTERVAL: u64 = 600;
const API: &str = "https://api.github.com/repos/kucharskill/mcpLazyProxy/releases/latest";
const STATE: &str = "installed-release.json";

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
            let _ = attempt(
                &directory,
                crate::config::VERSION,
                now(),
                &cancel,
                |url, output, limit| http::get(url, output, limit),
            );
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
fn version(tag: &str) -> Option<[u64; 4]> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let (base, build) = match tag.split_once("-build.") {
        Some((base, n)) => (base, n.parse().ok()?),
        None => (tag, 0),
    };
    let mut parts = base.split('.');
    let result = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        build,
    ];
    if parts.next().is_some() {
        return None;
    }
    Some(result)
}
fn state(directory: &Path) -> Value {
    fs::read(directory.join(STATE))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn save(directory: &Path, state: &Value) -> io::Result<()> {
    fs::write(directory.join(STATE), serde_json::to_vec(state)?)
}
fn eligible(directory: &Path, state: &Value, now: u64) -> bool {
    let installed = state["installedAtUnix"].as_u64().or_else(|| {
        fs::metadata(directory.join("mcp-lazy-proxy.exe"))
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs())
    });
    installed.is_some_and(|installed| now.saturating_sub(installed) >= INTERVAL)
        && state["lastCheckedAtUnix"]
            .as_u64()
            .is_none_or(|checked| now.saturating_sub(checked) >= INTERVAL)
}
struct Incoming(PathBuf);
impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn attempt(
    directory: &Path,
    current: &str,
    time: u64,
    stopped: &Mutex<bool>,
    mut get: impl FnMut(&str, &mut dyn io::Write, usize) -> io::Result<()>,
) -> io::Result<()> {
    if !eligible(directory, &state(directory), time) {
        return Ok(());
    }
    // Acquisition never waits. Re-read state after taking the installer's shared lock.
    let _lock = install_cleanup::lock(directory)?;
    let mut state = state(directory);
    if !eligible(directory, &state, time) {
        return Ok(());
    }
    state["lastCheckedAtUnix"] = json!(time);
    save(directory, &state)?;
    let mut bytes = Vec::new();
    get(API, &mut bytes, 2 * 1024 * 1024)?;
    let release: Value = serde_json::from_slice(&bytes)?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing release version"))?;
    let newest = version(tag).ok_or_else(|| io::Error::other("unsupported release version"))?;
    let current = version(current).ok_or_else(|| io::Error::other("missing local version"))?;
    let installed = state["tag"]
        .as_str()
        .and_then(version)
        .unwrap_or(current)
        .max(current);
    if newest <= installed {
        return Ok(());
    }
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
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing download URL"))?;
    if !url.starts_with("https://github.com/kucharskill/mcpLazyProxy/releases/download/") {
        return Err(io::Error::other("unexpected release URL"));
    }
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
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&incoming.0)?;
        get(url, &mut file, 32 * 1024 * 1024)?;
        file.sync_all()?;
    }
    let cancelled = stopped
        .lock()
        .map_err(|_| io::Error::other("updater stopped"))?;
    if *cancelled {
        return Ok(());
    }
    let target = directory.join("mcp-lazy-proxy.exe");
    let backup = directory.join(format!("mcp-lazy-proxy.old-{id}.exe"));
    fs::rename(&target, &backup)?;
    if let Err(error) = fs::rename(&incoming.0, &target) {
        let _ = fs::rename(&backup, &target);
        return Err(error);
    }
    state["tag"] = json!(tag);
    state["releaseId"] = release["id"].clone();
    state["installedAtUnix"] = json!(now());
    save(directory, &state)?;
    Ok(())
}
