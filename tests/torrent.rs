use std::time::Duration;

use ariai::{
    model::{DownloadOptions, DownloadProgress, DownloadRequest},
    torrent,
};
use librqbit::{AddTorrent, AddTorrentOptions, ListenerOptions, Session, SessionOptions};
use sha1::{Digest, Sha1};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

fn fixture() -> (Vec<u8>, String, Vec<u8>, Vec<u8>) {
    let a = vec![b'a'; 16384];
    let b = vec![b'b'; 16384];
    let mut info = b"d5:filesld6:lengthi16384e4:pathl5:a.txteed6:lengthi16384e4:pathl5:b.txteee4:name6:bundle12:piece lengthi16384e6:pieces40:".to_vec();
    info.extend(Sha1::digest(&a));
    info.extend(Sha1::digest(&b));
    info.push(b'e');
    let hash = hex::encode(Sha1::digest(&info));
    let mut torrent = b"d4:info".to_vec();
    torrent.extend(info);
    torrent.push(b'e');
    (torrent, hash, a, b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_selected_file_and_magnet_from_loopback_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = tmp.path().join("seed");
    tokio::fs::create_dir_all(seed.join("bundle"))
        .await
        .unwrap();
    let (metadata, hash, a, b) = fixture();
    tokio::fs::write(seed.join("bundle/a.txt"), &a)
        .await
        .unwrap();
    tokio::fs::write(seed.join("bundle/b.txt"), &b)
        .await
        .unwrap();
    let session = Session::new_with_opts(
        seed,
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: ([127, 0, 0, 1], 0).into(),
                ipv4_only: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(metadata.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let peer = session.listen_addr().unwrap().to_string();
    let metadata_path = tmp.path().join("fixture.torrent");
    tokio::fs::write(&metadata_path, &metadata).await.unwrap();
    let web_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_addr = web_listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/fixture.torrent",
        axum::routing::get(move || {
            let metadata = metadata.clone();
            async move { metadata }
        }),
    );
    let web_task = tokio::spawn(async move { axum::serve(web_listener, app).await.unwrap() });
    for (i, uri) in [
        metadata_path.to_string_lossy().to_string(),
        format!("magnet:?xt=urn:btih:{hash}"),
        url::Url::from_file_path(&metadata_path)
            .unwrap()
            .to_string(),
        format!("http://{web_addr}/fixture.torrent"),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = tmp.path().join(format!("download-{i}"));
        let options = DownloadOptions {
            dir: dir.clone(),
            bt_enable_dht: false,
            bt_initial_peers: vec![peer.clone()],
            select_file: Some("2".into()),
            seed_time_secs: if i == 3 { 1 } else { 0 },
            ..Default::default()
        };
        let (tx, rx) = watch::channel(DownloadProgress::default());
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            torrent::download(
                DownloadRequest {
                    uris: vec![uri],
                    options,
                },
                CancellationToken::new(),
                tx,
            ),
        )
        .await
        .expect("loopback transfer timed out")
        .unwrap();
        assert_eq!(result.path, dir.join("bundle"));
        assert_eq!(result.total_length, b.len() as u64);
        assert_eq!(tokio::fs::read(result.path.join("b.txt")).await.unwrap(), b);
        assert_eq!(rx.borrow().completed_length, b.len() as u64);
        assert_eq!(result.files.len(), 2);
        assert!(!result.files[0].selected);
        assert_eq!(result.files[1].index, 2);
        assert!(result.files[1].selected);
        assert_eq!(result.files[1].path, dir.join("bundle/b.txt"));
        assert_eq!(result.files[1].length, b.len() as u64);
        assert_eq!(result.files[1].completed_length, b.len() as u64);
        assert_eq!(rx.borrow().files[1].completed_length, b.len() as u64);
        if i == 3 {
            assert!(started.elapsed() >= Duration::from_secs(1));
        }
    }
    web_task.abort();

    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let seed_port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let options = DownloadOptions {
        dir: tmp.path().join("reseeder"),
        bt_enable_dht: false,
        bt_initial_peers: vec![peer],
        bt_listen_port: Some(seed_port),
        select_file: Some("2".into()),
        seed_time_secs: 30,
        ..Default::default()
    };
    let token = CancellationToken::new();
    let task_token = token.clone();
    let (tx, mut rx) = watch::channel(DownloadProgress::default());
    let torrent_file = metadata_path.to_string_lossy().to_string();
    let task = tokio::spawn(async move {
        torrent::download(
            DownloadRequest {
                uris: vec![torrent_file],
                options,
            },
            task_token,
            tx,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(20), async {
        while rx.borrow().completed_length < b.len() as u64 {
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("reseeder did not finish downloading");
    let options = DownloadOptions {
        dir: tmp.path().join("second-peer"),
        bt_enable_dht: false,
        bt_initial_peers: vec![format!("127.0.0.1:{seed_port}")],
        select_file: Some("2".into()),
        ..Default::default()
    };
    let (tx, _) = watch::channel(DownloadProgress::default());
    let second = tokio::time::timeout(
        Duration::from_secs(20),
        torrent::download(
            DownloadRequest {
                uris: vec![metadata_path.to_string_lossy().to_string()],
                options,
            },
            CancellationToken::new(),
            tx,
        ),
    )
    .await
    .expect("second peer did not receive seeded data")
    .unwrap();
    assert_eq!(tokio::fs::read(second.path.join("b.txt")).await.unwrap(), b);
    token.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_metadata_selection_and_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let metadata_path = tmp.path().join("bad.torrent");
    tokio::fs::write(&metadata_path, b"invalid").await.unwrap();
    let opts = DownloadOptions {
        dir: tmp.path().join("out"),
        bt_enable_dht: false,
        ..Default::default()
    };
    let request = |uri: String, options: DownloadOptions| DownloadRequest {
        uris: vec![uri],
        options,
    };
    let (tx, _) = watch::channel(DownloadProgress::default());
    let mirrors = DownloadRequest {
        uris: vec![
            metadata_path.to_string_lossy().to_string(),
            "http://127.0.0.1/alternate.torrent".into(),
        ],
        options: opts.clone(),
    };
    let error = torrent::download(mirrors, CancellationToken::new(), tx.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("mirror lists are unsupported"));
    assert!(
        torrent::download(
            request(metadata_path.to_string_lossy().to_string(), opts.clone()),
            CancellationToken::new(),
            tx.clone()
        )
        .await
        .is_err()
    );
    let (metadata, hash, _, _) = fixture();
    tokio::fs::write(&metadata_path, metadata).await.unwrap();
    let mut invalid_selection = opts.clone();
    invalid_selection.select_file = Some("3".into());
    assert!(
        torrent::download(
            request(
                metadata_path.to_string_lossy().to_string(),
                invalid_selection
            ),
            CancellationToken::new(),
            tx.clone()
        )
        .await
        .is_err()
    );
    let stalled_peer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_addr = stalled_peer.local_addr().unwrap();
    let stalled_task = tokio::spawn(async move {
        let (_socket, _) = stalled_peer.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let mut opts = opts;
    opts.bt_initial_peers = vec![stalled_addr.to_string()];
    opts.bt_listen_port = Some(port);
    let token = CancellationToken::new();
    let child = token.clone();
    let task = tokio::spawn(async move {
        torrent::download(
            request(format!("magnet:?xt=urn:btih:{hash}"), opts),
            child,
            tx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    stalled_task.abort();
}
