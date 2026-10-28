use crate::model::{DownloadProgress, DownloadRequest, DownloadResult};
use anyhow::{Context, Result, anyhow, bail, ensure};
use percent_encoding::percent_decode_str;
use russh::{
    client,
    keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

struct HostCheck {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    fingerprint: Option<String>,
}
impl client::Handler for HostCheck {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        // Certificates require CA and principal validation, which this client does not implement.
        ensure!(
            key.certificate().is_none(),
            "SSH host certificates are unsupported"
        );
        let key = key.public_key();
        if let Some(fingerprint) = &self.fingerprint {
            let (algorithm, expected) = fingerprint
                .split_once('=')
                .context("SSH host fingerprint must be ALGORITHM=HEX")?;
            let bytes = key.to_bytes()?;
            let actual = match algorithm.to_ascii_lowercase().as_str() {
                "sha-256" | "sha256" => hex::encode(Sha256::digest(&bytes)),
                "sha-1" | "sha1" => hex::encode(sha1::Sha1::digest(&bytes)),
                "md5" => hex::encode(md5::Md5::digest(&bytes)),
                _ => bail!("unsupported SSH host fingerprint algorithm"),
            };
            return Ok(actual.eq_ignore_ascii_case(expected));
        }
        if let Some(path) = &self.known_hosts {
            Ok(keys::check_known_hosts_path(
                &self.host, self.port, &key, path,
            )?)
        } else {
            Ok(keys::check_known_hosts(&self.host, self.port, &key)?)
        }
    }
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    uri_hash: String,
    size: u64,
    modified: Option<u32>,
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
    ensure!(url.scheme() == "sftp", "unsupported SFTP mirror");
    let host = url.host_str().context("missing SSH host")?.to_string();
    let port = url.port().unwrap_or(22);
    let path = percent_decode_str(url.path()).decode_utf8()?.into_owned();
    ensure!(
        !path.is_empty() && !path.contains('\0'),
        "invalid SFTP path"
    );
    let user = opts.ftp_user.clone().unwrap_or_else(|| {
        percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned()
    });
    ensure!(!user.is_empty(), "SFTP username is required");
    let password = opts.ftp_passwd.clone().or_else(|| {
        url.password()
            .map(|v| percent_decode_str(v).decode_utf8_lossy().into_owned())
    });
    let verifier = HostCheck {
        host: host.clone(),
        port,
        known_hosts: opts.ssh_known_hosts.clone(),
        fingerprint: opts.ssh_host_key_md.clone(),
    };
    let mut session = tokio::time::timeout(
        Duration::from_secs(opts.connect_timeout_secs),
        client::connect(
            Arc::new(client::Config::default()),
            (host.as_str(), port),
            verifier,
        ),
    )
    .await??;
    let authenticated = tokio::time::timeout(Duration::from_secs(opts.timeout_secs), async {
        let authenticated = if let Some(path) = &opts.ssh_private_key {
            let key = Arc::new(keys::load_secret_key(path, opts.ssh_passphrase.as_deref())?);
            let hash = session.best_supported_rsa_hash().await?.flatten();
            session
                .authenticate_publickey(&user, PrivateKeyWithHashAlg::new(key, hash))
                .await?
                .success()
        } else {
            session
                .authenticate_password(&user, password.as_deref().unwrap_or(""))
                .await?
                .success()
        };
        Ok::<bool, anyhow::Error>(authenticated)
    })
    .await??;
    ensure!(authenticated, "SFTP authentication failed");
    let channel = tokio::time::timeout(
        Duration::from_secs(opts.timeout_secs),
        session.channel_open_session(),
    )
    .await??;
    tokio::time::timeout(
        Duration::from_secs(opts.timeout_secs),
        channel.request_subsystem(true, "sftp"),
    )
    .await??;
    let sftp = russh_sftp::client::RawSftpSession::new(channel.into_stream());
    sftp.set_timeout(opts.timeout_secs);
    tokio::time::timeout(Duration::from_secs(opts.timeout_secs), sftp.init()).await??;
    let handle = tokio::time::timeout(
        Duration::from_secs(opts.timeout_secs),
        sftp.open(
            path,
            russh_sftp::protocol::OpenFlags::READ,
            Default::default(),
        ),
    )
    .await??
    .handle;
    let meta = tokio::time::timeout(Duration::from_secs(opts.timeout_secs), sftp.fstat(&handle))
        .await??
        .attrs;
    let size = meta.size.context("SFTP server did not provide file size")?;
    if range.is_some() {
        if let Some(expected) = opts.expected_length {
            ensure!(size == expected, "remote source length mismatch");
        }
    }
    let identity = Identity {
        uri_hash: hex::encode(Sha256::digest(uri.as_bytes())),
        size,
        modified: meta.mtime,
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
        && identity.modified.is_some()
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
    let start = Instant::now();
    let initial = offset;
    let mut buf = vec![0u8; 32 * 1024];
    while offset < end {
        let wanted = (end - offset).min(buf.len() as u64) as usize;
        let data = tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), data = tokio::time::timeout(Duration::from_secs(opts.timeout_secs), sftp.read(&handle, offset, wanted as u32)) => data??.data };
        let n = data.len();
        ensure!(n <= wanted, "SFTP response exceeds requested length");
        buf[..n].copy_from_slice(&data);
        ensure!(n > 0, "SFTP transfer ended early");
        ensure!(offset + n as u64 <= size, "SFTP response exceeds file size");
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
            let wait =
                Duration::from_secs_f64((offset - initial) as f64 / opts.max_download_limit as f64)
                    .saturating_sub(start.elapsed());
            tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), _ = tokio::time::sleep(wait) => {} }
        }
    }
    let after = tokio::time::timeout(Duration::from_secs(opts.timeout_secs), sftp.fstat(&handle))
        .await??
        .attrs;
    ensure!(
        after.size == Some(size) && after.mtime == identity.modified,
        "SFTP source changed during download"
    );
    tokio::time::timeout(Duration::from_secs(opts.timeout_secs), sftp.close(&handle)).await??;
    file.flush().await?;
    file.sync_all().await?;
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
    ensure!(request.options.proxy.is_none(), "SFTP proxy is unsupported");
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
    let mut last = anyhow!("no SFTP mirror succeeded");
    for attempt in 0..request.options.max_tries.max(1) {
        for uri in &request.uris {
            let result = tokio::select! { _ = cancel.cancelled() => bail!("download cancelled"), result = transfer(&request, uri, &target, &cancel, &progress, None, limiter.as_deref()) => result };
            match result {
                Ok(()) => {
                    let partial = super::side_path(&target, ".ariai.remote.part");
                    let result = async {
                        if request.options.piece_hashes.is_some() {
                            repair_partial(
                                &request,
                                &target,
                                &cancel,
                                &progress,
                                limiter.as_deref(),
                            )
                            .await?;
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
