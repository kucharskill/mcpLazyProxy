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
    set_modified(&directory.join("mcp-lazy-proxy.exe"), TIME - 1000);
    save(&directory, &json!({})).unwrap();
    directory
}
fn save(directory: &Path, value: &Value) -> io::Result<()> {
    fs::write(state_path(directory)?, serde_json::to_vec(value)?)
}
fn set_modified(path: &Path, time: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(time)))
        .unwrap();
}
fn checked(directory: &Path) -> u64 {
    modified(&directory.join(install_cleanup::LOCK_FILE))
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn release(tag: &str) -> Value {
    json!({"tag_name":tag,"id":123,"assets":[{"name":"mcp-lazy-proxy.exe", "digest":format!("sha256:{:x}",Sha256::digest(b"new-image")),
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
fn fresh_installation_and_recent_attempt_skip_network() {
    let directory = directory();
    for (installed, last_checked) in [(TIME - 599, 0), (TIME + 10, 0), (TIME - 1000, TIME - 599)] {
        set_modified(&directory.join("mcp-lazy-proxy.exe"), installed);
        let lock = install_cleanup::lock(&directory).unwrap();
        lock.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(last_checked)),
        )
        .unwrap();
        drop(lock);
        attempt(&directory, TIME, &Mutex::new(false), |_, _, _| {
            panic!("network should be skipped")
        })
        .unwrap();
    }
    set_modified(&directory.join("mcp-lazy-proxy.exe"), TIME - 600);
    assert!(eligible(&directory, &json!({}), TIME));
}

#[test]
fn missing_install_record_uses_binary_write_time() {
    let directory = directory();
    fs::remove_file(state_path(&directory).unwrap()).unwrap();
    set_modified(&directory.join("mcp-lazy-proxy.exe"), now());
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
fn different_digest_downloads_and_replaces_the_image() {
    let directory = directory();
    let mut requests = 0;
    attempt(&directory, TIME, &Mutex::new(false), |url, output, _| {
        requests += 1;
        if url == API {
            output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
        } else {
            output.write_all(b"new-image")
        }
    })
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
    assert_eq!(state(&directory), json!({}));
    assert_eq!(checked(&directory), TIME);
    no_staging(&directory);
}

#[test]
fn matching_digest_skips_download_regardless_of_version() {
    for tag in ["v0.1.5-build.7", "v0.1.4", "v99.0.0"] {
        let directory = directory();
        let mut requests = 0;
        attempt(&directory, TIME, &Mutex::new(false), |url, output, _| {
            assert_eq!(url, API);
            requests += 1;
            let mut response = release(tag);
            response["assets"][0]["digest"] =
                json!(digest(&directory.join("mcp-lazy-proxy.exe")).unwrap());
            output.write_all(&serde_json::to_vec(&response).unwrap())
        })
        .unwrap();
        assert_eq!(requests, 1);
        assert_eq!(
            fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
            b"old-image"
        );
    }
}

#[test]
fn busy_installer_lock_fails_immediately_without_network() {
    let directory = directory();
    let _lock = install_cleanup::lock(&directory).unwrap();
    let began = Instant::now();
    assert!(
        attempt(&directory, TIME, &Mutex::new(false), |_, _, _| panic!(
            "network while locked"
        ))
        .is_err()
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(checked(&directory), 0);
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
                let _ = attempt(&directory, TIME, &Mutex::new(false), |_, output, _| {
                    requests.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(50));
                    let mut response = release("v0.1.5-build.7");
                    response["assets"][0]["digest"] =
                        json!(digest(&directory.join("mcp-lazy-proxy.exe")).unwrap());
                    output.write_all(&serde_json::to_vec(&response).unwrap())
                });
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    attempt(&directory, TIME + 599, &Mutex::new(false), |_, _, _| {
        panic!("cooldown was renewed incorrectly")
    })
    .unwrap();
    assert_eq!(checked(&directory), TIME);
}

#[test]
fn partial_download_failure_keeps_the_old_image_and_records_the_attempt() {
    let directory = directory();
    assert!(
        attempt(&directory, TIME, &Mutex::new(false), |url, output, _| {
            if url == API {
                output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
            } else {
                output.write_all(b"partial")?;
                Err(io::Error::other("connection lost"))
            }
        })
        .is_err()
    );
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"old-image"
    );
    assert_eq!(checked(&directory), TIME);
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
        let result = attempt(&directory, TIME, &Mutex::new(cancel), |url, output, _| {
            if url == API {
                output.write_all(&serde_json::to_vec(&release("v0.1.6-build.1")).unwrap())
            } else {
                output.write_all(b"new-image")
            }
        });
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
        assert!(attempt(&directory,TIME,&Mutex::new(false),|url,output,_|{assert_eq!(url,API);output.write_all(&response)}).is_err());
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
        attempt(&worker_dir, TIME, &flag, |url, output, _| {
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
    attempt(&directory, TIME, &Mutex::new(false), http::get).unwrap();
    assert_eq!(state(&directory), json!({}));
    let file = fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap();
    assert!(file.len() > 100_000);
    assert!(file.starts_with(b"MZ"));
    no_staging(&directory);
}

#[test]
fn local_update_uses_digest_without_network_and_preserves_settings() {
    let directory = directory();
    let source = directory.join("local-build.exe");
    fs::write(&source, b"local-image").unwrap();
    let mut record = state(&directory);
    record["updatePath"] = json!(source);
    set_modified(&directory.join("mcp-lazy-proxy.exe"), TIME);
    save(&directory, &record).unwrap();
    attempt(&directory, TIME, &Mutex::new(false), |_, _, _| {
        panic!("local mode must not use network")
    })
    .unwrap();
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"local-image"
    );
    assert_eq!(state(&directory)["updatePath"], record["updatePath"]);
    assert_eq!(state(&directory), record);
    let before = fs::read_dir(&directory).unwrap().count();
    attempt(&directory, TIME, &Mutex::new(false), |_, _, _| {
        panic!("no network")
    })
    .unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), before);
}

#[test]
fn broken_local_update_never_falls_back_to_github() {
    for path in [
        "relative.exe".to_string(),
        directory()
            .join("missing.exe")
            .to_string_lossy()
            .into_owned(),
    ] {
        let directory = directory();
        let mut record = state(&directory);
        record["updatePath"] = json!(path);
        save(&directory, &record).unwrap();
        assert!(
            attempt(&directory, TIME, &Mutex::new(false), |_, _, _| panic!(
                "no network"
            ))
            .is_err()
        );
        assert_eq!(
            fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
            b"old-image"
        );
        no_staging(&directory);
    }
}

#[test]
fn mismatched_asset_digest_preserves_installed_image() {
    let directory = directory();
    assert!(
        attempt(&directory, TIME, &Mutex::new(false), |url, output, _| {
            if url == API {
                output.write_all(&serde_json::to_vec(&release("v0.1.5")).unwrap())
            } else {
                output.write_all(b"unexpected-image")
            }
        })
        .is_err()
    );
    assert_eq!(
        fs::read(directory.join("mcp-lazy-proxy.exe")).unwrap(),
        b"old-image"
    );
    no_staging(&directory);
}

#[test]
fn acquiring_cleanup_lock_does_not_renew_github_cooldown() {
    let directory = directory();
    let lock = install_cleanup::lock(&directory).unwrap();
    lock.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(TIME - 600)))
        .unwrap();
    drop(lock);
    drop(install_cleanup::lock(&directory).unwrap());
    assert_eq!(checked(&directory), TIME - 600);
}
