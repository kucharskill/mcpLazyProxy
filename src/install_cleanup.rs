pub const LOCK_FILE: &str = ".mcp-lazy-proxy-install.lock";

#[cfg(windows)]
pub fn run() {
    use std::fs;
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(directory) = executable.parent() else {
        return;
    };
    // Coordinate with the installer so a rollback copy cannot disappear during a swap.
    let Ok(_lock) = lock(directory) else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if retired_name(name) || staging_name(name) {
            // Mapped/locked images remain until a later startup. Never terminate their processes.
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(windows)]
pub fn lock(directory: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    let path = directory.join(LOCK_FILE);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(&path)
    {
        Ok(file) => {
            file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))?;
            return Ok(file);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

#[cfg(any(windows, test))]
fn staging_name(name: &str) -> bool {
    name.strip_prefix("mcp-lazy-proxy.new-")
        .and_then(|name| name.strip_suffix(".exe"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(any(windows, test))]
fn retired_name(name: &str) -> bool {
    name.strip_prefix("mcp-lazy-proxy.old-")
        .and_then(|name| name.strip_suffix(".exe"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(not(windows))]
pub fn run() {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_accepts_only_exact_installer_backup_names() {
        assert!(staging_name(
            "mcp-lazy-proxy.new-0123456789abcdef0123456789abcdef.exe"
        ));
        assert!(retired_name(
            "mcp-lazy-proxy.old-0123456789abcdef0123456789abcdef.exe"
        ));
        for name in [
            "mcp-lazy-proxy.exe",
            "other.old-0123456789abcdef0123456789abcdef.exe",
            "mcp-lazy-proxy.old-not-a-guid.exe",
            "mcp-lazy-proxy.old-0123456789abcdef0123456789abcdef.exe.bak",
        ] {
            assert!(!retired_name(name));
        }
    }
}
