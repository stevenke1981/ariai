use crate::{
    config::{apply_option, parse_bool, parse_size},
    manager::DownloadManager,
    model::{DownloadOptions, DownloadRequest, DownloadStatus, ManagerOptions},
    rpc,
};
use anyhow::{Context, Result, bail, ensure};
use clap::{Arg, ArgAction, Command};
use std::{io::Read, path::PathBuf};
use tokio_util::sync::CancellationToken;

const DOWNLOAD_OPTIONS: &[(&str, Option<char>, bool)] = &[
    ("dir", Some('d'), false),
    ("out", Some('o'), false),
    ("split", Some('s'), false),
    ("min-split-size", None, false),
    ("continue", Some('c'), true),
    ("max-tries", None, false),
    ("retry-wait", None, false),
    ("timeout", None, false),
    ("connect-timeout", None, false),
    ("header", None, false),
    ("user-agent", None, false),
    ("referer", None, false),
    ("all-proxy", None, false),
    ("http-proxy", None, false),
    ("http-user", None, false),
    ("http-passwd", None, false),
    ("http-auth-challenge", None, true),
    ("load-cookies", None, false),
    ("save-cookies", None, false),
    ("checksum", None, false),
    ("max-download-limit", None, false),
    ("max-upload-limit", Some('u'), false),
    ("check-integrity", Some('V'), true),
    ("allow-overwrite", None, true),
    ("auto-file-renaming", None, true),
    ("ftp-user", None, false),
    ("ftp-passwd", None, false),
    ("ftp-pasv", None, true),
    ("ssh-private-key", None, false),
    ("ssh-passphrase", None, false),
    ("ssh-known-hosts", None, false),
    ("ssh-host-key-md", None, false),
    ("select-file", None, false),
    ("bt-tracker", None, false),
    ("bt-initial-peer", None, false),
    ("enable-dht", None, true),
    ("listen-port", None, false),
    ("seed-time", None, false),
    ("seed-ratio", None, false),
    ("follow-torrent", None, true),
    ("follow-metalink", None, true),
];
const GLOBAL_OPTIONS: &[(&str, Option<char>, bool)] = &[
    ("max-concurrent-downloads", Some('j'), false),
    ("max-overall-download-limit", None, false),
    ("max-overall-upload-limit", None, false),
    ("input-file", Some('i'), false),
    ("force-sequential", None, true),
    ("enable-rpc", None, true),
    ("rpc-listen-port", None, false),
    ("rpc-listen-all", None, true),
    ("rpc-secret", None, false),
    ("save-session", None, false),
    ("load-session", None, false),
    ("json", None, true),
    ("torrent-file", Some('T'), false),
    ("metalink-file", Some('M'), false),
];
#[derive(Default)]
struct Settings {
    download: DownloadOptions,
    concurrent: Option<usize>,
    overall_download: u64,
    overall_upload: u64,
    input: Option<PathBuf>,
    sequential: bool,
    rpc: bool,
    port: Option<u16>,
    listen_all: bool,
    secret: Option<String>,
    save: Option<PathBuf>,
    load: Option<PathBuf>,
    json: bool,
    torrent: Option<String>,
    metalink: Option<String>,
}
impl Settings {
    fn apply(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "max-concurrent-downloads" => self.concurrent = Some(value.parse()?),
            "max-overall-download-limit" => self.overall_download = parse_size(value)?,
            "max-overall-upload-limit" => self.overall_upload = parse_size(value)?,
            "input-file" => self.input = Some(value.into()),
            "force-sequential" => self.sequential = parse_bool(value)?,
            "enable-rpc" => self.rpc = parse_bool(value)?,
            "rpc-listen-port" => self.port = Some(value.parse()?),
            "rpc-listen-all" => self.listen_all = parse_bool(value)?,
            "rpc-secret" => self.secret = Some(value.into()),
            "save-session" => self.save = Some(value.into()),
            "load-session" => self.load = Some(value.into()),
            "json" => self.json = parse_bool(value)?,
            "torrent-file" => self.torrent = Some(value.into()),
            "metalink-file" => self.metalink = Some(value.into()),
            _ => apply_option(&mut self.download, key, value)?,
        }
        Ok(())
    }
}

fn command() -> Command {
    let mut command = Command::new("ariai")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .about("Native multi-protocol downloader: HTTP(S), FTP, SFTP, BitTorrent, Metalink")
        .arg(
            Arg::new("version")
                .long("version")
                .short('v')
                .action(ArgAction::Version),
        )
        .arg(Arg::new("conf-path").long("conf-path").value_name("FILE"))
        .arg(
            Arg::new("uri")
                .value_name("URI")
                .num_args(0..)
                .action(ArgAction::Append),
        );
    for &(name, short, boolean) in DOWNLOAD_OPTIONS.iter().chain(GLOBAL_OPTIONS) {
        let mut arg = Arg::new(name).long(name).action(ArgAction::Append);
        if let Some(short) = short {
            arg = arg.short(short);
        }
        if boolean {
            arg = arg
                .num_args(0..=1)
                .default_missing_value("true")
                .require_equals(true)
                .value_name("BOOL");
        } else {
            arg = arg.value_name("VALUE");
        }
        command = command.arg(arg);
    }
    command
}

fn parse_input(text: &str, defaults: &DownloadOptions) -> Result<Vec<DownloadRequest>> {
    let mut requests: Vec<DownloadRequest> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if raw.starts_with(char::is_whitespace) {
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("input line {}: expected key=value", index + 1))?;
            let request = requests
                .last_mut()
                .with_context(|| format!("input line {}: option without URI", index + 1))?;
            apply_option(&mut request.options, key.trim(), value.trim())
                .with_context(|| format!("input line {}", index + 1))?;
        } else {
            requests.push(DownloadRequest {
                uris: line.split_whitespace().map(str::to_owned).collect(),
                options: defaults.clone(),
            });
        }
    }
    Ok(requests)
}

pub async fn run() -> Result<()> {
    let matches = command().get_matches();
    let mut settings = Settings::default();
    if let Some(path) = matches.get_one::<String>("conf-path") {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading config {path}"))?;
        for (index, raw) in contents.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("config line {}: expected key=value", index + 1))?;
            settings
                .apply(key.trim(), value.trim())
                .with_context(|| format!("config line {}", index + 1))?;
        }
    }
    for &(name, _, _) in DOWNLOAD_OPTIONS.iter().chain(GLOBAL_OPTIONS) {
        if let Some(values) = matches.get_many::<String>(name) {
            for value in values {
                settings
                    .apply(name, value)
                    .with_context(|| format!("option --{name}"))?;
            }
        }
    }
    settings.download.validate()?;
    let mut requests = Vec::new();
    if let Some(source) = &settings.torrent {
        requests.push(DownloadRequest {
            uris: vec![source.clone()],
            options: settings.download.clone(),
        });
    }
    if let Some(source) = &settings.metalink {
        requests.extend(crate::source::metalink(source, &settings.download).await?);
    }
    if let Some(path) = &settings.input {
        let text = if path == std::path::Path::new("-") {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        } else {
            std::fs::read_to_string(path)
                .with_context(|| format!("reading input {}", path.display()))?
        };
        requests.extend(parse_input(&text, &settings.download)?);
    }
    let uris: Vec<String> = matches
        .get_many::<String>("uri")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    if settings.sequential {
        requests.extend(uris.into_iter().map(|uri| DownloadRequest {
            uris: vec![uri],
            options: settings.download.clone(),
        }));
    } else if !uris.is_empty() {
        requests.push(DownloadRequest {
            uris,
            options: settings.download.clone(),
        });
    }
    ensure!(
        settings.rpc || settings.load.is_some() || !requests.is_empty(),
        "provide a URI, input file, session, or --enable-rpc"
    );
    let mut expanded = Vec::new();
    for request in requests {
        if request.uris.len() == 1
            && request.options.follow_metalink
            && crate::source::is_metalink(&request.uris[0])
        {
            expanded.extend(crate::source::metalink(&request.uris[0], &request.options).await?);
        } else {
            expanded.push(request);
        }
    }
    let requests = expanded;
    for request in &requests {
        request.validate()?;
    }
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: settings.concurrent.unwrap_or(5),
        max_overall_download_limit: settings.overall_download,
        max_overall_upload_limit: settings.overall_upload,
        session_file: settings.save.clone(),
        download_options: settings.download,
    })?;
    if let Some(path) = &settings.load {
        manager.load_session(path).await?;
    }
    for request in requests {
        manager.add(request).await?;
    }
    if settings.rpc {
        let address = if settings.listen_all {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        };
        let listener =
            tokio::net::TcpListener::bind((address, settings.port.unwrap_or(6800))).await?;
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let server = rpc::serve(listener, manager.clone(), settings.secret, cancel);
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result?,
            result = tokio::signal::ctrl_c() => { result?; signal.cancel(); server.await?; }
        }
        manager.shutdown().await?;
        return Ok(());
    }
    let interrupted = tokio::select! {
        _ = manager.wait_idle() => false,
        result = tokio::signal::ctrl_c() => { result?; true },
    };
    manager.shutdown().await?;
    let snapshots = manager.list().await;
    if settings.json {
        println!("{}", serde_json::to_string_pretty(&snapshots)?);
    } else {
        for snapshot in &snapshots {
            if let Some(error) = &snapshot.error {
                eprintln!("{}: {error}", snapshot.gid);
            } else if let Some(path) = &snapshot.path {
                println!("{}", path.display());
            }
        }
    }
    if interrupted {
        bail!("interrupted; unfinished downloads saved when --save-session is configured");
    }
    ensure!(
        !snapshots.iter().any(|s| s.status == DownloadStatus::Error),
        "one or more downloads failed"
    );
    Ok(())
}
