//! Native BitTorrent downloads. `out` is ignored for torrents; metadata names files.
//! By default seeding stops immediately after verification (`seed_time_secs = 0`).
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use librqbit::{
    AddTorrent, AddTorrentOptions, DhtSessionConfig, ListenerOptions, Session, SessionOptions,
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
    ensure!(
        options.proxy.is_none(),
        "proxy is not supported for BitTorrent"
    );
    ensure!(
        options.max_download_limit == 0,
        "max-download-limit is not supported for BitTorrent"
    );
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
    let listen = Some(ListenerOptions {
        listen_addr: ([0, 0, 0, 0], options.bt_listen_port.unwrap_or(0)).into(),
        ipv4_only: true,
        ..Default::default()
    });
    let session_options = SessionOptions {
        dht: options.bt_enable_dht.then(|| DhtSessionConfig {
            persistence: None,
            ..Default::default()
        }),
        listen,
        trackers,
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
        loop {
            tokio::select! {
                _ = cancel.cancelled() => bail!("download cancelled"),
                result = handle.wait_until_completed() => { result?; break; }
                _ = interval.tick() => {
                    let stats = handle.stats();
                    for (file, completed) in files.iter_mut().zip(&stats.file_progress) { file.completed_length = *completed; }
                    let completed_length = files.iter().filter(|file| file.selected).map(|file| file.completed_length).sum();
                    progress.send_replace(DownloadProgress { total_length: Some(total_length), completed_length, download_speed: stats.live.map_or(0, |s| s.download_speed.as_bytes()), path: Some(path.clone()), files: files.clone() });
                }
            }
        }
        let stats = handle.stats();
        for (file, completed) in files.iter_mut().zip(&stats.file_progress) { file.completed_length = *completed; }
        let completed_length = files.iter().filter(|file| file.selected).map(|file| file.completed_length).sum();
        progress.send_replace(DownloadProgress { total_length: Some(total_length), completed_length, download_speed: 0, path: Some(path.clone()), files: files.clone() });
        if options.seed_time_secs > 0 {
            tokio::select! {
                _ = cancel.cancelled() => bail!("download cancelled"),
                _ = tokio::time::sleep(Duration::from_secs(options.seed_time_secs)) => {}
            }
        }
        Ok(DownloadResult { path, total_length, files })
    }.await;
    session.stop().await;
    result
}
