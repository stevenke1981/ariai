use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    engine,
    model::{
        DownloadOptions, DownloadProgress, DownloadRequest, DownloadSnapshot, DownloadStatus,
        ManagerOptions,
    },
};

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<Inner>,
}
struct Inner {
    options: ManagerOptions,
    state: Mutex<State>,
    changed: Notify,
    events: broadcast::Sender<(String, DownloadStatus)>,
}
struct State {
    entries: Vec<Entry>,
    shutting_down: bool,
}
struct Entry {
    snapshot: DownloadSnapshot,
    progress: watch::Receiver<DownloadProgress>,
    cancel: Option<CancellationToken>,
    finished: Option<watch::Receiver<bool>>,
}
#[derive(Serialize, Deserialize)]
struct Session {
    version: u32,
    downloads: Vec<SessionEntry>,
}
#[derive(Serialize, Deserialize)]
struct SessionEntry {
    gid: String,
    request: DownloadRequest,
    paused: bool,
}

impl DownloadManager {
    pub fn new(options: ManagerOptions) -> Result<Self> {
        ensure!(
            options.max_concurrent_downloads > 0,
            "max concurrent downloads must be positive"
        );
        options.download_options.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                state: Mutex::new(State {
                    entries: Vec::new(),
                    shutting_down: false,
                }),
                changed: Notify::new(),
                events: broadcast::channel(256).0,
            }),
        })
    }
    pub fn download_options(&self) -> DownloadOptions {
        self.inner.options.download_options.clone()
    }
    pub async fn add(&self, request: DownloadRequest) -> Result<String> {
        self.add_at(request, None).await
    }
    pub fn subscribe(&self) -> broadcast::Receiver<(String, DownloadStatus)> {
        self.inner.events.subscribe()
    }
    pub async fn add_at(
        &self,
        request: DownloadRequest,
        position: Option<usize>,
    ) -> Result<String> {
        request.validate()?;
        let gid = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
        let mut state = self.inner.state.lock().await;
        ensure!(!state.shutting_down, "manager is shutting down");
        let index = position
            .and_then(|p| {
                state
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        matches!(
                            e.snapshot.status,
                            DownloadStatus::Waiting | DownloadStatus::Paused
                        )
                    })
                    .nth(p)
                    .map(|(i, _)| i)
            })
            .unwrap_or(state.entries.len());
        state.entries.insert(
            index,
            Self::entry(gid.clone(), request, DownloadStatus::Waiting),
        );
        self.schedule(&mut state);
        Ok(gid)
    }
    fn entry(gid: String, request: DownloadRequest, status: DownloadStatus) -> Entry {
        let (_, progress) = watch::channel(DownloadProgress::default());
        Entry {
            snapshot: DownloadSnapshot {
                gid,
                status,
                request,
                total_length: None,
                completed_length: 0,
                download_speed: 0,
                upload_length: 0,
                upload_speed: 0,
                connections: 0,
                path: None,
                error: None,
                files: Vec::new(),
            },
            progress,
            cancel: None,
            finished: None,
        }
    }
    fn schedule(&self, state: &mut State) {
        if state.shutting_down {
            return;
        }
        let mut active = state
            .entries
            .iter()
            .filter(|e| e.finished.is_some())
            .count();
        for entry in &mut state.entries {
            if active >= self.inner.options.max_concurrent_downloads {
                break;
            }
            if entry.snapshot.status != DownloadStatus::Waiting || entry.finished.is_some() {
                continue;
            }
            active += 1;
            entry.snapshot.status = DownloadStatus::Active;
            entry.snapshot.error = None;
            let (progress_tx, progress_rx) = watch::channel(DownloadProgress::default());
            entry.progress = progress_rx;
            let cancel = CancellationToken::new();
            entry.cancel = Some(cancel.clone());
            let (finished_tx, finished_rx) = watch::channel(false);
            entry.finished = Some(finished_rx);
            let request = entry.snapshot.request.clone();
            let gid = entry.snapshot.gid.clone();
            let _ = self
                .inner
                .events
                .send((gid.clone(), DownloadStatus::Active));
            let manager = self.clone();
            tokio::spawn(async move {
                let result = engine::download(request, cancel, progress_tx).await;
                let mut state = manager.inner.state.lock().await;
                if let Some(entry) = state.entries.iter_mut().find(|e| e.snapshot.gid == gid) {
                    let progress = entry.progress.borrow().clone();
                    entry.snapshot.total_length = progress.total_length;
                    entry.snapshot.completed_length = progress.completed_length;
                    entry.snapshot.path = progress.path;
                    entry.snapshot.files = progress.files;
                    entry.snapshot.download_speed = 0;
                    entry.snapshot.upload_length = progress.upload_length;
                    entry.snapshot.upload_speed = 0;
                    entry.snapshot.connections = 0;
                    if entry.snapshot.status == DownloadStatus::Active {
                        match result {
                            Ok(result) => {
                                entry.snapshot.status = DownloadStatus::Complete;
                                entry.snapshot.total_length = Some(result.total_length);
                                entry.snapshot.completed_length = result.total_length;
                                entry.snapshot.path = Some(result.path);
                                entry.snapshot.files = result.files;
                            }
                            Err(error) => {
                                entry.snapshot.status = DownloadStatus::Error;
                                entry.snapshot.error = Some(format!("{error:#}"));
                            }
                        }
                        let _ = manager
                            .inner
                            .events
                            .send((gid.clone(), entry.snapshot.status));
                    }
                    entry.cancel = None;
                    entry.finished = None;
                }
                let _ = finished_tx.send(true);
                manager.schedule(&mut state);
                manager.inner.changed.notify_waiters();
            });
        }
    }
    fn snapshot(entry: &Entry) -> DownloadSnapshot {
        let mut snapshot = entry.snapshot.clone();
        if snapshot.status == DownloadStatus::Active {
            let p = entry.progress.borrow();
            snapshot.total_length = p.total_length;
            snapshot.completed_length = p.completed_length;
            snapshot.download_speed = p.download_speed;
            snapshot.upload_length = p.upload_length;
            snapshot.upload_speed = p.upload_speed;
            snapshot.connections = p.connections;
            snapshot.path = p.path.clone();
            snapshot.files = p.files.clone();
        }
        snapshot
    }
    pub async fn get(&self, gid: &str) -> Option<DownloadSnapshot> {
        self.inner
            .state
            .lock()
            .await
            .entries
            .iter()
            .find(|e| e.snapshot.gid == gid)
            .map(Self::snapshot)
    }
    pub async fn list(&self) -> Vec<DownloadSnapshot> {
        self.inner
            .state
            .lock()
            .await
            .entries
            .iter()
            .map(Self::snapshot)
            .collect()
    }
    pub async fn pause(&self, gid: &str) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
        ensure!(
            matches!(
                entry.snapshot.status,
                DownloadStatus::Waiting | DownloadStatus::Active
            ),
            "download cannot be paused"
        );
        entry.snapshot.status = DownloadStatus::Paused;
        if let Some(cancel) = &entry.cancel {
            cancel.cancel();
        }
        let finished = entry.finished.clone();
        drop(state);
        self.inner.changed.notify_waiters();
        Self::wait_finished(finished).await;
        let _ = self
            .inner
            .events
            .send((gid.to_owned(), DownloadStatus::Paused));
        Ok(())
    }
    async fn wait_finished(finished: Option<watch::Receiver<bool>>) {
        if let Some(mut finished) = finished {
            while !*finished.borrow_and_update() {
                if finished.changed().await.is_err() {
                    break;
                }
            }
        }
    }
    pub async fn unpause(&self, gid: &str) -> Result<()> {
        loop {
            let mut state = self.inner.state.lock().await;
            ensure!(!state.shutting_down, "manager is shutting down");
            let entry = state
                .entries
                .iter_mut()
                .find(|e| e.snapshot.gid == gid)
                .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
            ensure!(
                entry.snapshot.status == DownloadStatus::Paused,
                "download is not paused"
            );
            if let Some(finished) = entry.finished.clone() {
                drop(state);
                Self::wait_finished(Some(finished)).await;
            } else {
                entry.snapshot.status = DownloadStatus::Waiting;
                self.schedule(&mut state);
                return Ok(());
            }
        }
    }
    pub async fn remove(&self, gid: &str) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
        ensure!(
            matches!(
                entry.snapshot.status,
                DownloadStatus::Waiting | DownloadStatus::Active | DownloadStatus::Paused
            ),
            "download cannot be removed"
        );
        entry.snapshot.status = DownloadStatus::Removed;
        if let Some(cancel) = &entry.cancel {
            cancel.cancel();
        }
        let finished = entry.finished.clone();
        drop(state);
        self.inner.changed.notify_waiters();
        Self::wait_finished(finished).await;
        let _ = self
            .inner
            .events
            .send((gid.to_owned(), DownloadStatus::Removed));
        Ok(())
    }
    pub async fn change_options(&self, gid: &str, options: DownloadOptions) -> Result<()> {
        options.validate()?;
        let mut state = self.inner.state.lock().await;
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
        ensure!(
            matches!(
                entry.snapshot.status,
                DownloadStatus::Waiting | DownloadStatus::Paused
            ) && entry.finished.is_none(),
            "options may be changed only for waiting or fully paused downloads"
        );
        entry.snapshot.request.options = options;
        Ok(())
    }
    pub async fn change_uris(
        &self,
        gid: &str,
        file_index: usize,
        del: &[String],
        add: &[String],
        position: Option<usize>,
    ) -> Result<(usize, usize)> {
        ensure!(file_index == 1, "only file index 1 is supported");
        let mut state = self.inner.state.lock().await;
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
        ensure!(
            matches!(
                entry.snapshot.status,
                DownloadStatus::Waiting | DownloadStatus::Paused
            ) && entry.finished.is_none(),
            "URIs can only be changed for waiting or fully paused downloads"
        );
        let mut request = entry.snapshot.request.clone();
        ensure!(
            !request.uris.iter().chain(add).any(|uri| {
                let parsed = url::Url::parse(uri).ok();
                uri.starts_with("magnet:")
                    || parsed.as_ref().is_some_and(|url| url.scheme() == "file")
                    || (!uri.contains("://")
                        && std::path::Path::new(uri)
                            .extension()
                            .is_some_and(|s| s.eq_ignore_ascii_case("torrent")))
                    || (request.options.follow_torrent
                        && parsed.as_ref().is_some_and(|url| {
                            matches!(url.scheme(), "http" | "https")
                                && url.path().ends_with(".torrent")
                        }))
            }),
            "changing torrent URIs is not supported"
        );
        let mut removed = 0;
        for uri in del {
            if let Some(index) = request.uris.iter().position(|existing| existing == uri) {
                request.uris.remove(index);
                removed += 1;
            }
        }
        let index = position
            .unwrap_or(request.uris.len())
            .min(request.uris.len());
        request.uris.splice(index..index, add.iter().cloned());
        request.validate()?;
        entry.snapshot.request = request;
        Ok((removed, add.len()))
    }
    pub async fn change_position(&self, gid: &str, position: i64, how: &str) -> Result<usize> {
        let mut state = self.inner.state.lock().await;
        let queue: Vec<usize> = state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e.snapshot.status,
                    DownloadStatus::Waiting | DownloadStatus::Paused
                )
            })
            .map(|(i, _)| i)
            .collect();
        let current = queue
            .iter()
            .position(|i| state.entries[*i].snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download is not in waiting queue"))?;
        let target = match how {
            "POS_SET" => position,
            "POS_CUR" => (current as i64).saturating_add(position),
            "POS_END" => (queue.len() as i64 - 1).saturating_add(position),
            _ => bail!("invalid position mode"),
        }
        .clamp(0, queue.len() as i64 - 1) as usize;
        let entry = state.entries.remove(queue[current]);
        let index = state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e.snapshot.status,
                    DownloadStatus::Waiting | DownloadStatus::Paused
                )
            })
            .nth(target)
            .map(|(i, _)| i)
            .unwrap_or(state.entries.len());
        state.entries.insert(index, entry);
        Ok(target)
    }
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let state = self.inner.state.lock().await;
            if !state.entries.iter().any(|e| {
                e.finished.is_some()
                    || (!state.shutting_down && e.snapshot.status == DownloadStatus::Waiting)
            }) {
                return;
            }
            drop(state);
            notified.await;
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        state.shutting_down = true;
        let mut pending = Vec::new();
        for entry in &mut state.entries {
            if entry.snapshot.status == DownloadStatus::Active {
                entry.snapshot.status = DownloadStatus::Waiting;
            }
            if let Some(cancel) = &entry.cancel {
                cancel.cancel();
            }
            if let Some(finished) = &entry.finished {
                pending.push(finished.clone());
            }
        }
        drop(state);
        self.inner.changed.notify_waiters();
        for finished in pending {
            Self::wait_finished(Some(finished)).await;
        }
        if let Some(path) = &self.inner.options.session_file {
            self.save_session(path).await?;
        }
        Ok(())
    }
    pub async fn save_session(&self, path: &Path) -> Result<()> {
        let state = self.inner.state.lock().await;
        let session = Session {
            version: 1,
            downloads: state
                .entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.snapshot.status,
                        DownloadStatus::Active | DownloadStatus::Waiting | DownloadStatus::Paused
                    )
                })
                .map(|e| SessionEntry {
                    gid: e.snapshot.gid.clone(),
                    request: e.snapshot.request.clone(),
                    paused: e.snapshot.status == DownloadStatus::Paused,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&session)?;
        drop(state);
        tokio::fs::write(path, bytes).await?;
        Ok(())
    }
    pub async fn load_session(&self, path: &Path) -> Result<usize> {
        let bytes = tokio::fs::read(path).await?;
        let session: Session =
            if bytes.iter().copied().find(|c| !c.is_ascii_whitespace()) == Some(b'{') {
                serde_json::from_slice(&bytes)?
            } else {
                let text = std::str::from_utf8(&bytes).context("session must be UTF-8")?;
                let mut downloads: Vec<SessionEntry> = Vec::new();
                for (index, raw) in text.lines().enumerate() {
                    let line = raw.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if raw.starts_with(char::is_whitespace) {
                        let (key, value) = line.split_once('=').with_context(|| {
                            format!("session line {}: expected key=value", index + 1)
                        })?;
                        let entry = downloads.last_mut().with_context(|| {
                            format!("session line {}: option without URI", index + 1)
                        })?;
                        match key.trim() {
                            "gid" => entry.gid = value.trim().to_owned(),
                            "pause" => entry.paused = crate::config::parse_bool(value.trim())?,
                            key => crate::config::apply_option(
                                &mut entry.request.options,
                                key,
                                value.trim(),
                            )
                            .with_context(|| format!("session line {}", index + 1))?,
                        }
                    } else {
                        downloads.push(SessionEntry {
                            gid: uuid::Uuid::new_v4().simple().to_string()[..16].to_owned(),
                            request: DownloadRequest {
                                uris: line.split_whitespace().map(str::to_owned).collect(),
                                options: self.download_options(),
                            },
                            paused: false,
                        });
                    }
                }
                Session {
                    version: 1,
                    downloads,
                }
            };
        ensure!(session.version == 1, "unsupported session version");
        let mut ids = std::collections::HashSet::new();
        for entry in &session.downloads {
            entry.request.validate()?;
            ensure!(
                entry.gid.len() == 16 && entry.gid.bytes().all(|c| c.is_ascii_hexdigit()),
                "invalid session gid"
            );
            ensure!(ids.insert(entry.gid.clone()), "duplicate session gid");
        }
        let count = session.downloads.len();
        let mut state = self.inner.state.lock().await;
        ensure!(!state.shutting_down, "manager is shutting down");
        if state
            .entries
            .iter()
            .any(|entry| ids.contains(&entry.snapshot.gid))
        {
            bail!("session contains existing gid");
        }
        for item in session.downloads {
            state.entries.push(Self::entry(
                item.gid,
                item.request,
                if item.paused {
                    DownloadStatus::Paused
                } else {
                    DownloadStatus::Waiting
                },
            ));
        }
        self.schedule(&mut state);
        Ok(count)
    }
    pub async fn remove_result(&self, gid: &str) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        let index = state
            .entries
            .iter()
            .position(|e| e.snapshot.gid == gid)
            .ok_or_else(|| anyhow::anyhow!("download not found: {gid}"))?;
        ensure!(
            matches!(
                state.entries[index].snapshot.status,
                DownloadStatus::Complete | DownloadStatus::Error | DownloadStatus::Removed
            ) && state.entries[index].finished.is_none(),
            "download has not stopped"
        );
        state.entries.remove(index);
        Ok(())
    }
    pub async fn purge_results(&self) {
        self.inner.state.lock().await.entries.retain(|e| {
            !matches!(
                e.snapshot.status,
                DownloadStatus::Complete | DownloadStatus::Error | DownloadStatus::Removed
            ) || e.finished.is_some()
        });
    }
    pub async fn save_default_session(&self) -> Result<()> {
        let path = self
            .inner
            .options
            .session_file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session file is not configured"))?;
        self.save_session(path).await
    }
}
