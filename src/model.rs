use std::path::{Component, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadOptions {
    pub dir: PathBuf,
    pub out: Option<String>,
    pub split: usize,
    pub min_split_size: u64,
    pub continue_download: bool,
    pub max_tries: u32,
    pub retry_wait_secs: u64,
    pub timeout_secs: u64,
    pub connect_timeout_secs: u64,
    pub headers: Vec<String>,
    pub user_agent: String,
    pub referer: Option<String>,
    pub proxy: Option<String>,
    pub http_user: Option<String>,
    pub http_passwd: Option<String>,
    pub http_auth_challenge: bool,
    pub load_cookies: Option<PathBuf>,
    pub save_cookies: Option<PathBuf>,
    pub checksum: Option<String>,
    pub max_download_limit: u64,
    pub max_upload_limit: u64,
    pub check_integrity: bool,
    pub allow_overwrite: bool,
    pub auto_file_renaming: bool,
    pub ftp_user: Option<String>,
    pub ftp_passwd: Option<String>,
    pub ftp_pasv: bool,
    pub ssh_private_key: Option<PathBuf>,
    pub ssh_passphrase: Option<String>,
    pub ssh_known_hosts: Option<PathBuf>,
    pub ssh_host_key_md: Option<String>,
    pub select_file: Option<String>,
    pub bt_trackers: Vec<String>,
    pub bt_initial_peers: Vec<String>,
    pub bt_enable_dht: bool,
    pub bt_listen_port: Option<u16>,
    pub seed_time_secs: u64,
    pub seed_time_explicit: bool,
    pub seed_ratio: Option<f64>,
    pub follow_torrent: bool,
    pub follow_metalink: bool,
    pub expected_length: Option<u64>,
    pub piece_hashes: Option<PieceHashes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PieceHashes {
    pub algorithm: String,
    pub length: u64,
    pub hashes: Vec<String>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            out: None,
            split: 5,
            min_split_size: 20 * 1024 * 1024,
            continue_download: false,
            max_tries: 5,
            retry_wait_secs: 1,
            timeout_secs: 60,
            connect_timeout_secs: 15,
            headers: Vec::new(),
            user_agent: concat!("ariai/", env!("CARGO_PKG_VERSION")).into(),
            referer: None,
            proxy: None,
            http_user: None,
            http_passwd: None,
            http_auth_challenge: false,
            load_cookies: None,
            save_cookies: None,
            checksum: None,
            max_download_limit: 0,
            max_upload_limit: 0,
            check_integrity: false,
            allow_overwrite: false,
            auto_file_renaming: true,
            ftp_user: None,
            ftp_passwd: None,
            ftp_pasv: true,
            ssh_private_key: None,
            ssh_passphrase: None,
            ssh_known_hosts: None,
            ssh_host_key_md: None,
            select_file: None,
            bt_trackers: Vec::new(),
            bt_initial_peers: Vec::new(),
            bt_enable_dht: true,
            bt_listen_port: None,
            seed_time_secs: 0,
            seed_time_explicit: false,
            seed_ratio: None,
            follow_torrent: true,
            follow_metalink: true,
            expected_length: None,
            piece_hashes: None,
        }
    }
}

impl DownloadOptions {
    pub fn validate(&self) -> Result<()> {
        if let Some(ratio) = self.seed_ratio {
            ensure!(
                ratio.is_finite() && ratio >= 0.0,
                "seed-ratio must be finite and nonnegative"
            );
        }
        ensure!(
            (1..=64).contains(&self.split),
            "split must be between 1 and 64"
        );
        ensure!(self.min_split_size > 0, "min-split-size must be positive");
        ensure!(
            self.timeout_secs > 0 && self.connect_timeout_secs > 0,
            "timeouts must be positive"
        );
        if let Some(out) = &self.out {
            let path = std::path::Path::new(out);
            ensure!(
                !out.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
                "out must be a relative path without parent traversal"
            );
        }
        let validate_digest = |algorithm: &str, digest: &str| -> Result<()> {
            let length = match algorithm.to_ascii_lowercase().as_str() {
                "sha-256" | "sha256" => 64,
                "sha-1" | "sha1" => 40,
                "md5" => 32,
                _ => bail!("unsupported checksum algorithm; use sha-256, sha-1 or md5"),
            };
            ensure!(
                digest.len() == length && digest.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid checksum digest"
            );
            Ok(())
        };
        if let Some(checksum) = &self.checksum {
            let Some((algorithm, digest)) = checksum.split_once('=') else {
                bail!("checksum must be ALGORITHM=HEX");
            };
            validate_digest(algorithm, digest)?;
        }
        for header in &self.headers {
            let Some((name, value)) = header.split_once(':') else {
                bail!("header must be NAME: VALUE");
            };
            reqwest::header::HeaderName::from_bytes(name.trim().as_bytes())?;
            reqwest::header::HeaderValue::from_str(value.trim())?;
        }
        if let Some(proxy) = &self.proxy {
            reqwest::Proxy::all(proxy)?;
        }
        if let Some(spec) = &self.select_file {
            selected_indices(Some(spec), None)?;
        }
        if let Some(pieces) = &self.piece_hashes {
            ensure!(
                pieces.length > 0 && !pieces.hashes.is_empty(),
                "invalid Metalink piece list"
            );
            for hash in &pieces.hashes {
                validate_digest(&pieces.algorithm, hash)?;
            }
        }
        if let Some(digest) = &self.ssh_host_key_md {
            let (algorithm, digest) = digest
                .split_once('=')
                .context("ssh-host-key-md must be ALGORITHM=HEX")?;
            validate_digest(algorithm, digest)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    /// URLs here are mirrors of the same file, as in aria2's default CLI mode.
    pub uris: Vec<String>,
    #[serde(default)]
    pub options: DownloadOptions,
}

impl DownloadRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.uris.is_empty(), "at least one URI is required");
        for uri in &self.uris {
            if !uri.contains("://")
                && !uri.starts_with("magnet:")
                && std::path::Path::new(uri)
                    .extension()
                    .is_some_and(|v| v.eq_ignore_ascii_case("torrent"))
            {
                continue;
            }
            let parsed = url::Url::parse(uri)?;
            ensure!(
                matches!(
                    parsed.scheme(),
                    "http" | "https" | "ftp" | "sftp" | "magnet" | "file"
                ),
                "protocol {} is not implemented yet",
                parsed.scheme()
            );
            if !matches!(parsed.scheme(), "magnet" | "file") {
                ensure!(parsed.host_str().is_some(), "URI must have a host");
            }
            if parsed.scheme() == "file" {
                ensure!(
                    parsed.path().ends_with(".torrent"),
                    "file URI only supports torrent metadata"
                );
            }
        }
        self.options.validate()
    }
}

/// Parse aria2's 1-based comma/range selection; return zero-based indices.
pub fn selected_indices(spec: Option<&str>, count: Option<usize>) -> Result<Vec<usize>> {
    let Some(spec) = spec else {
        return Ok((0..count.unwrap_or(0)).collect());
    };
    let mut indices = std::collections::BTreeSet::new();
    ensure!(!spec.is_empty(), "select-file cannot be empty");
    for token in spec.split(',') {
        let (start, end) = if let Some((start, end)) = token.split_once('-') {
            (start.parse::<usize>()?, end.parse::<usize>()?)
        } else {
            let value = token.parse::<usize>()?;
            (value, value)
        };
        ensure!(
            start > 0 && end >= start && end - start < 1_000_000,
            "invalid select-file range"
        );
        if let Some(count) = count {
            ensure!(end <= count, "select-file index exceeds file count");
        }
        indices.extend((start - 1)..end);
    }
    Ok(indices.into_iter().collect())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub total_length: Option<u64>,
    pub completed_length: u64,
    pub download_speed: u64,
    #[serde(default)]
    pub upload_length: u64,
    #[serde(default)]
    pub upload_speed: u64,
    #[serde(default)]
    pub connections: u64,
    pub path: Option<PathBuf>,
    pub files: Vec<DownloadFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFile {
    pub index: usize,
    pub path: PathBuf,
    pub length: u64,
    pub completed_length: u64,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub total_length: u64,
    pub files: Vec<DownloadFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Waiting,
    Active,
    Paused,
    Complete,
    Error,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSnapshot {
    pub gid: String,
    pub status: DownloadStatus,
    pub request: DownloadRequest,
    pub total_length: Option<u64>,
    pub completed_length: u64,
    pub download_speed: u64,
    #[serde(default)]
    pub upload_length: u64,
    #[serde(default)]
    pub upload_speed: u64,
    #[serde(default)]
    pub connections: u64,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub files: Vec<DownloadFile>,
}

#[derive(Debug, Clone)]
pub struct ManagerOptions {
    pub session_format: SessionFormat,
    pub max_concurrent_downloads: usize,
    pub max_overall_download_limit: u64,
    pub max_overall_upload_limit: u64,
    pub session_file: Option<PathBuf>,
    pub download_options: DownloadOptions,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum SessionFormat {
    #[default]
    Json,
    Aria2,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: 5,
            max_overall_download_limit: 0,
            max_overall_upload_limit: 0,
            session_file: None,
            session_format: SessionFormat::Json,
            download_options: DownloadOptions::default(),
        }
    }
}
