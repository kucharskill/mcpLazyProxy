use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

const TIME: u64 = 10_000;
fn directory() -> PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/native-update-tests")
        .join(format!("{}-{id}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("mcp-lazy-proxy.exe"), b"old-image").unwrap();
    save(
        &directory,
        &json!({"tag":"v0.1.5-build.7","installedAtUnix":TIME-1000,"lastCheckedAtUnix":0}),
    )
    .unwrap();
    directory
}
fn release(tag: &str) -> Value {
    json!({"tag_name":tag,"id":123,"assets":[{"name":"mcp-lazy-proxy.exe",
        "browser_download_url":"https://github.com/kucharskill/mcpLazyProxy/releases/download/v0.1.6-build.1/mcp-lazy-proxy.exe"}]})
}
fn no_staging(directory: &Path) {
    assert!(!fs::read_dir(directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("mcp-lazy-proxy.new-")
    }));
}

#[test]
fn versions_order_numeric_release_and_build_components() {
    assert!(version("v0.1.10-build.2") > version("v0.1.9-build.999"));
    assert!(version("v0.1.5-build.10") > version("v0.1.5-build.9"));
    assert_eq!(version("0.1.5"), Some([0, 1, 5, 0]));
    for value in [
        "latest",
        "v0.1",
        "v0.1.5-extra",
        "v0.1.5.6",
        "v0.1.5-build.bad",
    ] {
        assert_eq!(version(value), None);
    }
}

#[test]
fn fresh_installation_and_recent_attempt_skip_network() {
    let directory = directory();
    for state in [
        json!({"installedAtUnix":TIME-599}),
        json!({"installedAtUnix":TIME+10}),
        json!({"installedAtUnix":TIME-1000,"lastCheckedAtUnix":TIME-599}),
    ] {
        save(&directory, &state).unwrap();
        attempt(
            &directory,
            "0.1.5-build.7",
            TIME,
            &Mutex::new(false),
            |_, _, _| panic!("network should be skipped"),
        )
        .unwrap();
    }
    assert!(eligible(
        &directory,
        &json!({"installedAtUnix":TIME-600,"lastCheckedAtUnix":TIME-600}),
        TIME
    ));
}

#[test]
fn missing_install_record_uses_binary_write_time() {
    let directory = directory();
    fs::remove_file(directory.join(STATE)).unwrap();
    assert!(!eligible(&directory, &state(&directory), now()));
    let file = fs::OpenOptions::new()
        .write(true)
        .open(directory.join("mcp-lazy-proxy.exe"))
        .unwrap();
    file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    assert!(eligible(&directory, &state(&directory), now()));
}

#[test]
fn only_newer_release_downloads_and_replaces_the_image() {
    let directory = directory();
    let mut requests = 0;
    attempt(
        &directory,
        "0.1.5-build.7",
        TIME,
        &Mutex::new(false),
        |url, output, _| {
            requests += 1;
            if url == API {
                output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
            } else {
                output.write_all(b"new-image")
            }
        },
    )
    .unwrap();
    assert_eq!(requests, 2);
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"new-image"
    );
    let backups: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("mcp-lazy-proxy.old-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), b"old-image");
    assert_eq!(state(&directory)["tag"], "v0.1.6-build.1");
    assert_eq!(state(&directory)["lastCheckedAtUnix"], TIME);
    no_staging(&directory);
}

#[test]
fn equal_older_or_already_installed_release_is_metadata_only() {
    for tag in ["v0.1.5-build.7", "v0.1.5-build.6", "v0.1.4-build.999"] {
        let directory = directory();
        let mut requests = 0;
        attempt(
            &directory,
            "0.1.5-build.7",
            TIME,
            &Mutex::new(false),
            |url, output, _| {
                assert_eq!(url, API);
                requests += 1;
                output.write_all(&serde_json::to_vec(&release(tag)).unwrap())
            },
        )
        .unwrap();
        assert_eq!(requests, 1);
        assert_eq!(
            fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
            b"old-image"
        );
    }
    let directory = directory();
    let mut saved = state(&directory);
    saved["tag"] = json!("v0.1.6-build.1");
    save(&directory, &saved).unwrap();
    attempt(
        &directory,
        "0.1.5-build.7",
        TIME,
        &Mutex::new(false),
        |url, output, _| {
            assert_eq!(url, API);
            output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"old-image"
    );
}

#[test]
fn busy_installer_lock_fails_immediately_without_network() {
    let directory = directory();
    let _lock = install_cleanup::lock(&directory).unwrap();
    let began = Instant::now();
    assert!(
        attempt(
            &directory,
            "0.1.5",
            TIME,
            &Mutex::new(false),
            |_, _, _| panic!("network while locked")
        )
        .is_err()
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(state(&directory)["lastCheckedAtUnix"], 0);
}

#[test]
fn concurrent_starts_make_one_attempt_and_later_starts_obey_cooldown() {
    let directory = directory();
    let requests = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..10)
        .map(|_| {
            let directory = directory.clone();
            let requests = requests.clone();
            thread::spawn(move || {
                let _ = attempt(
                    &directory,
                    "0.1.5-build.7",
                    TIME,
                    &Mutex::new(false),
                    |_, output, _| {
                        requests.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        output.write_all(&serde_json::to_vec(&release("v0.1.5-build.7")).unwrap())
                    },
                );
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    attempt(
        &directory,
        "0.1.5-build.7",
        TIME + 599,
        &Mutex::new(false),
        |_, _, _| panic!("cooldown was renewed incorrectly"),
    )
    .unwrap();
    assert_eq!(state(&directory)["lastCheckedAtUnix"], TIME);
}

#[test]
fn partial_download_failure_keeps_the_old_image_and_records_the_attempt() {
    let directory = directory();
    assert!(
        attempt(
            &directory,
            "0.1.5",
            TIME,
            &Mutex::new(false),
            |url, output, _| {
                if url == API {
                    output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
                } else {
                    output.write_all(b"partial")?;
                    Err(io::Error::other("connection lost"))
                }
            }
        )
        .is_err()
    );
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"old-image"
    );
    assert_eq!(state(&directory)["lastCheckedAtUnix"], TIME);
    no_staging(&directory);
}

#[test]
fn normal_exit_cancels_before_swap_and_locked_target_is_left_untouched() {
    use std::os::windows::fs::OpenOptionsExt;
    for cancel in [true, false] {
        let directory = directory();
        let held = if cancel {
            None
        } else {
            Some(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(directory.join("mcp-lazy-proxy.exe"))
                    .unwrap(),
            )
        };
        let result = attempt(
            &directory,
            "0.1.5",
            TIME,
            &Mutex::new(cancel),
            |url, output, _| {
                if url == API {
                    output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
                } else {
                    output.write_all(b"new-image")
                }
            },
        );
        assert_eq!(result.is_ok(), cancel);
        drop(held);
        assert_eq!(
            fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
            b"old-image"
        );
        no_staging(&directory);
    }
}

#[test]
fn invalid_release_response_preserves_the_image() {
    for response in [b"invalid JSON".to_vec(),serde_json::to_vec(&json!({"tag_name":"v0.1.6","assets":[]})).unwrap(),
        serde_json::to_vec(&json!({"tag_name":"v0.1.6","assets":[{"name":"mcp-lazy-proxy.exe","browser_download_url":"https://example.com/program.exe"}]})).unwrap()] {
        let directory=directory();
        assert!(attempt(&directory,"0.1.5",TIME,&Mutex::new(false),|url,output,_|{assert_eq!(url,API);output.write_all(&response)}).is_err());
        assert_eq!(fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),b"old-image");no_staging(&directory);
    }
}

#[test]
fn native_http_streams_content_and_rejects_http_errors_and_oversized_responses() {
    for (status, limit, success) in [
        ("200 OK", 100, true),
        ("404 Not Found", 100, false),
        ("200 OK", 2, false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                connection.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with("GET /asset?test=1 HTTP/"));
            write!(
                connection,
                "HTTP/1.1 {status}\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
            )
            .unwrap();
        });
        let mut bytes = Vec::new();
        let result = http::get(&format!("http://{address}/asset?test=1"), &mut bytes, limit);
        server.join().unwrap();
        assert_eq!(result.is_ok(), success);
        if success {
            assert_eq!(bytes, b"hello");
        }
    }
}

#[test]
fn dropping_the_startup_guard_waits_only_for_the_swap_section() {
    let directory = directory();
    let flag = Arc::new(Mutex::new(false));
    let guard = Guard(flag.clone());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let worker_dir = directory.clone();
    let worker = thread::spawn(move || {
        attempt(&worker_dir, "0.1.5", TIME, &flag, |url, output, _| {
            if url == API {
                output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
            } else {
                entered_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                output.write_all(b"new-image")
            }
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let began = Instant::now();
    drop(guard);
    assert!(began.elapsed() < Duration::from_secs(1));
    resume_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"old-image"
    );
    no_staging(&directory);
}

#[test]
#[ignore = "downloads the latest public GitHub executable through native WinHTTP"]
fn live_github_update() {
    let directory = directory();
    let mut saved = state(&directory);
    saved["tag"] = json!("v0.0.0");
    save(&directory, &saved).unwrap();
    attempt(&directory, "0.0.0", TIME, &Mutex::new(false), http::get).unwrap();
    assert!(version(state(&directory)["tag"].as_str().unwrap()) > version("0.0.0"));
    let file = fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap();
    assert!(file.len() > 100_000);
    assert!(file.starts_with(b"MZ"));
    no_staging(&directory);
}
