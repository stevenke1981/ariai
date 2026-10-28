use crate::model::{DownloadOptions, DownloadRequest, DownloadResult};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

pub mod ftp;
pub mod sftp;

pub fn side_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

pub async fn target(request: &DownloadRequest) -> Result<PathBuf> {
    let options = &request.options;
    let name = if let Some(out) = &options.out {
        out.clone()
    } else {
        let url = url::Url::parse(&request.uris[0])?;
        let leaf = url
            .path_segments()
            .and_then(|mut parts| parts.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("download");
        let decoded = percent_encoding::percent_decode_str(leaf).decode_utf8_lossy();
        let safe: String = decoded
            .chars()
            .map(|c| {
                if c.is_control() || "<>:\"/\\|?*".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        if safe.is_empty() || safe == "." || safe == ".." {
            "download".into()
        } else {
            safe
        }
    };
    let mut path = options.dir.join(name);
    if path.exists() && !options.allow_overwrite {
        ensure!(
            options.auto_file_renaming,
            "output already exists: {}",
            path.display()
        );
        let original = path.clone();
        let mut index = 1;
        loop {
            path = side_path(&original, &format!(".{index}"));
            if !path.exists() && !side_path(&path, ".ariai").exists() {
                break;
            }
            index += 1;
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(path)
}

pub async fn verify_file(path: &Path, options: &DownloadOptions) -> Result<()> {
    if let Some(length) = options.expected_length {
        ensure!(
            tokio::fs::metadata(path).await?.len() == length,
            "manifest length mismatch"
        );
    }
    if let Some(checksum) = &options.checksum {
        let (algorithm, expected) = checksum.split_once('=').context("invalid checksum")?;
        let mut hash: Box<dyn sha2::digest::DynDigest + Send> =
            match algorithm.to_ascii_lowercase().as_str() {
                "sha-256" | "sha256" => Box::new(Sha256::new()),
                "sha-1" | "sha1" => Box::new(sha1::Sha1::new()),
                "md5" => Box::new(md5::Md5::new()),
                _ => bail!("unsupported checksum"),
            };
        let mut file = tokio::fs::File::open(path).await?;
        let mut buffer = vec![0; 128 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        ensure!(
            hex::encode(hash.finalize()).eq_ignore_ascii_case(expected),
            "checksum mismatch"
        );
    }
    if let Some(pieces) = &options.piece_hashes {
        let mut file = tokio::fs::File::open(path).await?;
        let mut buffer = vec![0; 128 * 1024];
        for (index, expected) in pieces.hashes.iter().enumerate() {
            let mut hash: Box<dyn sha2::digest::DynDigest + Send> =
                match pieces.algorithm.to_ascii_lowercase().as_str() {
                    "sha-256" | "sha256" => Box::new(Sha256::new()),
                    "sha-1" | "sha1" => Box::new(sha1::Sha1::new()),
                    "md5" => Box::new(md5::Md5::new()),
                    _ => bail!("unsupported piece checksum"),
                };
            let mut remaining = pieces.length;
            let mut length = 0;
            while remaining > 0 {
                let wanted = (remaining.min(buffer.len() as u64)) as usize;
                let read = file.read(&mut buffer[..wanted]).await?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
                remaining -= read as u64;
                length += read;
            }
            ensure!(
                length > 0 && (remaining == 0 || index + 1 == pieces.hashes.len()),
                "missing Metalink piece data"
            );
            ensure!(
                hex::encode(hash.finalize()).eq_ignore_ascii_case(expected),
                "piece {} checksum mismatch",
                index + 1
            );
        }
        ensure!(
            file.read(&mut buffer[..1]).await? == 0,
            "piece hash list does not cover the file"
        );
    }
    Ok(())
}

pub async fn publish(
    partial: &Path,
    target: &Path,
    options: &DownloadOptions,
) -> Result<DownloadResult> {
    verify_file(partial, options).await?;
    let total_length = tokio::fs::metadata(partial).await?.len();
    if target.exists() {
        ensure!(options.allow_overwrite, "output already exists");
    }
    tokio::fs::rename(partial, target).await?;
    Ok(DownloadResult {
        path: target.into(),
        total_length,
        files: Vec::new(),
    })
}
