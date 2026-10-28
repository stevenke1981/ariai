//! Native BitTorrent downloads. `out` is ignored for torrents; metadata names files.
//! By default seeding stops immediately after verification (`seed_time_secs = 0`).
use std::{net::SocketAddr, num::NonZeroU32, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use librqbit::{
    AddTorrent, AddTorrentOptions, ConnectionOptions, DhtSessionConfig, ListenerOptions, Session,
    SessionOptions, limits::LimitsConfig,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::model::{
    DownloadFile, DownloadProgress, DownloadRequest, DownloadResult, selected_indices,
};

pub async fn download(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<DownloadResult> {
    request.validate()?;
    ensure!(
        request.uris.len() == 1,
        "BitTorrent accepts one metadata source; torrent mirror lists are unsupported"
    );
    let options = &request.options;
    let proxied = options.proxy.is_some();
    if let Some(proxy) = &options.proxy {
        let url = url::Url::parse(proxy)?;
        ensure!(
            url.scheme() == "socks5" && url.host_str().is_some() && url.port().is_some(),
            "BitTorrent proxy requires a socks5://host:port URL"
        );
        ensure!(
            !options.bt_initial_peers.is_empty(),
            "BitTorrent proxy requires --bt-initial-peer; trackers and DHT are disabled in proxy mode"
        );
        ensure!(
            options.bt_trackers.is_empty(),
            "BitTorrent trackers are not supported in proxy mode"
        );
        ensure!(
            options.bt_listen_port.is_none(),
            "BitTorrent incoming connections are disabled in proxy mode"
        );
    }
    // librqbit uses a token bucket whose burst equals its per-second rate.
    // Its standard requests are 16 KiB, and a smaller burst rejects a request.
    let limit = |value: u64, name: &str| -> Result<Option<NonZeroU32>> {
        if value == 0 {
            return Ok(None);
        }
        ensure!(
            (16_384..=u32::MAX as u64).contains(&value),
            "BitTorrent {name} must be 0 (unlimited) or between 16384 and {} bytes/s",
            u32::MAX
        );
        Ok(NonZeroU32::new(value as u32))
    };
    let ratelimits = LimitsConfig {
        download_bps: limit(options.max_download_limit, "max-download-limit")?,
        upload_bps: limit(options.max_upload_limit, "max-upload-limit")?,
    };
    ensure!(
        options.checksum.is_none()
            && options.expected_length.is_none()
            && options.piece_hashes.is_none(),
        "external checksum and Metalink piece options are not supported for BitTorrent"
    );
    let selected = options
        .select_file
        .as_deref()
        .map(|s| selected_indices(Some(s), None))
        .transpose()?;
    let peers: Vec<SocketAddr> = options
        .bt_initial_peers
        .iter()
        .map(|s| {
            s.parse()
                .with_context(|| format!("invalid BitTorrent peer: {s}"))
        })
        .collect::<Result<_>>()?;
    let trackers = options
        .bt_trackers
        .iter()
        .map(|t| url::Url::parse(t).with_context(|| format!("invalid BitTorrent tracker: {t}")))
        .collect::<Result<_>>()?;
    let session_cancel = CancellationToken::new();
    let listen = (!proxied).then(|| ListenerOptions {
        listen_addr: ([0, 0, 0, 0], options.bt_listen_port.unwrap_or(0)).into(),
        ipv4_only: true,
        ..Default::default()
    });
    let session_options = SessionOptions {
        dht: (options.bt_enable_dht && !proxied).then(|| DhtSessionConfig {
            persistence: None,
            ..Default::default()
        }),
        listen,
        connect: options.proxy.as_ref().map(|proxy| ConnectionOptions {
            proxy_url: Some(proxy.clone()),
            ..Default::default()
        }),
        disable_trackers: proxied,
        trackers,
        ratelimits,
        cancellation_token: Some(session_cancel.clone()),
        disable_local_service_discovery: true,
        ..Default::default()
    };
    let session = tokio::select! {
        _ = cancel.cancelled() => { session_cancel.cancel(); bail!("download cancelled") },
        result = Session::new_with_opts(options.dir.clone(), session_options) => result?,
    };
    let result = async {
        let uri = &request.uris[0];
        let add = if uri.starts_with("file:") {
            let url = url::Url::parse(uri)?;
            let path = url.to_file_path().map_err(|_| anyhow::anyhow!("invalid torrent file URL"))?;
            AddTorrent::from_bytes(tokio::fs::read(path).await?)
        } else if uri.starts_with("magnet:") || uri.starts_with("http://") || uri.starts_with("https://") {
            AddTorrent::from_url(uri.as_str())
        } else {
            AddTorrent::from_bytes(tokio::fs::read(uri).await?)
        };
        let add_options = AddTorrentOptions {
            only_files: selected.clone(),
            overwrite: options.continue_download || options.allow_overwrite,
            initial_peers: (!peers.is_empty()).then_some(peers),
            ..Default::default()
        };
        let handle = tokio::select! {
            _ = cancel.cancelled() => bail!("download cancelled"),
            result = session.add_torrent(add, Some(add_options)) => result?.into_handle().context("torrent was not added")?,
        };
        let selected = handle.only_files();
        let (path, total_length, mut files) = handle.with_metadata(|metadata| {
            let all = &metadata.file_infos;
            let indexes = selected.clone().unwrap_or_else(|| (0..all.len()).collect());
            let total = indexes.iter().map(|&i| all[i].len).sum();
            let path = if all.len() == 1 { handle.output_folder().join(&all[0].relative_filename) } else { PathBuf::from(handle.output_folder()) };
            let files = all.iter().enumerate().map(|(index, file)| DownloadFile {
                index: index + 1,
                path: handle.output_folder().join(&file.relative_filename),
                length: file.len,
                completed_length: 0,
                selected: indexes.contains(&index),
            }).collect::<Vec<_>>();
            (path, total, files)
        })?;
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        let mut completed_at = None;
        let mut publish = || {
            let stats = handle.stats();
            for (file, completed) in files.iter_mut().zip(&stats.file_progress) {
                file.completed_length = *completed;
            }
            let completed_length = files.iter().filter(|file| file.selected).map(|file| file.completed_length).sum();
            let (download_speed, upload_speed, connections) = stats.live.as_ref().map_or((0, 0, 0), |live| {
                (live.download_speed.as_bytes(), live.upload_speed.as_bytes(), u64::from(live.snapshot.peer_stats.live))
            });
            progress.send_replace(DownloadProgress {
                total_length: Some(total_length), completed_length, download_speed,
                upload_length: stats.uploaded_bytes, upload_speed, connections,
                path: Some(path.clone()), files: files.clone(),
            });
            stats.uploaded_bytes
        };
        loop {
            let cancelled = tokio::select! {
                _ = cancel.cancelled() => true,
                result = handle.wait_until_completed(), if completed_at.is_none() => {
                    result?;
                    completed_at = Some(tokio::time::Instant::now());
                    false
                }
                _ = interval.tick() => false,
            };
            let uploaded = publish();
            if cancelled {
                bail!("download cancelled");
            }
            if let Some(started) = completed_at {
                let explicit_no_seed = options.seed_time_explicit && options.seed_time_secs == 0;
                let default_no_seed = options.seed_time_secs == 0 && options.seed_ratio.is_none();
                let time_reached = options.seed_time_secs > 0 && started.elapsed() >= Duration::from_secs(options.seed_time_secs);
                let ratio_reached = options.seed_ratio.is_some_and(|ratio| ratio > 0.0 && uploaded as f64 >= total_length as f64 * ratio);
                if explicit_no_seed || default_no_seed || time_reached || ratio_reached {
                    break;
                }
            }
        }
        drop(publish);
        Ok(DownloadResult { path, total_length, files })
    }.await;
    session.stop().await;
    result
}
