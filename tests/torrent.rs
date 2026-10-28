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
async fn socks_proxy_carries_metadata_and_peer_traffic() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let tmp = tempfile::tempdir().unwrap();
    let (metadata, _, a, b) = fixture();
    let seed_dir = tmp.path().join("seed");
    tokio::fs::create_dir_all(seed_dir.join("bundle"))
        .await
        .unwrap();
    tokio::fs::write(seed_dir.join("bundle/a.txt"), &a)
        .await
        .unwrap();
    tokio::fs::write(seed_dir.join("bundle/b.txt"), &b)
        .await
        .unwrap();
    let seed = Session::new_with_opts(
        seed_dir,
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
    let handle = seed
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
    let peer = seed.listen_addr().unwrap();
    let web = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_addr = web.local_addr().unwrap();
    let web_task = tokio::spawn(async move {
        axum::serve(
            web,
            axum::Router::new().route(
                "/file.torrent",
                axum::routing::get(move || {
                    let data = metadata.clone();
                    async move { data }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let peer_connections = Arc::new(AtomicUsize::new(0));
    let metadata_connections = Arc::new(AtomicUsize::new(0));
    let pc = peer_connections.clone();
    let mc = metadata_connections.clone();
    let proxy_task = tokio::spawn(async move {
        loop {
            let (mut inbound, _) = listener.accept().await.unwrap();
            let pc = pc.clone();
            let mc = mc.clone();
            tokio::spawn(async move {
                let mut greeting = [0; 2];
                inbound.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting[0], 5);
                let mut methods = vec![0; greeting[1] as usize];
                inbound.read_exact(&mut methods).await.unwrap();
                assert!(methods.contains(&0));
                inbound.write_all(&[5, 0]).await.unwrap();
                let mut head = [0; 4];
                inbound.read_exact(&mut head).await.unwrap();
                assert_eq!(&head[..3], &[5, 1, 0]);
                let host = match head[3] {
                    1 => {
                        let mut ip = [0; 4];
                        inbound.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    3 => {
                        let len = inbound.read_u8().await.unwrap();
                        let mut name = vec![0; len as usize];
                        inbound.read_exact(&mut name).await.unwrap();
                        String::from_utf8(name).unwrap()
                    }
                    other => panic!("unexpected address type {other}"),
                };
                let port = inbound.read_u16().await.unwrap();
                if port == peer.port() {
                    pc.fetch_add(1, Ordering::SeqCst);
                }
                if port == web_addr.port() {
                    mc.fetch_add(1, Ordering::SeqCst);
                }
                let mut outbound = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .unwrap();
                inbound
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    let (tx, _) = watch::channel(DownloadProgress::default());
    let result = tokio::time::timeout(
        Duration::from_secs(25),
        torrent::download(
            DownloadRequest {
                uris: vec![format!("http://{web_addr}/file.torrent")],
                options: DownloadOptions {
                    dir: tmp.path().join("download"),
                    proxy: Some(format!("socks5://{addr}")),
                    bt_initial_peers: vec![peer.to_string()],
                    ..Default::default()
                },
            },
            CancellationToken::new(),
            tx,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tokio::fs::read(result.path.join("a.txt")).await.unwrap(), a);
    assert_eq!(tokio::fs::read(result.path.join("b.txt")).await.unwrap(), b);
    assert!(peer_connections.load(Ordering::SeqCst) > 0);
    assert!(metadata_connections.load(Ordering::SeqCst) > 0);
    proxy_task.abort();
    web_task.abort();
    seed.stop().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measured_seeding_ratio_and_limits_with_real_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = vec![b'x'; 8 * 16384];
    let mut info = b"d6:lengthi131072e4:name7:big.bin12:piece lengthi16384e6:pieces160:".to_vec();
    for chunk in bytes.chunks(16384) {
        info.extend(Sha1::digest(chunk));
    }
    info.push(b'e');
    let mut metadata = b"d4:info".to_vec();
    metadata.extend(info);
    metadata.push(b'e');
    let torrent_path = tmp.path().join("big.torrent");
    tokio::fs::write(&torrent_path, metadata).await.unwrap();
    let seed_dir = tmp.path().join("seed");
    tokio::fs::create_dir_all(&seed_dir).await.unwrap();
    tokio::fs::write(seed_dir.join("big.bin"), &bytes)
        .await
        .unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let (tx, mut rx) = watch::channel(DownloadProgress::default());
    let seed_request = DownloadRequest {
        uris: vec![torrent_path.to_string_lossy().to_string()],
        options: DownloadOptions {
            dir: seed_dir,
            bt_enable_dht: false,
            bt_listen_port: Some(port),
            allow_overwrite: true,
            max_upload_limit: 16384,
            seed_ratio: Some(1.0),
            seed_time_secs: 30,
            ..Default::default()
        },
    };
    let seed_task = tokio::spawn(torrent::download(
        seed_request,
        CancellationToken::new(),
        tx,
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        while rx.borrow().completed_length != bytes.len() as u64 {
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("seed did not initialize");
    let monitor = tokio::spawn(async move {
        let mut peak_connections = 0;
        let mut peak_upload_speed = 0;
        let mut uploaded = 0;
        loop {
            {
                let p = rx.borrow_and_update();
                peak_connections = peak_connections.max(p.connections);
                peak_upload_speed = peak_upload_speed.max(p.upload_speed);
                uploaded = uploaded.max(p.upload_length);
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
        (peak_connections, peak_upload_speed, uploaded)
    });
    let (tx, _) = watch::channel(DownloadProgress::default());
    let start = tokio::time::Instant::now();
    let peer = tokio::time::timeout(
        Duration::from_secs(25),
        torrent::download(
            DownloadRequest {
                uris: vec![torrent_path.to_string_lossy().to_string()],
                options: DownloadOptions {
                    dir: tmp.path().join("peer"),
                    bt_enable_dht: false,
                    bt_initial_peers: vec![format!("127.0.0.1:{port}")],
                    ..Default::default()
                },
            },
            CancellationToken::new(),
            tx,
        ),
    )
    .await
    .expect("peer timed out")
    .unwrap();
    assert_eq!(tokio::fs::read(peer.path).await.unwrap(), bytes);
    assert!(
        start.elapsed() >= Duration::from_secs(4),
        "limits did not throttle transfer: {:?}",
        start.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(5), seed_task)
        .await
        .expect("seed ratio did not stop")
        .unwrap()
        .unwrap();
    let (connections, speed, uploaded) = monitor.await.unwrap();
    assert!(connections > 0, "no live connection reported");
    assert!(speed > 0, "no upload speed reported");
    assert!(uploaded >= bytes.len() as u64, "reported upload {uploaded}");
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seed_zero_ratio_waits_and_explicit_zero_time_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let (metadata, _, a, b) = fixture();
    let torrent_path = tmp.path().join("fixture.torrent");
    tokio::fs::write(&torrent_path, metadata).await.unwrap();
    let dir = tmp.path().join("seed");
    tokio::fs::create_dir_all(dir.join("bundle")).await.unwrap();
    tokio::fs::write(dir.join("bundle/a.txt"), a).await.unwrap();
    tokio::fs::write(dir.join("bundle/b.txt"), b).await.unwrap();
    let mut options = DownloadOptions {
        dir,
        bt_enable_dht: false,
        allow_overwrite: true,
        seed_ratio: Some(0.0),
        ..Default::default()
    };
    let (tx, mut rx) = watch::channel(DownloadProgress::default());
    let cancel = CancellationToken::new();
    let request = DownloadRequest {
        uris: vec![torrent_path.to_string_lossy().to_string()],
        options: options.clone(),
    };
    let mut task = tokio::spawn(torrent::download(request, cancel.clone(), tx));
    tokio::time::timeout(Duration::from_secs(10), async {
        while rx.borrow().completed_length != 32768 {
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(600), &mut task)
            .await
            .is_err()
    );
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    options.seed_time_explicit = true;
    let (tx, _) = watch::channel(DownloadProgress::default());
    let request = DownloadRequest {
        uris: vec![torrent_path.to_string_lossy().to_string()],
        options: options.clone(),
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        torrent::download(request, CancellationToken::new(), tx),
    )
    .await
    .unwrap()
    .unwrap();
    options.seed_time_secs = 1;
    options.seed_ratio = Some(100.0);
    let (tx, _) = watch::channel(DownloadProgress::default());
    let request = DownloadRequest {
        uris: vec![torrent_path.to_string_lossy().to_string()],
        options,
    };
    let start = tokio::time::Instant::now();
    tokio::time::timeout(
        Duration::from_secs(5),
        torrent::download(request, CancellationToken::new(), tx),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(start.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn rejects_unsupported_torrent_rate_boundaries() {
    for (download, upload) in [
        (1, 0),
        (16383, 0),
        (u32::MAX as u64 + 1, 0),
        (0, 1),
        (0, u32::MAX as u64 + 1),
    ] {
        let (tx, _) = watch::channel(DownloadProgress::default());
        let request = DownloadRequest {
            uris: vec!["missing.torrent".into()],
            options: DownloadOptions {
                max_download_limit: download,
                max_upload_limit: upload,
                ..Default::default()
            },
        };
        let err = torrent::download(request, CancellationToken::new(), tx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("between 16384 and"), "{err:#}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovers_seed_through_loopback_http_tracker() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let tmp = tempfile::tempdir().unwrap();
    let seed_dir = tmp.path().join("seed");
    tokio::fs::create_dir_all(seed_dir.join("bundle"))
        .await
        .unwrap();
    let (metadata, _, a, b) = fixture();
    tokio::fs::write(seed_dir.join("bundle/a.txt"), a)
        .await
        .unwrap();
    tokio::fs::write(seed_dir.join("bundle/b.txt"), &b)
        .await
        .unwrap();
    let session = Session::new_with_opts(
        seed_dir,
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
    let seed = session
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
    tokio::time::timeout(Duration::from_secs(10), seed.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let seed_port = session.listen_addr().unwrap().port();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tracker_addr = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let tracker_calls = calls.clone();
    let app = axum::Router::new().route(
        "/announce",
        axum::routing::get(move || {
            let tracker_calls = tracker_calls.clone();
            async move {
                tracker_calls.fetch_add(1, Ordering::SeqCst);
                let mut response = b"d8:intervali60e5:peers6:".to_vec();
                response.extend([127, 0, 0, 1]);
                response.extend(seed_port.to_be_bytes());
                response.push(b'e');
                response
            }
        }),
    );
    let tracker_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let announce = format!("http://{tracker_addr}/announce");
    let mut tracked = format!("d8:announce{}:{}", announce.len(), announce).into_bytes();
    tracked.extend(&metadata[1..]);
    let path = tmp.path().join("tracked.torrent");
    tokio::fs::write(&path, tracked).await.unwrap();
    let (tx, _) = watch::channel(DownloadProgress::default());
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        torrent::download(
            DownloadRequest {
                uris: vec![path.to_string_lossy().to_string()],
                options: DownloadOptions {
                    dir: tmp.path().join("download"),
                    bt_enable_dht: false,
                    select_file: Some("2".into()),
                    ..Default::default()
                },
            },
            CancellationToken::new(),
            tx,
        ),
    )
    .await
    .expect("HTTP tracker transfer timed out")
    .unwrap();
    assert_eq!(tokio::fs::read(result.path.join("b.txt")).await.unwrap(), b);
    assert!(calls.load(Ordering::SeqCst) > 0);
    tracker_task.abort();
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn download_limit_throttles_transfer_from_unrestricted_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = vec![b'y'; 8 * 16384];
    let mut info = b"d6:lengthi131072e4:name7:big.bin12:piece lengthi16384e6:pieces160:".to_vec();
    for chunk in bytes.chunks(16384) {
        info.extend(Sha1::digest(chunk));
    }
    info.push(b'e');
    let mut metadata = b"d4:info".to_vec();
    metadata.extend(info);
    metadata.push(b'e');
    let dir = tmp.path().join("seed");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("big.bin"), &bytes).await.unwrap();
    let session = Session::new_with_opts(
        dir,
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
    let torrent_path = tmp.path().join("big.torrent");
    tokio::fs::write(&torrent_path, metadata).await.unwrap();
    let (tx, _) = watch::channel(DownloadProgress::default());
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        torrent::download(
            DownloadRequest {
                uris: vec![torrent_path.to_string_lossy().to_string()],
                options: DownloadOptions {
                    dir: tmp.path().join("peer"),
                    bt_enable_dht: false,
                    bt_initial_peers: vec![session.listen_addr().unwrap().to_string()],
                    max_download_limit: 32768,
                    ..Default::default()
                },
            },
            CancellationToken::new(),
            tx,
        ),
    )
    .await
    .expect("download-limited peer timed out")
    .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), bytes);
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "download limit did not throttle transfer: {:?}",
        started.elapsed()
    );
    session.stop().await;
}
