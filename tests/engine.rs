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
