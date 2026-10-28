use crate::http::HttpSession;
use crate::model::{DownloadProgress, DownloadRequest, DownloadResult};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::{StreamExt, future::join_all};
use reqwest::{
    Method, Response, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Validator {
    etag: Option<String>,
    modified: Option<String>,
}
impl Validator {
    fn from_response(response: &Response) -> Self {
        Self {
            etag: response
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.starts_with("W/"))
                .map(str::to_owned),
            modified: response
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        }
    }
    fn value(&self) -> Option<&str> {
        self.etag.as_deref().or(self.modified.as_deref())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    uris: Vec<String>,
    length: u64,
    validator: Validator,
    ranges: Vec<(u64, u64)>,
    parts: Vec<u64>,
}
#[derive(Clone)]
struct Shared {
    client: HttpSession,
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
    state: Arc<Mutex<State>>,
    target: PathBuf,
}
struct State {
    completed: u64,
    start: Instant,
    rate_at: Instant,
}
fn side_path(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}
fn part_path(path: &Path, index: usize) -> PathBuf {
    side_path(path, &format!(".ariai.part{index}"))
}
fn content_range(response: &Response) -> Result<(u64, u64, u64)> {
    let value = response
        .headers()
        .get(CONTENT_RANGE)
        .context("missing Content-Range")?
        .to_str()?;
    let (span, total) = value
        .strip_prefix("bytes ")
        .context("invalid Content-Range unit")?
        .split_once('/')
        .context("invalid Content-Range")?;
    let (start, end) = span.split_once('-').context("invalid Content-Range span")?;
    let (start, end, total): (u64, u64, u64) = (start.parse()?, end.parse()?, total.parse()?);
    ensure!(start <= end && end < total, "invalid Content-Range bounds");
    Ok((start, end, total))
}
async fn send(
    shared: &Shared,
    url: &str,
    range: Option<(u64, u64)>,
    validator: Option<&str>,
) -> Result<Response> {
    let mut builder = shared.client.request(Method::GET, url);
    if let Some((start, end)) = range {
        builder = builder.header(RANGE, format!("bytes={start}-{end}"));
    }
    if let Some(value) = validator {
        builder = builder.header(IF_RANGE, value);
    }
    tokio::select! { _ = shared.cancel.cancelled() => bail!("download cancelled"), response = shared.client.send(builder) => response }
}
async fn update(shared: &Shared, bytes: u64) -> Result<()> {
    let mut state = shared.state.lock().await;
    state.completed += bytes;
    let elapsed = state.start.elapsed().as_secs_f64().max(0.001);
    shared.progress.send_modify(|p| {
        p.completed_length = state.completed;
        p.download_speed = (state.completed as f64 / elapsed) as u64;
    });
    let limit = shared.request.options.max_download_limit;
    let wait = if limit > 0 {
        let now = Instant::now();
        state.rate_at =
            state.rate_at.max(now) + Duration::from_secs_f64(bytes as f64 / limit as f64);
        state.rate_at.saturating_duration_since(now)
    } else {
        Duration::ZERO
    };
    drop(state);
    if !wait.is_zero() {
        tokio::select! { _ = shared.cancel.cancelled() => bail!("download cancelled"), _ = tokio::time::sleep(wait) => {} }
    }
    Ok(())
}
async fn transfer_part(
    shared: Shared,
    index: usize,
    range: (u64, u64),
    length: u64,
    validator: Validator,
    resume: u64,
    probe_url: String,
) -> Result<()> {
    let (start, end) = range;
    let path = part_path(&shared.target, index);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&path)
        .await?;
    let mut done = resume;
    let expected = end - start + 1;
    let tries = shared.request.options.max_tries.max(1);
    let mut last_error = anyhow!("no mirror succeeded");
    for attempt in 0..tries {
        if done == expected {
            break;
        }
        for url in &shared.request.uris {
            if validator.value().is_none() && url != &probe_url {
                continue;
            }
            if done == expected {
                break;
            }
            let result: Result<()> = async {
                let response = send(&shared, url, Some((start + done,end)), validator.value()).await?;
                ensure!(response.status() == StatusCode::PARTIAL_CONTENT, "server did not honor Range: {}", response.status());
                ensure!(content_range(&response)? == (start + done,end,length), "unexpected Content-Range");
                if let Some(size) = response.content_length() { ensure!(size == expected-done, "incorrect Content-Length"); }
                if validator.value().is_some() { ensure!(Validator::from_response(&response) == validator, "remote validator changed"); }
                let mut stream = response.bytes_stream();
                loop {
                    let next = tokio::select! { _ = shared.cancel.cancelled() => bail!("download cancelled"), next = stream.next() => next };
                    let Some(chunk) = next else { break; }; let chunk = chunk?;
                    ensure!(chunk.len() as u64 <= expected-done, "range response exceeds expected length");
                    file.write_all(&chunk).await?; done += chunk.len() as u64;
                    update(&shared, chunk.len() as u64).await?;
                }
                ensure!(done == expected, "range response ended early");
                Ok(())
            }.await;
            if let Err(err) = result {
                last_error = err;
                if shared.cancel.is_cancelled() {
                    break;
                }
            }
        }
        if done < expected && !shared.cancel.is_cancelled() && attempt + 1 < tries {
            tokio::select! { _ = shared.cancel.cancelled() => {}, _ = tokio::time::sleep(Duration::from_secs(shared.request.options.retry_wait_secs)) => {} }
        }
        if shared.cancel.is_cancelled() {
            break;
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    ensure!(
        done == expected && !shared.cancel.is_cancelled(),
        "{last_error}"
    );
    Ok(())
}
async fn checkpoint(target: &Path, data: &mut Checkpoint) -> Result<()> {
    for (index, size) in data.parts.iter_mut().enumerate() {
        *size = tokio::fs::metadata(part_path(target, index))
            .await
            .map(|m| m.len())
            .unwrap_or(0);
    }
    let path = side_path(target, ".ariai");
    let temp = side_path(target, ".ariai.tmp");
    let mut file = tokio::fs::File::create(&temp).await?;
    file.write_all(&serde_json::to_vec(data)?).await?;
    file.sync_all().await?;
    drop(file);
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    tokio::fs::rename(temp, path).await?;
    Ok(())
}
async fn clear_parts(target: &Path, count: usize) {
    for i in 0..count {
        let _ = tokio::fs::remove_file(part_path(target, i)).await;
    }
}

pub async fn download(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<DownloadResult> {
    request.validate()?;
    let first = &request.uris[0];
    let parsed = url::Url::parse(first).ok();
    let torrent = first.starts_with("magnet:")
        || parsed.as_ref().is_some_and(|url| url.scheme() == "file")
        || (!first.contains("://")
            && std::path::Path::new(first)
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("torrent")))
        || (request.options.follow_torrent
            && parsed.as_ref().is_some_and(|url| {
                matches!(url.scheme(), "http" | "https") && url.path().ends_with(".torrent")
            }));
    if torrent {
        return crate::torrent::download(request, cancel, progress).await;
    }
    let mut schemes = Vec::new();
    for uri in &request.uris {
        let scheme = url::Url::parse(uri)?.scheme().to_owned();
        let group = if scheme == "https" {
            "http".to_string()
        } else {
            scheme
        };
        if !schemes.contains(&group) {
            schemes.push(group);
        }
    }
    let mut failure = anyhow!("no source succeeded");
    for scheme in schemes {
        let uris = request
            .uris
            .iter()
            .filter(|uri| {
                url::Url::parse(uri).is_ok_and(|u| {
                    u.scheme() == scheme || scheme == "http" && u.scheme() == "https"
                })
            })
            .cloned()
            .collect();
        let attempt = DownloadRequest {
            uris,
            options: request.options.clone(),
        };
        let result = match scheme.as_str() {
            "http" => download_http(attempt, cancel.clone(), progress.clone()).await,
            "ftp" => {
                crate::protocols::ftp::download(attempt, cancel.clone(), progress.clone()).await
            }
            "sftp" => {
                crate::protocols::sftp::download(attempt, cancel.clone(), progress.clone()).await
            }
            _ => bail!("unsupported source protocol: {scheme}"),
        };
        match result {
            Ok(result) => return Ok(result),
            Err(error) => failure = error,
        }
        if cancel.is_cancelled() {
            break;
        }
    }
    Err(failure)
}

async fn download_http(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<DownloadResult> {
    request.validate()?;
    let options = &request.options;
    let client = HttpSession::new(options)?;
    let result = download_http_inner(request.clone(), cancel, progress, client.clone()).await;
    let saved = client.save_cookies().await;
    match result {
        Ok(result) => {
            saved?;
            Ok(result)
        }
        Err(error) => {
            saved?;
            Err(error)
        }
    }
}

async fn download_http_inner(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
    client: HttpSession,
) -> Result<DownloadResult> {
    let options = &request.options;
    let name = match &options.out {
        Some(out) => out.clone(),
        None => {
            let url = url::Url::parse(&request.uris[0])?;
            let leaf = url
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or("download");
            let safe: String = leaf
                .chars()
                .map(|c| {
                    if c.is_control() || "<>:\"/\\|?*".contains(c) {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            if safe == "." || safe == ".." || safe.is_empty() {
                "download".into()
            } else {
                safe
            }
        }
    };
    let mut target = options.dir.join(name);
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if options.check_integrity && options.piece_hashes.is_some() && target.exists() {
        crate::integrity::repair_file(&target, &request, &client, &cancel, &progress).await?;
        let length = tokio::fs::metadata(&target).await?.len();
        progress.send_modify(|p| {
            p.path = Some(target.clone());
            p.completed_length = length;
            p.total_length = Some(length);
            p.download_speed = 0;
        });
        return Ok(DownloadResult {
            files: Vec::new(),
            path: target,
            total_length: length,
        });
    }
    if target.exists() && !options.allow_overwrite {
        ensure!(
            options.auto_file_renaming,
            "output already exists: {}",
            target.display()
        );
        let original = target.clone();
        let mut i = 1;
        loop {
            target = side_path(&original, &format!(".{i}"));
            if !target.exists() && !side_path(&target, ".ariai").exists() {
                break;
            }
            i += 1;
        }
    }
    let now = Instant::now();
    let shared = Shared {
        client,
        request: request.clone(),
        cancel: cancel.clone(),
        progress: progress.clone(),
        state: Arc::new(Mutex::new(State {
            completed: 0,
            start: now,
            rate_at: now,
        })),
        target: target.clone(),
    };
    progress.send_modify(|p| {
        p.path = Some(target.clone());
        p.completed_length = 0;
        p.total_length = None;
        p.download_speed = 0;
    });
    let mut probe = None;
    let mut last = anyhow!("no mirror succeeded");
    for attempt in 0..options.max_tries.max(1) {
        for url in &request.uris {
            match send(&shared, url, Some((0, 0)), None).await {
                Ok(response) if response.status() == StatusCode::PARTIAL_CONTENT => {
                    match content_range(&response) {
                        Ok((0, 0, total)) if response.content_length().is_none_or(|n| n == 1) => {
                            probe = Some((
                                true,
                                Some(total),
                                Validator::from_response(&response),
                                url.clone(),
                            ));
                            break;
                        }
                        _ => last = anyhow!("invalid probe Content-Range"),
                    }
                }
                Ok(response)
                    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE
                        && response
                            .headers()
                            .get(CONTENT_RANGE)
                            .and_then(|v| v.to_str().ok())
                            == Some("bytes */0") =>
                {
                    probe = Some((
                        false,
                        Some(0),
                        Validator::from_response(&response),
                        url.clone(),
                    ));
                    break;
                }
                Ok(response) if response.status() == StatusCode::OK => {
                    probe = Some((
                        false,
                        response
                            .headers()
                            .get(CONTENT_LENGTH)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse().ok()),
                        Validator::from_response(&response),
                        url.clone(),
                    ));
                    break;
                }
                Ok(response) => last = anyhow!("probe returned {}", response.status()),
                Err(err) => last = err,
            }
        }
        if probe.is_some() || cancel.is_cancelled() {
            break;
        }
        if attempt + 1 < options.max_tries.max(1) {
            tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(options.retry_wait_secs)) => {} }
        }
    }
    let (ranged, total, validator, probe_url) = probe.ok_or(last)?;
    progress.send_modify(|p| p.total_length = total);
    let part_count = if ranged {
        let n = total.unwrap();
        ((n / options.min_split_size).max(1) as usize)
            .min(options.split)
            .min(n as usize)
            .max(1)
    } else {
        1
    };
    let length = total.unwrap_or(0);
    let ranges = if ranged {
        (0..part_count)
            .map(|i| {
                let start = length * i as u64 / part_count as u64;
                let end = length * (i + 1) as u64 / part_count as u64 - 1;
                (start, end)
            })
            .collect::<Vec<_>>()
    } else {
        vec![(0, length.saturating_sub(1))]
    };
    let sidecar = side_path(&target, ".ariai");
    let previous = match tokio::fs::read(&sidecar).await {
        Ok(bytes) => serde_json::from_slice::<Checkpoint>(&bytes).ok(),
        Err(_) => None,
    };
    let can_resume = ranged
        && options.continue_download
        && validator.value().is_some()
        && previous.as_ref().is_some_and(|p| {
            p.uris == request.uris
                && p.length == length
                && p.validator == validator
                && p.ranges == ranges
                && p.parts.len() == part_count
        });
    if !can_resume {
        if let Some(old) = &previous {
            clear_parts(&target, old.parts.len()).await;
        }
        clear_parts(&target, part_count).await;
    }
    let mut cp = Checkpoint {
        uris: request.uris.clone(),
        length,
        validator: validator.clone(),
        ranges: ranges.clone(),
        parts: vec![0; part_count],
    };
    if can_resume {
        for (i, (start, end)) in ranges.iter().enumerate() {
            let size = tokio::fs::metadata(part_path(&target, i))
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            if size > end - start + 1 {
                clear_parts(&target, part_count).await;
                cp.parts.fill(0);
                break;
            }
            cp.parts[i] = size;
        }
    }
    let resumed: u64 = cp.parts.iter().sum();
    {
        let mut state = shared.state.lock().await;
        state.completed = resumed;
    }
    progress.send_modify(|p| p.completed_length = resumed);
    checkpoint(&target, &mut cp).await?;
    if ranged {
        let jobs = ranges.iter().enumerate().map(|(i, (start, end))| {
            transfer_part(
                shared.clone(),
                i,
                (*start, *end),
                length,
                validator.clone(),
                cp.parts[i],
                probe_url.clone(),
            )
        });
        let results = join_all(jobs).await;
        checkpoint(&target, &mut cp).await?;
        for result in results {
            result?;
        }
    } else {
        let mut succeeded = false;
        let mut failure = anyhow!("no mirror succeeded");
        'attempts: for attempt in 0..options.max_tries.max(1) {
            for uri in &request.uris {
                let mut file = tokio::fs::File::create(part_path(&target, 0)).await?;
                {
                    let mut state = shared.state.lock().await;
                    state.completed = 0;
                }
                progress.send_modify(|p| p.completed_length = 0);
                let result: Result<()> = async {
                    let response = send(&shared,uri,None,None).await?; ensure!(response.status().is_success(), "server returned {}", response.status());
                    let expected = response.content_length(); if let Some(n) = total { ensure!(expected.is_none_or(|v| v == n), "remote length changed"); }
                    if validator.value().is_some() { ensure!(Validator::from_response(&response) == validator, "remote validator changed"); }
                    let mut bytes=0u64; let mut stream=response.bytes_stream();
                    loop { let item = tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), item=stream.next() => item }; let Some(item)=item else { break; }; let chunk=item?; bytes += chunk.len() as u64; if let Some(n)=expected { ensure!(bytes<=n,"response exceeds Content-Length"); } file.write_all(&chunk).await?; update(&shared,chunk.len() as u64).await?; }
                    if let Some(n)=expected { ensure!(bytes==n,"response ended early"); } Ok(())
                }.await;
                file.flush().await?;
                file.sync_all().await?;
                match result {
                    Ok(()) => {
                        succeeded = true;
                        break 'attempts;
                    }
                    Err(err) => failure = err,
                }
                if cancel.is_cancelled() {
                    break 'attempts;
                }
            }
            if attempt + 1 < options.max_tries.max(1) {
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(options.retry_wait_secs)) => {} }
            }
        }
        checkpoint(&target, &mut cp).await?;
        ensure!(succeeded, "{failure}");
    }
    ensure!(!cancel.is_cancelled(), "download cancelled");
    let assembled = side_path(&target, ".ariai.assembled");
    let mut output = tokio::fs::File::create(&assembled).await?;
    let mut written = 0;
    for i in 0..part_count {
        let mut file = tokio::fs::File::open(part_path(&target, i)).await?;
        written += tokio::io::copy(&mut file, &mut output).await?;
    }
    output.flush().await?;
    output.sync_all().await?;
    drop(output);
    if let Some(expected) = total {
        ensure!(written == expected, "assembled length mismatch");
    }
    let repaired =
        crate::integrity::repair_file(&assembled, &request, &shared.client, &cancel, &progress)
            .await;
    progress.send_modify(|p| p.path = Some(target.clone()));
    if let Err(err) = repaired {
        let _ = tokio::fs::remove_file(&assembled).await;
        return Err(err);
    }
    let written = tokio::fs::metadata(&assembled).await?.len();
    ensure!(!cancel.is_cancelled(), "download cancelled");
    if target.exists() {
        ensure!(options.allow_overwrite, "output already exists");
        tokio::fs::remove_file(&target).await?;
    }
    tokio::fs::rename(&assembled, &target).await?;
    clear_parts(&target, part_count).await;
    let _ = tokio::fs::remove_file(sidecar).await;
    progress.send_modify(|p| {
        p.completed_length = written;
        p.total_length = Some(written);
        p.download_speed = 0;
    });
    Ok(DownloadResult {
        files: Vec::new(),
        path: target,
        total_length: written,
    })
}
