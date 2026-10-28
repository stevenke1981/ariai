use ariai::{
    engine::download,
    model::{DownloadOptions, DownloadProgress, DownloadRequest, PieceHashes},
};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Response, StatusCode},
    routing::get,
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::{net::TcpListener, sync::watch};
use tokio_util::sync::CancellationToken;

const GOOD: &[u8] = b"abcdefghij";
const BAD: &[u8] = b"abcdXXXXij";

#[tokio::test]
async fn repairs_only_damaged_or_missing_pieces_and_skips_verified_files() {
    let hits = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let app = Router::new();
    let mut app = app;
    for route in ["bad", "good", "wrong-range", "short"] {
        let hits = hits.clone();
        app = app.route(
            &format!("/{route}"),
            get(move |headers: HeaderMap| {
                let hits = hits.clone();
                async move {
                    let range = headers.get("range").unwrap().to_str().unwrap();
                    hits.lock()
                        .unwrap()
                        .push((route.to_owned(), range.to_owned()));
                    let (start, end) = range
                        .strip_prefix("bytes=")
                        .unwrap()
                        .split_once('-')
                        .unwrap();
                    let (start, end) = (
                        start.parse::<usize>().unwrap(),
                        end.parse::<usize>().unwrap(),
                    );
                    let data = match route {
                        "good" => GOOD,
                        "short" => &GOOD[..8],
                        _ => BAD,
                    };
                    if end >= data.len() {
                        return Response::builder()
                            .status(StatusCode::RANGE_NOT_SATISFIABLE)
                            .body(Body::empty())
                            .unwrap();
                    }
                    Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("etag", "\"fixed\"")
                        .header(
                            "content-range",
                            if route == "wrong-range" {
                                "bytes 0-3/10".into()
                            } else {
                                format!("bytes {start}-{end}/{}", data.len())
                            },
                        )
                        .body(Body::from(data[start..=end].to_vec()))
                        .unwrap()
                }
            }),
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let options = DownloadOptions {
        dir: dir.path().to_owned(),
        out: Some("data.bin".into()),
        expected_length: Some(GOOD.len() as u64),
        checksum: Some(format!("sha-256={}", hex::encode(Sha256::digest(GOOD)))),
        piece_hashes: Some(PieceHashes {
            algorithm: "sha-256".into(),
            length: 4,
            hashes: GOOD
                .chunks(4)
                .map(|b| hex::encode(Sha256::digest(b)))
                .collect(),
        }),
        check_integrity: true,
        split: 1,
        min_split_size: 1,
        max_tries: 1,
        retry_wait_secs: 0,
        ..Default::default()
    };
    let target = dir.path().join("data.bin");
    let request = DownloadRequest {
        uris: vec![
            format!("http://{address}/bad"),
            format!("http://{address}/good"),
        ],
        options: options.clone(),
    };
    for (initial, expected) in [
        (BAD, vec![("bad", "bytes=4-7"), ("good", "bytes=4-7")]),
        (
            &GOOD[..4],
            vec![
                ("bad", "bytes=4-7"),
                ("good", "bytes=4-7"),
                ("bad", "bytes=8-9"),
            ],
        ),
        (GOOD, vec![]),
    ] {
        tokio::fs::write(&target, initial).await.unwrap();
        hits.lock().unwrap().clear();
        let (progress, rx) = watch::channel(DownloadProgress::default());
        let result = download(request.clone(), CancellationToken::new(), progress)
            .await
            .unwrap();
        assert_eq!(result.path, target);
        assert_eq!(tokio::fs::read(&target).await.unwrap(), GOOD);
        assert_eq!(rx.borrow().completed_length, 10);
        assert_eq!(rx.borrow().path.as_ref(), Some(&target));
        let actual = hits.lock().unwrap().clone();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(r, s)| (r.to_owned(), s.to_owned()))
            .collect();
        assert_eq!(actual, expected);
        assert!(!dir.path().join("data.bin.1").exists());
    }
    // Also repair bytes corrupted during a fresh segmented transfer.
    tokio::fs::remove_file(&target).await.unwrap();
    hits.lock().unwrap().clear();
    let (progress, _) = watch::channel(DownloadProgress::default());
    download(request.clone(), CancellationToken::new(), progress)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&target).await.unwrap(), GOOD);
    assert_eq!(
        hits.lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r == "good")
            .cloned()
            .collect::<Vec<_>>(),
        vec![("good".into(), "bytes=4-7".into())]
    );

    // Repair can extend a fresh assembly from a truncated first mirror.
    tokio::fs::remove_file(&target).await.unwrap();
    hits.lock().unwrap().clear();
    let mut truncated = request.clone();
    truncated.uris = vec![
        format!("http://{address}/short"),
        format!("http://{address}/good"),
    ];
    let (progress, rx) = watch::channel(DownloadProgress::default());
    let result = download(truncated, CancellationToken::new(), progress)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&target).await.unwrap(), GOOD);
    assert_eq!(result.total_length, GOOD.len() as u64);
    assert_eq!(rx.borrow().total_length, Some(GOOD.len() as u64));
    assert_eq!(rx.borrow().completed_length, GOOD.len() as u64);
    assert_eq!(
        hits.lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r == "good")
            .cloned()
            .collect::<Vec<_>>(),
        vec![("good".into(), "bytes=8-9".into())]
    );

    // A malformed range cannot be accepted, and valid local pieces survive.
    tokio::fs::write(&target, BAD).await.unwrap();
    let mut invalid = request.clone();
    invalid.uris = vec![format!("http://{address}/wrong-range")];
    let (progress, _) = watch::channel(DownloadProgress::default());
    assert!(
        download(invalid, CancellationToken::new(), progress)
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&target).await.unwrap(), BAD);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (progress, _) = watch::channel(DownloadProgress::default());
    assert!(download(request, cancel, progress).await.is_err());
    assert_eq!(tokio::fs::read(&target).await.unwrap(), BAD);
    server.abort();
}
