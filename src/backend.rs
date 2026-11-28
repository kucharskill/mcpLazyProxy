use crate::config::Config;
use process_wrap::tokio::*;
use std::{io, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::ChildStdin,
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

pub enum Event {
    Line(u64, String),
    Closed(u64),
    Stderr(u64, String),
    ParentGone,
}
pub struct Backend {
    pub child: Box<dyn ChildWrapper>,
    pub input: Option<ChildStdin>,
    reader: JoinHandle<()>,
    stderr_reader: JoinHandle<()>,
}
impl Backend {
    pub fn spawn(
        config: &Config,
        generation: u64,
        events: mpsc::Sender<Event>,
    ) -> io::Result<Self> {
        let mut command = CommandWrap::with_new(&config.command[0], |command| {
            command
                .args(&config.command[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        command.wrap(KillOnDrop);
        #[cfg(windows)]
        {
            command.wrap(CreationFlags(0x08000000)).wrap(JobObject);
        }
        #[cfg(unix)]
        {
            command.wrap(ProcessGroup::leader());
        }
        let mut child = command.spawn()?;
        let input = child.stdin().take();
        let output = child
            .stdout()
            .take()
            .ok_or_else(|| io::Error::other("backend stdout missing"))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| io::Error::other("backend stderr missing"))?;
        let stderr_events = events.clone();
        let stderr_reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stderr_events
                    .send(Event::Stderr(generation, line))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        eprintln!("mcp-lazy: backend started pid={:?}", child.id());
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(output).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if events.send(Event::Line(generation, line)).await.is_err() {
                    return;
                }
            }
            let _ = events.send(Event::Closed(generation)).await;
        });
        Ok(Self {
            child,
            input,
            reader,
            stderr_reader,
        })
    }
    pub async fn send(&mut self, value: &serde_json::Value) -> io::Result<()> {
        let mut data = serde_json::to_vec(value)?;
        data.push(b'\n');
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::other("backend stdin closed"))?;
        timeout(std::time::Duration::from_secs(30), input.write_all(&data))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "backend stdin write timed out"))?
    }
    pub async fn stop(&mut self, grace: std::time::Duration) {
        drop(self.input.take());
        if !matches!(timeout(grace, self.child.wait()).await, Ok(Ok(_))) {
            let _ = self.child.start_kill();
            let _ = timeout(grace, self.child.wait()).await;
        }
        self.reader.abort();
        self.stderr_reader.abort();
        eprintln!("mcp-lazy: backend stopped");
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.reader.abort();
        self.stderr_reader.abort();
        let _ = self.child.start_kill();
    }
}

#[cfg(windows)]
pub fn watch_parent(events: mpsc::Sender<Event>) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
    };
    // Hold a HANDLE, not a PID, so PID reuse cannot select a different parent.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut parent = 0;
        let mut found = Process32FirstW(snapshot, &mut entry);
        while found != 0 {
            if entry.th32ProcessID == std::process::id() {
                parent = entry.th32ParentProcessID;
                break;
            }
            found = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        if parent == 0 {
            return Err(io::Error::other("cannot identify parent"));
        }
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent);
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = handle as usize;
        std::thread::spawn(move || {
            let handle = handle as HANDLE;
            if WaitForSingleObject(handle, INFINITE) == WAIT_OBJECT_0 {
                let _ = events.blocking_send(Event::ParentGone);
            }
            CloseHandle(handle);
        });
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn watch_parent(_events: mpsc::Sender<Event>) -> io::Result<()> {
    Ok(())
}
