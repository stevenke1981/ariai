//! Metalink piece verification and selective HTTP(S) repair.
use crate::{
    http::HttpSession,
    model::{DownloadProgress, DownloadRequest},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::StreamExt;
use reqwest::{Method, StatusCode, header::CONTENT_RANGE};
use sha2::Digest;
use std::{io::SeekFrom, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

fn hasher(algorithm: &str) -> Result<Box<dyn sha2::digest::DynDigest + Send>> {
    Ok(match algorithm.to_ascii_lowercase().as_str() {
        "sha256" | "sha-256" => Box::new(sha2::Sha256::new()),
        "sha1" | "sha-1" => Box::new(sha1::Sha1::new()),
        "md5" => Box::new(md5::Md5::new()),
        _ => bail!("unsupported piece checksum"),
    })
}

/// Identify corrupt or missing pieces without changing the file.
pub async fn bad_pieces(
    path: &Path,
    options: &crate::model::DownloadOptions,
    cancel: &CancellationToken,
) -> Result<(u64, Vec<(usize, u64, u64)>)> {
    let pieces = options
        .piece_hashes
        .as_ref()
        .context("missing piece hashes")?;
    let mut file = tokio::fs::File::open(path).await?;
    let length = options
        .expected_length
        .unwrap_or(file.metadata().await?.len());
    ensure!(
        pieces.length > 0
            && length > 0
            && length.div_ceil(pieces.length) == pieces.hashes.len() as u64,
        "piece hash list does not cover the expected file length"
    );
    let mut buffer = vec![0; 128 * 1024];
    let mut bad = Vec::new();
    for (index, expected) in pieces.hashes.iter().enumerate() {
        let start = index as u64 * pieces.length;
        let size = pieces.length.min(length - start);
        file.seek(SeekFrom::Start(start)).await?;
        let mut remaining = size;
        let mut hash = hasher(&pieces.algorithm)?;
        while remaining > 0 {
            let wanted = remaining.min(buffer.len() as u64) as usize;
            let n = tokio::select! {
                _ = cancel.cancelled() => bail!("download cancelled"),
                n = file.read(&mut buffer[..wanted]) => n?,
            };
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            remaining -= n as u64;
        }
        if remaining != 0 || !hex::encode(hash.finalize()).eq_ignore_ascii_case(expected) {
            bad.push((index, start, size));
        }
    }
    ensure!(!cancel.is_cancelled(), "download cancelled");
    Ok((length, bad))
}

pub async fn repair_file(
    path: &Path,
    request: &DownloadRequest,
    session: &HttpSession,
    cancel: &CancellationToken,
    progress: &watch::Sender<DownloadProgress>,
) -> Result<()> {
    let options = &request.options;
    let Some(pieces) = &options.piece_hashes else {
        return crate::protocols::verify_file(path, options).await;
    };
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await?;
    let length = options
        .expected_length
        .unwrap_or(file.metadata().await?.len());
    ensure!(
        length > 0 && length.div_ceil(pieces.length) == pieces.hashes.len() as u64,
        "piece hash list does not cover the expected file length; truncated files require a Metalink size"
    );
    let mut buffer = vec![0; 128 * 1024];
    let mut bad = Vec::new();
    let mut verified = 0;
    for (index, expected) in pieces.hashes.iter().enumerate() {
        ensure!(!cancel.is_cancelled(), "download cancelled");
        let start = index as u64 * pieces.length;
        let size = pieces.length.min(length - start);
        file.seek(SeekFrom::Start(start)).await?;
        let mut remaining = size;
        let mut hash = hasher(&pieces.algorithm)?;
        while remaining > 0 {
            let wanted = remaining.min(buffer.len() as u64) as usize;
            let read = tokio::select! {
                _ = cancel.cancelled() => bail!("download cancelled"),
                read = file.read(&mut buffer[..wanted]) => read?,
            };
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            remaining -= read as u64;
        }
        if remaining == 0 && hex::encode(hash.finalize()).eq_ignore_ascii_case(expected) {
            verified += size;
        } else {
            bad.push((index, start, size));
        }
    }
    progress.send_modify(|p| {
        p.path = Some(path.to_owned());
        p.total_length = Some(length);
        p.completed_length = verified;
    });
    let mirrors: Vec<_> = request
        .uris
        .iter()
        .filter(|uri| uri.starts_with("http://") || uri.starts_with("https://"))
        .collect();
    ensure!(
        bad.is_empty() || !mirrors.is_empty(),
        "piece repair requires an HTTP(S) mirror"
    );
    for (index, start, size) in bad {
        let end = start + size - 1;
        let mut repaired = false;
        let mut failure = anyhow!("no repair mirror succeeded");
        'attempts: for attempt in 0..options.max_tries.max(1) {
            for uri in &mirrors {
                let result: Result<()> = async {
                    let response = tokio::select! {
                        _ = cancel.cancelled() => bail!("download cancelled"),
                        response = session.send(session.request(Method::GET, uri).header("Range", format!("bytes={start}-{end}"))) => response?,
                    };
                    ensure!(response.status() == StatusCode::PARTIAL_CONTENT, "repair server did not honor Range");
                    let range = response.headers().get(CONTENT_RANGE).context("missing repair Content-Range")?.to_str()?;
                    ensure!(range == format!("bytes {start}-{end}/{length}"), "unexpected repair Content-Range");
                    ensure!(response.content_length().is_none_or(|n| n == size), "incorrect repair Content-Length");
                    file.seek(SeekFrom::Start(start)).await?;
                    let mut stream = response.bytes_stream();
                    let mut hash = hasher(&pieces.algorithm)?;
                    let mut received = 0;
                    loop {
                        let next = tokio::select! {
                            _ = cancel.cancelled() => bail!("download cancelled"),
                            next = stream.next() => next,
                        };
                        let Some(chunk) = next else { break };
                        let chunk = chunk?;
                        ensure!(chunk.len() as u64 <= size - received, "repair response exceeds piece length");
                        file.write_all(&chunk).await?;
                        hash.update(&chunk);
                        received += chunk.len() as u64;
                        if options.max_download_limit > 0 {
                            tokio::select! {
                                _ = cancel.cancelled() => bail!("download cancelled"),
                                _ = tokio::time::sleep(Duration::from_secs_f64(chunk.len() as f64 / options.max_download_limit as f64)) => {},
                            }
                        }
                    }
                    ensure!(received == size, "repair response ended early");
                    ensure!(hex::encode(hash.finalize()).eq_ignore_ascii_case(&pieces.hashes[index]), "repaired piece checksum mismatch");
                    Ok(())
                }.await;
                match result {
                    Ok(()) => {
                        repaired = true;
                        break 'attempts;
                    }
                    Err(error) => failure = error,
                }
                ensure!(!cancel.is_cancelled(), "download cancelled");
            }
            if attempt + 1 < options.max_tries.max(1) {
                tokio::select! {
                    _ = cancel.cancelled() => bail!("download cancelled"),
                    _ = tokio::time::sleep(Duration::from_secs(options.retry_wait_secs)) => {},
                }
            }
        }
        ensure!(repaired, "piece {} repair failed: {failure:#}", index + 1);
        verified += size;
        progress.send_modify(|p| p.completed_length = verified);
    }
    ensure!(!cancel.is_cancelled(), "download cancelled");
    file.set_len(length).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    crate::protocols::verify_file(path, options).await
}
