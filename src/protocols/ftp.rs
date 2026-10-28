use crate::model::{DownloadProgress, DownloadRequest, DownloadResult};
use anyhow::{Context, Result, anyhow, bail, ensure};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

struct Control {
    stream: BufReader<TcpStream>,
    timeout: Duration,
}
impl Control {
    async fn reply(&mut self) -> Result<(u16, String)> {
        let mut all = String::new();
        let mut first = String::new();
        tokio::time::timeout(self.timeout, self.stream.read_line(&mut first)).await??;
        ensure!(
            first.len() >= 4
                && first.len() <= 8192
                && first.as_bytes()[..3].iter().all(u8::is_ascii_digit)
                && matches!(first.as_bytes()[3], b' ' | b'-'),
            "invalid FTP reply"
        );
        let code: u16 = first[..3].parse().context("invalid FTP reply code")?;
        let multi = first.as_bytes()[3] == b'-';
        all.push_str(&first);
        if multi {
            loop {
                let mut line = String::new();
                ensure!(
                    tokio::time::timeout(self.timeout, self.stream.read_line(&mut line)).await??
                        > 0,
                    "FTP connection closed"
                );
                ensure!(all.len() + line.len() <= 65536, "FTP reply too large");
                let end = line.starts_with(&format!("{code} "));
                all.push_str(&line);
                if end {
                    break;
                }
            }
        }
        Ok((code, all))
    }
    async fn command(&mut self, cmd: &str) -> Result<(u16, String)> {
        ensure!(!cmd.contains(['\r', '\n']), "invalid FTP command");
        tokio::time::timeout(
            self.timeout,
            self.stream
                .get_mut()
                .write_all(format!("{cmd}\r\n").as_bytes()),
        )
        .await??;
        self.reply().await
    }
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    uri_hash: String,
    size: u64,
    modified: String,
}

async fn transfer(
    request: &DownloadRequest,
    uri: &str,
    target: &Path,
    cancel: &CancellationToken,
    progress: &watch::Sender<DownloadProgress>,
    range: Option<(u64, u64)>,
    limiter: Option<&crate::engine::RateLimiter>,
) -> Result<()> {
    let opts = &request.options;
    let url = url::Url::parse(uri)?;
    ensure!(url.scheme() == "ftp", "unsupported FTP mirror");
    let host = url.host_str().context("missing FTP host")?;
    let path = percent_decode_str(url.path()).decode_utf8()?.into_owned();
    ensure!(
        !path.contains(['\r', '\n']) && !path.is_empty(),
        "invalid FTP path"
    );
    let user = opts.ftp_user.clone().unwrap_or_else(|| {
        if url.username().is_empty() {
            "anonymous".into()
        } else {
            percent_decode_str(url.username())
                .decode_utf8_lossy()
                .into_owned()
        }
    });
    let password = opts.ftp_passwd.clone().unwrap_or_else(|| {
        url.password()
            .map(|p| percent_decode_str(p).decode_utf8_lossy().into_owned())
            .unwrap_or_else(|| "anonymous@".into())
    });
    ensure!(
        !user.contains(['\r', '\n']) && !password.contains(['\r', '\n']),
        "invalid FTP credentials"
    );
    let stream = tokio::time::timeout(
        Duration::from_secs(opts.connect_timeout_secs),
        TcpStream::connect((host, url.port().unwrap_or(21))),
    )
    .await??;
    let peer = stream.peer_addr()?;
    let mut control = Control {
        stream: BufReader::new(stream),
        timeout: Duration::from_secs(opts.timeout_secs),
    };
    ensure!(
        control.reply().await?.0 == 220,
        "FTP server rejected connection"
    );
    let code = control.command(&format!("USER {user}")).await?.0;
    if code == 331 {
        ensure!(
            control.command(&format!("PASS {password}")).await?.0 == 230,
            "FTP authentication failed"
        );
    } else {
        ensure!(code == 230, "FTP authentication failed");
    }
    ensure!(
        control.command("TYPE I").await?.0 == 200,
        "FTP binary mode unavailable"
    );
    let (code, size_reply) = control.command(&format!("SIZE {path}")).await?;
    ensure!(code == 213, "FTP SIZE unavailable");
    let size: u64 = size_reply[4..].trim().parse()?;
    let (mtime_code, mtime_reply) = control.command(&format!("MDTM {path}")).await?;
    let modified = if mtime_code == 213 {
        mtime_reply[4..].trim().to_string()
    } else {
        String::new()
    };
    if range.is_some() {
        if let Some(expected) = opts.expected_length {
            ensure!(size == expected, "remote source length mismatch");
        }
    }
    let identity = Identity {
        uri_hash: hex::encode(Sha256::digest(uri.as_bytes())),
        size,
        modified,
    };
    let partial = super::side_path(target, ".ariai.remote.part");
    let sidecar = super::side_path(target, ".ariai.remote.json");
    let previous = tokio::fs::read(&sidecar)
        .await
        .ok()
        .and_then(|v| serde_json::from_slice::<Identity>(&v).ok());
    let mut offset = if let Some((start, _)) = range {
        start
    } else if opts.continue_download
        && !identity.modified.is_empty()
        && previous.as_ref() == Some(&identity)
    {
        tokio::fs::metadata(&partial)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    if offset > size {
        offset = 0;
    }
    let end = if let Some((start, length)) = range {
        ensure!(
            start.checked_add(length).is_some_and(|end| end <= size),
            "invalid repair range"
        );
        start + length
    } else {
        size
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(range.is_none() && offset == 0)
        .open(&partial)
        .await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    tokio::fs::write(&sidecar, serde_json::to_vec(&identity)?).await?;
    progress.send_modify(|p| {
        p.path = Some(target.to_path_buf());
        p.total_length = Some(size);
        p.completed_length = offset;
        p.download_speed = 0;
    });
    if offset < end {
        let mut data = if opts.ftp_pasv {
            let (code, reply) = control.command("EPSV").await?;
            let port = if code == 229 {
                let section = reply
                    .split('(')
                    .nth(1)
                    .and_then(|v| v.split(')').next())
                    .context("invalid EPSV response")?;
                section
                    .split('|')
                    .nth(3)
                    .context("invalid EPSV port")?
                    .parse::<u16>()?
            } else {
                let (code, reply) = control.command("PASV").await?;
                ensure!(code == 227, "FTP passive mode unavailable");
                let values = reply
                    .split('(')
                    .nth(1)
                    .and_then(|v| v.split(')').next())
                    .context("invalid PASV response")?
                    .split(',')
                    .map(str::parse::<u8>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                ensure!(values.len() == 6, "invalid PASV address");
                u16::from(values[4]) * 256 + u16::from(values[5])
            };
            Some(
                tokio::time::timeout(
                    Duration::from_secs(opts.connect_timeout_secs),
                    TcpStream::connect(SocketAddr::new(peer.ip(), port)),
                )
                .await??,
            )
        } else {
            None
        };
        let listener = if data.is_none() {
            let local = control.stream.get_ref().local_addr()?;
            let listener = TcpListener::bind(SocketAddr::new(local.ip(), 0)).await?;
            let addr = listener.local_addr()?;
            let family = if addr.is_ipv4() { 1 } else { 2 };
            let command = format!("EPRT |{family}|{}|{}|", addr.ip(), addr.port());
            let (code, _) = control.command(&command).await?;
            if code != 200 && addr.is_ipv4() {
                let std::net::IpAddr::V4(ip) = addr.ip() else {
                    unreachable!()
                };
                let a = ip.octets();
                ensure!(
                    control
                        .command(&format!(
                            "PORT {},{},{},{},{},{}",
                            a[0],
                            a[1],
                            a[2],
                            a[3],
                            addr.port() / 256,
                            addr.port() % 256
                        ))
                        .await?
                        .0
                        == 200,
                    "FTP active mode unavailable"
                );
            } else {
                ensure!(code == 200, "FTP active mode unavailable");
            }
            Some(listener)
        } else {
            None
        };
        if offset > 0 || range.is_some() {
            ensure!(
                control.command(&format!("REST {offset}")).await?.0 == 350,
                "FTP REST unavailable"
            );
        }
        let code = control.command(&format!("RETR {path}")).await?.0;
        ensure!(code == 125 || code == 150, "FTP RETR rejected");
        if let Some(listener) = listener {
            let (stream, address) = tokio::time::timeout(
                Duration::from_secs(opts.connect_timeout_secs),
                listener.accept(),
            )
            .await??;
            ensure!(
                address.ip() == peer.ip(),
                "FTP active connection from unexpected host"
            );
            data = Some(stream);
        }
        let mut data = data.context("missing FTP data connection")?;
        let start = Instant::now();
        let initial = offset;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if offset == end {
                break;
            }
            let wanted = (end - offset).min(buf.len() as u64) as usize;
            let n = tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), n = tokio::time::timeout(Duration::from_secs(opts.timeout_secs), data.read(&mut buf[..wanted])) => n?? };
            if n == 0 {
                break;
            }
            ensure!(offset + n as u64 <= size, "FTP response exceeds SIZE");
            file.write_all(&buf[..n]).await?;
            offset += n as u64;
            progress.send_modify(|p| {
                p.completed_length = offset;
                p.download_speed =
                    ((offset - initial) as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64;
            });
            if let Some(limiter) = limiter {
                limiter.acquire(n as u64, cancel).await?;
            }
            if opts.max_download_limit > 0 {
                let wait = Duration::from_secs_f64(
                    (offset - initial) as f64 / opts.max_download_limit as f64,
                )
                .saturating_sub(start.elapsed());
                tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), _ = tokio::time::sleep(wait) => {} }
            }
        }
        ensure!(offset == end, "FTP transfer ended early");
        drop(data);
        let code = control.reply().await?.0;
        if code == 426 && range.is_some() {
            ensure!(
                matches!(control.reply().await?.0, 226 | 250),
                "FTP abort not completed"
            );
        } else {
            ensure!(matches!(code, 226 | 250), "FTP transfer not completed");
        }
    }
    let (size_code, final_size) = control.command(&format!("SIZE {path}")).await?;
    ensure!(
        size_code == 213 && final_size[4..].trim().parse::<u64>().ok() == Some(size),
        "FTP source changed during download"
    );
    if !identity.modified.is_empty() {
        let (code, stamp) = control.command(&format!("MDTM {path}")).await?;
        ensure!(
            code == 213 && stamp[4..].trim() == identity.modified,
            "FTP source changed during download"
        );
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    ensure!(!cancel.is_cancelled(), "download cancelled");
    Ok(())
}

async fn repair_partial(
    request: &DownloadRequest,
    target: &Path,
    cancel: &CancellationToken,
    progress: &watch::Sender<DownloadProgress>,
    limiter: Option<&crate::engine::RateLimiter>,
) -> Result<()> {
    let partial = super::side_path(target, ".ariai.remote.part");
    let (length, bad) = crate::integrity::bad_pieces(&partial, &request.options, cancel).await?;
    let mut repair_request = request.clone();
    repair_request.options.expected_length = Some(length);
    let mut verified = length - bad.iter().map(|(_, _, size)| size).sum::<u64>();
    progress.send_modify(|p| {
        p.path = Some(target.to_owned());
        p.total_length = Some(length);
        p.completed_length = verified;
    });
    for (index, start, size) in bad {
        let mut failure = anyhow!("no repair mirror succeeded");
        let mut repaired = false;
        'attempts: for attempt in 0..request.options.max_tries.max(1) {
            for uri in &request.uris {
                let result = tokio::select! {
                    _ = cancel.cancelled() => bail!("download cancelled"),
                    result = transfer(&repair_request, uri, target, cancel, progress, Some((start, size)), limiter) => result,
                };
                match result {
                    Ok(()) => {
                        let (_, remaining) =
                            crate::integrity::bad_pieces(&partial, &request.options, cancel)
                                .await?;
                        if !remaining.iter().any(|(i, _, _)| *i == index) {
                            repaired = true;
                            break 'attempts;
                        }
                        failure = anyhow!("repaired piece checksum mismatch");
                    }
                    Err(error) => failure = error,
                }
            }
            if attempt + 1 < request.options.max_tries.max(1) {
                tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), _ = tokio::time::sleep(Duration::from_secs(request.options.retry_wait_secs)) => {} }
            }
        }
        ensure!(repaired, "piece {} repair failed: {failure:#}", index + 1);
        verified += size;
        progress.send_modify(|p| p.completed_length = verified);
    }
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&partial)
        .await?;
    file.set_len(length).await?;
    file.sync_all().await?;
    ensure!(!cancel.is_cancelled(), "download cancelled");
    super::verify_file(&partial, &request.options).await
}

pub async fn download(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<DownloadResult> {
    download_with_limit(request, cancel, progress, None).await
}

pub(crate) async fn download_with_limit(
    request: DownloadRequest,
    cancel: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
    limiter: Option<std::sync::Arc<crate::engine::RateLimiter>>,
) -> Result<DownloadResult> {
    request.validate()?;
    ensure!(request.options.proxy.is_none(), "FTP proxy is unsupported");
    let mut target_request = request.clone();
    if request.options.check_integrity && request.options.piece_hashes.is_some() {
        target_request.options.allow_overwrite = true;
    }
    let target = super::target(&target_request).await?;
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if request.options.check_integrity && request.options.piece_hashes.is_some() && target.exists()
    {
        let partial = super::side_path(&target, ".ariai.remote.part");
        tokio::fs::copy(&target, &partial).await?;
        repair_partial(&request, &target, &cancel, &progress, limiter.as_deref()).await?;
        ensure!(!cancel.is_cancelled(), "download cancelled");
        let mut options = request.options.clone();
        options.allow_overwrite = true;
        let result = super::publish(&partial, &target, &options).await?;
        let _ = tokio::fs::remove_file(super::side_path(&target, ".ariai.remote.json")).await;
        return Ok(result);
    }
    let mut last = anyhow!("no FTP mirror succeeded");
    for attempt in 0..request.options.max_tries.max(1) {
        for uri in &request.uris {
            let result = tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), result = transfer(&request, uri, &target, &cancel, &progress, None, limiter.as_deref()) => result };
            match result {
                Ok(()) => {
                    let partial = super::side_path(&target, ".ariai.remote.part");
                    let result = async {
                        if request.options.piece_hashes.is_some() {
                            repair_partial(&request, &target, &cancel, &progress, limiter.as_deref()).await?;
                        }
                        ensure!(!cancel.is_cancelled(), "download cancelled");
                        super::publish(&partial, &target, &request.options).await
                    }
                    .await;
                    match result {
                        Ok(result) => {
                            let _ = tokio::fs::remove_file(super::side_path(
                                &target,
                                ".ariai.remote.json",
                            ))
                            .await;
                            progress.send_modify(|p| {
                                p.total_length = Some(result.total_length);
                                p.completed_length = result.total_length;
                                p.download_speed = 0;
                            });
                            return Ok(result);
                        }
                        Err(err) => last = err,
                    }
                }
                Err(err) => last = err,
            }
        }
        if attempt + 1 < request.options.max_tries.max(1) {
            tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), _ = tokio::time::sleep(Duration::from_secs(request.options.retry_wait_secs)) => {} }
        }
    }
    Err(last)
}
