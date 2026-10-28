use crate::model::DownloadOptions;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

pub fn apply_option(options: &mut DownloadOptions, key: &str, value: &str) -> Result<()> {
    match key {
        "dir" => options.dir = PathBuf::from(value),
        "out" => options.out = Some(value.into()),
        "split" => options.split = value.parse()?,
        "min-split-size" => options.min_split_size = parse_size(value)?,
        "continue" => options.continue_download = parse_bool(value)?,
        "max-tries" => options.max_tries = value.parse()?,
        "retry-wait" => options.retry_wait_secs = value.parse()?,
        "timeout" => options.timeout_secs = value.parse()?,
        "connect-timeout" => options.connect_timeout_secs = value.parse()?,
        "header" => options.headers.push(value.into()),
        "user-agent" => options.user_agent = value.into(),
        "referer" => options.referer = Some(value.into()),
        "all-proxy" | "http-proxy" => options.proxy = Some(value.into()),
        "http-user" => options.http_user = Some(value.into()),
        "http-passwd" => options.http_passwd = Some(value.into()),
        "http-auth-challenge" => options.http_auth_challenge = parse_bool(value)?,
        "load-cookies" => options.load_cookies = Some(value.into()),
        "save-cookies" => options.save_cookies = Some(value.into()),
        "checksum" => options.checksum = Some(value.into()),
        "max-download-limit" => options.max_download_limit = parse_size(value)?,
        "max-upload-limit" => options.max_upload_limit = parse_size(value)?,
        "check-integrity" => options.check_integrity = parse_bool(value)?,
        "allow-overwrite" => options.allow_overwrite = parse_bool(value)?,
        "auto-file-renaming" => options.auto_file_renaming = parse_bool(value)?,
        "ftp-user" => options.ftp_user = Some(value.into()),
        "ftp-passwd" => options.ftp_passwd = Some(value.into()),
        "ftp-pasv" => options.ftp_pasv = parse_bool(value)?,
        "ssh-private-key" => options.ssh_private_key = Some(value.into()),
        "ssh-passphrase" => options.ssh_passphrase = Some(value.into()),
        "ssh-known-hosts" => options.ssh_known_hosts = Some(value.into()),
        "ssh-host-key-md" => options.ssh_host_key_md = Some(value.into()),
        "select-file" => options.select_file = Some(value.into()),
        "bt-tracker" => options.bt_trackers.extend(
            value
                .split(',')
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
        ),
        "bt-initial-peer" => options.bt_initial_peers.extend(
            value
                .split(',')
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
        ),
        "enable-dht" => options.bt_enable_dht = parse_bool(value)?,
        "listen-port" => options.bt_listen_port = Some(value.parse()?),
        "seed-time" => {
            let minutes: f64 = value.parse()?;
            anyhow::ensure!(
                minutes.is_finite() && minutes >= 0.0 && minutes < (u64::MAX / 60) as f64,
                "invalid seed-time"
            );
            options.seed_time_secs = (minutes * 60.0) as u64;
            options.seed_time_explicit = true;
        }
        "seed-ratio" => options.seed_ratio = Some(value.parse()?),
        "follow-torrent" => options.follow_torrent = parse_bool(value)?,
        "follow-metalink" => options.follow_metalink = parse_bool(value)?,
        _ => bail!("unsupported option: {key}"),
    }
    Ok(())
}

pub fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("expected true or false, got {value}"),
    }
}

pub fn parse_size(value: &str) -> Result<u64> {
    let value = value.trim();
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let number: u64 = value[..digits].parse().context("invalid size")?;
    let multiplier = match value[digits..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => bail!("invalid size suffix: {value}"),
    };
    number.checked_mul(multiplier).context("size is too large")
}
