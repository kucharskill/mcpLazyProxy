//! Local file control for transport lifecycle experiments.
use std::{
    fs, io,
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
}

impl Control {
    pub fn open(root: &Path) -> io::Result<Self> {
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
        };
        control.status(false, false, "ready")?;
        Ok(control)
    }

    fn status(&self, busy: bool, backend: bool, state: &str) -> io::Result<()> {
        let data = serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(), "version": crate::config::VERSION,
            "busy": busy, "backendRunning": backend, "state": state,
        }))?;
        let staged = self.directory.join("status.part");
        fs::write(&staged, data)?;
        fs::rename(staged, self.directory.join("status.json"))
    }

    pub fn poll(&mut self, busy: bool, backend: bool) -> io::Result<Option<Command>> {
        if Instant::now() < self.next {
            return Ok(None);
        }
        self.next = Instant::now() + Duration::from_secs(1);
        let path = self.directory.join("command");
        match fs::read_to_string(&path) {
            Ok(text) => {
                fs::remove_file(path)?;
                match text.trim() {
                    "status" => (),
                    "stop-backend" => self.pending = Some(Command::StopBackend),
                    "exit" => self.pending = Some(Command::Exit),
                    "fail" => self.pending = Some(Command::Fail),
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
        let _ = fs::remove_dir(&self.directory);
    }
}
