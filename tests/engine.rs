use ariai::{
    engine::download,
    model::{DownloadOptions, DownloadProgress, DownloadRequest},
};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Server {
    bytes: Arc<Vec<u8>>,
    etag: Arc<std::sync::Mutex<String>>,
    ranges: bool,
    slow: bool,
    requests: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<(usize, usize)>>>,
}
async fn handler(State(server): State<Server>, headers: HeaderMap) -> Response {
    server.requests.fetch_add(1, Ordering::SeqCst);
    let tag = server.etag.lock().unwrap().clone();
    let size = server.bytes.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
    if let Some(r) = range {
        server.seen.lock().unwrap().push(r);
    }
    let (status, start, end) = if let (true, Some((a, b))) = (server.ranges, range) {
        (StatusCode::PARTIAL_CONTENT, a, b)
    } else {
        (StatusCode::OK, 0, size - 1)
    };
    let data = server.bytes[start..=end].to_vec();
    let mut builder = Response::builder()
        .status(status)
        .header(header::ETAG, tag)
        .header(header::CONTENT_LENGTH, data.len());
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    if server.slow && data.len() > 1 {
        let chunks: Vec<Vec<u8>> = data.chunks(1024).map(|c| c.to_vec()).collect();
        let stream = futures_util::stream::unfold((chunks, 0usize), |(chunks, i)| async move {
            if i == chunks.len() {
                None
            } else {
                tokio::time::sleep(Duration::from_millis(4)).await;
                let bytes = chunks[i].clone();
                Some((Ok::<_, std::io::Error>(bytes), (chunks, i + 1)))
            }
        });
        builder.body(Body::from_stream(stream)).unwrap()
    } else {
        builder.body(Body::from(data)).unwrap()
    }
}
async fn start(ranges: bool, slow: bool) -> (String, Server) {
    let server = Server {
        bytes: Arc::new((0..131072).map(|n| (n % 251) as u8).collect()),
        etag: Arc::new(std::sync::Mutex::new("\"v1\"".into())),
        ranges,
        slow,
        requests: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/file", get(handler))
                .with_state(server.clone()),
        )
        .into_future(),
    );
    (format!("http://{addr}/file"), server)
}
fn request(urls: Vec<String>, dir: &std::path::Path) -> DownloadRequest {
    DownloadRequest {
        uris: urls,
        options: DownloadOptions {
            dir: dir.to_path_buf(),
            out: Some("result.bin".into()),
            split: 4,
            min_split_size: 1024,
            retry_wait_secs: 0,
            max_tries: 2,
            ..Default::default()
        },
    }
}
fn progress() -> (
    watch::Sender<DownloadProgress>,
    watch::Receiver<DownloadProgress>,
) {
    watch::channel(DownloadProgress::default())
}
#[tokio::test]
async fn segmented_file_matches_source() {
    let (url, server) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(vec![url], dir.path());
    req.options.checksum = Some(format!(
        "sha-256={}",
        hex::encode(Sha256::digest(server.bytes.as_ref()))
    ));
    let result = download(req, CancellationToken::new(), progress().0)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
    assert!(server.requests.load(Ordering::SeqCst) >= 5);
}
#[tokio::test]
async fn ignores_range_and_falls_back_to_streaming() {
    let (url, server) = start(false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let result = download(
        request(vec![url], dir.path()),
        CancellationToken::new(),
        progress().0,
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
}
#[tokio::test]
async fn checksum_mismatch_never_publishes_output() {
    let (url, _) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(vec![url], dir.path());
    req.options.checksum = Some(format!("md5={}", "0".repeat(32)));
    let err = download(req, CancellationToken::new(), progress().0)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"));
    assert!(!dir.path().join("result.bin").exists());
}
#[tokio::test]
async fn failed_mirror_falls_back() {
    let (url, server) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let result = download(
        request(vec!["http://127.0.0.1:1/missing".into(), url], dir.path()),
        CancellationToken::new(),
        progress().0,
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
}
#[tokio::test]
async fn cancel_then_resume_and_changed_validator_restart() {
    let (url, server) = start(true, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(vec![url], dir.path());
    req.options.continue_download = true;
    for changed in [false, true] {
        let cancel = CancellationToken::new();
        let (tx, mut rx) = progress();
        let handle = tokio::spawn(download(req.clone(), cancel.clone(), tx));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                rx.changed().await.unwrap();
                if rx.borrow().completed_length >= 4096 {
                    cancel.cancel();
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(handle.await.unwrap().is_err());
        assert!(dir.path().join("result.bin.ariai").exists());
        let checkpoint: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(dir.path().join("result.bin.ariai"))
                .await
                .unwrap(),
        )
        .unwrap();
        let saved: Vec<u64> = checkpoint["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|size| size.as_u64().unwrap())
            .collect();
        assert!(saved.iter().sum::<u64>() >= 4096);
        if changed {
            *server.etag.lock().unwrap() = "\"v2\"".into();
        }
        server.seen.lock().unwrap().clear();
        let result = download(req.clone(), CancellationToken::new(), progress().0)
            .await
            .unwrap();
        let seen = server.seen.lock().unwrap().clone();
        if changed {
            assert!(
                seen.contains(&(0, 32767)),
                "changed validator must restart first segment: {seen:?}"
            );
        } else {
            for (i, size) in saved.iter().copied().enumerate() {
                let start = i * 32768;
                let end = start + 32767;
                if size < 32768 {
                    assert!(
                        seen.contains(&(start + size as usize, end)),
                        "segment {i} must resume at saved offset {size}: {seen:?}"
                    );
                } else {
                    assert!(
                        !seen.contains(&(start, end)),
                        "completed segment {i} must not be fetched again: {seen:?}"
                    );
                }
            }
        }
        assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
        tokio::fs::remove_file(dir.path().join("result.bin"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn overall_limit_covers_parallel_ranged_downloads() {
    use ariai::{
        manager::DownloadManager,
        model::{DownloadStatus, ManagerOptions},
    };
    let (url, server) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 2,
        max_overall_download_limit: 128 * 1024,
        ..Default::default()
    })
    .unwrap();
    let started = std::time::Instant::now();
    for i in 0..2 {
        let mut req = request(vec![url.clone()], dir.path());
        req.options.out = Some(format!("range-{i}"));
        manager.add(req).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(8), manager.wait_idle())
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        manager
            .list()
            .await
            .iter()
            .all(|s| s.status == DownloadStatus::Complete)
    );
    assert!(server.requests.load(Ordering::SeqCst) >= 10);
    assert!(
        elapsed >= Duration::from_millis(1800),
        "ranged transfers exceeded aggregate limit: {elapsed:?}"
    );
    for i in 0..2 {
        assert_eq!(
            tokio::fs::read(dir.path().join(format!("range-{i}")))
                .await
                .unwrap(),
            *server.bytes
        );
    }
    println!(
        "two ranged transfers: {} bytes in {:?}",
        2 * server.bytes.len(),
        elapsed
    );
}

#[tokio::test]
async fn resumes_official_format_completed_pieces_and_inflight_chunks() {
    let (url, server) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("result.bin");
    let mut data = vec![0; server.bytes.len()];
    data[..32768].copy_from_slice(&server.bytes[..32768]);
    data[32768..49152].copy_from_slice(&server.bytes[32768..49152]);
    data[65536..98304].copy_from_slice(&server.bytes[65536..98304]);
    tokio::fs::write(&target, data).await.unwrap();
    // aria2 version 1, HTTP, four 32 KiB pieces, pieces 0 and 2 complete;
    // piece 1 has a 16 KiB in-flight block, which is also retained.
    let mut control = Vec::new();
    control.extend_from_slice(&1u16.to_be_bytes());
    control.extend_from_slice(&0u32.to_be_bytes());
    control.extend_from_slice(&0u32.to_be_bytes());
    control.extend_from_slice(&32768u32.to_be_bytes());
    control.extend_from_slice(&131072u64.to_be_bytes());
    control.extend_from_slice(&0u64.to_be_bytes());
    control.extend_from_slice(&1u32.to_be_bytes());
    control.push(0xa0);
    control.extend_from_slice(&1u32.to_be_bytes());
    control.extend_from_slice(&1u32.to_be_bytes());
    control.extend_from_slice(&32768u32.to_be_bytes());
    control.extend_from_slice(&1u32.to_be_bytes());
    control.push(0x80);
    tokio::fs::write(dir.path().join("result.bin.aria2"), control)
        .await
        .unwrap();
    let mut req = request(vec![url], dir.path());
    req.options.continue_download = true;
    let result = download(req, CancellationToken::new(), progress().0)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
    let seen = server.seen.lock().unwrap().clone();
    assert!(
        seen.contains(&(49152, 65535)) && seen.contains(&(98304, 131071)),
        "{seen:?}"
    );
    assert!(
        !seen.contains(&(0, 32767)) && !seen.contains(&(65536, 98303)),
        "{seen:?}"
    );
    assert!(!dir.path().join("result.bin.aria2").exists());
}

#[tokio::test]
async fn invalid_aria2_does_not_modify_existing_data() {
    let (url, _) = start(true, false).await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("result.bin");
    tokio::fs::write(&target, b"original data").await.unwrap();
    tokio::fs::write(dir.path().join("result.bin.aria2"), b"invalid")
        .await
        .unwrap();
    let mut req = request(vec![url], dir.path());
    req.options.continue_download = true;
    assert!(
        download(req, CancellationToken::new(), progress().0)
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"original data");
    assert_eq!(
        tokio::fs::read(dir.path().join("result.bin.aria2"))
            .await
            .unwrap(),
        b"invalid"
    );
}

#[tokio::test]
#[ignore = "requires ARIA2C pointing to the official aria2 executable"]
async fn official_aria2_bidirectional_resume() {
    let aria2 = std::env::var_os("ARIA2C").expect("set ARIA2C to official aria2c");
    let (url, server) = start(true, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(vec![url.clone()], dir.path());
    req.options.continue_download = true;
    let cancel = CancellationToken::new();
    let (tx, mut rx) = progress();
    let task = tokio::spawn(download(req.clone(), cancel.clone(), tx));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            rx.changed().await.unwrap();
            if rx.borrow().completed_length >= 70_000 {
                cancel.cancel();
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(task.await.unwrap().is_err());
    let path = dir.path().join("result.bin.aria2");
    let saved = ariai::control::Control::parse(&tokio::fs::read(&path).await.unwrap()).unwrap();
    assert!(saved.partial.iter().flatten().any(|b| *b));
    server.seen.lock().unwrap().clear();
    let output = tokio::process::Command::new(&aria2)
        .args([
            "--no-conf",
            "--continue=true",
            "--file-allocation=none",
            "--summary-interval=0",
            "--dir",
        ])
        .arg(dir.path())
        .args(["--out", "result.bin", &url])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("result.bin"))
            .await
            .unwrap(),
        *server.bytes
    );
    assert!(
        server.seen.lock().unwrap().iter().any(|(a, _)| *a > 0),
        "aria2 did not issue a resumed range"
    );

    let other = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(&aria2)
        .args([
            "--no-conf",
            "--file-allocation=none",
            "--auto-save-interval=1",
            "--max-download-limit=16K",
            "--summary-interval=0",
            "--dir",
        ])
        .arg(other.path())
        .args(["--out", "result.bin", &url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let control = other.path().join("result.bin.aria2");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(bytes) = tokio::fs::read(&control).await
                && let Ok(saved) = ariai::control::Control::parse(&bytes)
                && saved.partial.iter().flatten().any(|b| *b)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    server.seen.lock().unwrap().clear();
    req.options.dir = other.path().to_path_buf();
    let result = download(req, CancellationToken::new(), progress().0)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), *server.bytes);
    assert!(
        server.seen.lock().unwrap().iter().any(|(a, _)| *a > 0),
        "ariai did not issue a resumed range"
    );
}
