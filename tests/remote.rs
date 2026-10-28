use ariai::{
    engine,
    model::{DownloadOptions, DownloadProgress, DownloadRequest},
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

async fn ftp_server(
    content: Vec<u8>,
) -> (
    SocketAddr,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    ftp_server_with_changed_size(content, false).await
}

async fn ftp_server_with_changed_size(
    content: Vec<u8>,
    change: bool,
) -> (
    SocketAddr,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let captured = commands.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let content = content.clone();
            let captured = captured.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 ready\r\n").await.unwrap();
                let mut passive = None;
                let mut active = None;
                let mut offset = 0usize;
                let mut size_queries = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    captured.lock().unwrap().push(line.clone());
                    let (cmd, arg) = line.split_once(' ').unwrap_or((&line, ""));
                    let reply = match cmd {
                        "USER" => "331 password\r\n".to_string(),
                        "PASS" if arg == "secret" => "230 ok\r\n".to_string(),
                        "PASS" => "530 rejected\r\n".to_string(),
                        "TYPE" => "200 binary\r\n".to_string(),
                        "SIZE" => {
                            size_queries += 1;
                            format!(
                                "213 {}\r\n",
                                content.len() + usize::from(change && size_queries > 1)
                            )
                        }
                        "MDTM" => "213 20261028000000\r\n".to_string(),
                        "EPSV" => {
                            let data = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = data.local_addr().unwrap().port();
                            passive = Some(data);
                            format!("229 Entering Extended Passive Mode (|||{port}|)\r\n")
                        }
                        "EPRT" => {
                            let fields: Vec<_> = arg.split('|').collect();
                            active = Some(format!("{}:{}", fields[2], fields[3]));
                            "200 active\r\n".to_string()
                        }
                        "REST" => {
                            offset = arg.parse().unwrap();
                            "350 restart\r\n".to_string()
                        }
                        "RETR" => {
                            if write.write_all(b"150 opening\r\n").await.is_err() {
                                break;
                            }
                            let data = if let Some(listener) = passive.take() {
                                listener.accept().await.ok().map(|v| v.0)
                            } else if let Some(address) = active.take() {
                                TcpStream::connect(address).await.ok()
                            } else {
                                None
                            };
                            if let Some(mut data) = data {
                                let _ = data.write_all(&content[offset..]).await;
                                let _ = data.shutdown().await;
                            }
                            "226 done\r\n".to_string()
                        }
                        _ => "500 unsupported\r\n".to_string(),
                    };
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, commands, task)
}
fn request(addr: SocketAddr, dir: &std::path::Path) -> DownloadRequest {
    DownloadRequest {
        uris: vec![format!("ftp://user:secret@{addr}/file%20name")],
        options: DownloadOptions {
            dir: dir.to_path_buf(),
            out: Some("result".into()),
            max_tries: 1,
            retry_wait_secs: 0,
            ..Default::default()
        },
    }
}
fn progress() -> watch::Sender<DownloadProgress> {
    watch::channel(DownloadProgress::default()).0
}

#[tokio::test]
async fn ftp_passive_active_and_checksum() {
    let content = b"a real FTP transfer".to_vec();
    let (addr, commands, server) = ftp_server(content.clone()).await;
    for passive in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut req = request(addr, dir.path());
        req.options.ftp_pasv = passive;
        let result = engine::download(req, CancellationToken::new(), progress())
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(result.path).await.unwrap(), content);
    }
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    req.options.checksum = Some(format!("sha256={}", "0".repeat(64)));
    assert!(
        engine::download(req, CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert!(!dir.path().join("result").exists());
    let calls = commands.lock().unwrap();
    assert!(calls.iter().any(|v| v == "RETR /file name"));
    assert!(calls.iter().any(|v| v.starts_with("EPRT ")));
    server.abort();
}

#[tokio::test]
async fn ftp_resume_cancel_and_auth_failure() {
    let content = vec![42; 256 * 1024];
    let (addr, commands, server) = ftp_server(content.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    req.options.continue_download = true;
    req.options.max_download_limit = 64 * 1024;
    let token = CancellationToken::new();
    let (tx, mut rx) = watch::channel(DownloadProgress::default());
    let job = tokio::spawn(engine::download(req.clone(), token.clone(), tx));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            rx.changed().await.unwrap();
            if rx.borrow().completed_length > 0 {
                break;
            }
        }
    })
    .await
    .unwrap();
    token.cancel();
    assert!(job.await.unwrap().is_err());
    assert!(!dir.path().join("result").exists());
    req.options.max_download_limit = 0;
    let result = engine::download(req, CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), content);
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.starts_with("REST "))
    );
    let other = tempfile::tempdir().unwrap();
    let mut wrong = request(addr, other.path());
    wrong.options.ftp_passwd = Some("wrong".into());
    assert!(
        engine::download(wrong, CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert!(!other.path().join("result").exists());
    server.abort();
}

const TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn\nNhAAAAAwEAAQAAAQEAzXFzBprJRnloMqy6YBEIHGSkA2HhAkXPfNqb66NW4vZRo5jyVqB9\nStvPc1mnLVdMBLLsPhs+/QNd9DP+1a2rbkHbA112mGlFjX28GeD7cJ8tbSLbO9Ovqs9Fr9\n7PTWLCgIwObWNcmedCHRZp1o61ZO7ez+YPtrl+97UPjwyvr9pJ9P5vtIFpWfUpzlUzA3OI\nlGEy3ozWAC3nz7BqeIrP6shQV92Z6YvzA4GmqzCIHGYu3LjMbykpmrgPvTbOVGk5MlV61u\n3VmpwwYAxfBYIIsW1CVFDYgoNhtz6CHrcL+yjaaMJ6XNdNv0Fkm718k3J/tr+NhaFxMGSd\nx/kLPozW+QAAA7jZ6aUF2emlBQAAAAdzc2gtcnNhAAABAQDNcXMGmslGeWgyrLpgEQgcZK\nQDYeECRc982pvro1bi9lGjmPJWoH1K289zWactV0wEsuw+Gz79A130M/7VratuQdsDXXaY\naUWNfbwZ4Ptwny1tIts706+qz0Wv3s9NYsKAjA5tY1yZ50IdFmnWjrVk7t7P5g+2uX73tQ\n+PDK+v2kn0/m+0gWlZ9SnOVTMDc4iUYTLejNYALefPsGp4is/qyFBX3Znpi/MDgaarMIgc\nZi7cuMxvKSmauA+9Ns5UaTkyVXrW7dWanDBgDF8FggixbUJUUNiCg2G3PoIetwv7KNpown\npc102/QWSbvXyTcn+2v42FoXEwZJ3H+Qs+jNb5AAAAAwEAAQAAAQAXOOu3o2d8/9w3Xi7z\nDPnNu9KOT1IP37REeLGHQT9hCFbSF0fNYvbGr3ITp96t0WBWZ6MsogfMscS9BeOYY7ktDW\nSkTLxLHhsukzff9P53DIcl/vqpGJSSyjsmVhk4tiEmRq6aztB41Rs0DoSILDl030twVKES\nFoWzmlPX56c0SVRzbbqrZr2D458iK4TxmAa2kmFJk0EicGpgZWNC8zcHKhkAblPb1PdfFE\nirphsql3yPa1InoGrp/X/uHXsT3MjDxgXwOUPsQHG2t2kLKbdPCJ1Cci3wysXpzvGfytOK\neheuvUMTcLDSw4Vf7nadT9CR2pP4QJsAbfdWza1aQt3BAAAAgG+4pVf6y26Ki/Q41mD8a6\nNv3wFNTkTgMVWXNezJ2zGhuaZgYqLu5n6GS1s0qyZzW7xfwyowajttWH4haHqddi1jYC53\nrKE0vLHsBiMlc/pyvr0QueWyAXy0OKcZ9YzS36H2A5dznpLuwnkgI/Aw7RsfgnXp2YdmpS\n+jl3L7OSDYAAAAgQD3idiLn9RDCEXBf4foi6r5j2B5h9sOIX5ixdfAlDZtcMd8eRVOFJNl\n1GpqJRqeh5QM2oV657fqHlqouSM0ooTwSHYBDpQDGV43u+YHFqGkKb3OC24sN/RejNyLDA\naP8MSCv8JhvjWnlMT9iFI+Es+1AjAov0s0T2K2L3xDbk6kqwAAAIEA1Hc8oE88mYLa+7x2\nykqMjoPb9b8J4I3oBvIUQIy+wKqe9K5s6rh3nIaaAnhOEwahOqayb0jfvkv4bc1taHBgQS\ndJ2lO2kZ+AZDZbboOD+Gi5iOnNphtCem/h2LTQ/crjcDx1QGfDOsfqtF1ggzXFJVNqJaUA\n+fC7anjMQbvHCusAAAAAAQID\n-----END OPENSSH PRIVATE KEY-----\n";

#[derive(Clone, Default)]
struct SshServer {
    reads: Arc<Mutex<Vec<(u64, u32)>>>,
}
struct SshHandler {
    channels: std::collections::HashMap<russh::ChannelId, russh::Channel<russh::server::Msg>>,
    reads: Arc<Mutex<Vec<(u64, u32)>>>,
}
impl russh::server::Server for SshServer {
    type Handler = SshHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshHandler {
            channels: Default::default(),
            reads: self.reads.clone(),
        }
    }
}
impl russh::server::Handler for SshHandler {
    type Error = anyhow::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        if user == "user" && password == "secret" {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        _: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<russh::server::Auth, Self::Error> {
        if user == "user" {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }
    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: russh::ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            session.channel_success(id)?;
            let channel = self.channels.remove(&id).unwrap();
            let reads = self.reads.clone();
            tokio::spawn(async move {
                russh_sftp::server::run(channel.into_stream(), SftpFixture { reads }).await;
            });
        } else {
            session.channel_failure(id)?;
        }
        Ok(())
    }
}
struct SftpFixture {
    reads: Arc<Mutex<Vec<(u64, u32)>>>,
}
impl russh_sftp::server::Handler for SftpFixture {
    type Error = russh_sftp::protocol::StatusCode;
    fn unimplemented(&self) -> Self::Error {
        Self::Error::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        _: russh_sftp::protocol::OpenFlags,
        _: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        if filename != "/file name" {
            return Err(Self::Error::NoSuchFile);
        }
        Ok(russh_sftp::protocol::Handle {
            id,
            handle: "file".into(),
        })
    }
    async fn fstat(
        &mut self,
        id: u32,
        _: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: russh_sftp::protocol::FileAttributes {
                size: Some(14),
                mtime: Some(100),
                ..Default::default()
            },
        })
    }
    async fn read(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        self.reads.lock().unwrap().push((offset, len));
        let contents = b"SFTP loopback!";
        let start = offset as usize;
        if start >= contents.len() {
            return Err(Self::Error::Eof);
        }
        let end = (start + len as usize).min(contents.len());
        Ok(russh_sftp::protocol::Data {
            id,
            data: contents[start..end].to_vec(),
        })
    }
    async fn close(
        &mut self,
        id: u32,
        _: String,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
}
#[tokio::test]
async fn sftp_verified_host_authentication_and_transfer() {
    use russh::server::Server as _;
    use sha2::{Digest, Sha256};
    let key = russh::keys::PrivateKey::from_openssh(TEST_KEY).unwrap();
    let fingerprint = hex::encode(Sha256::digest(key.public_key().to_bytes().unwrap()));
    let public_key = key.public_key().to_openssh().unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let server_reads = reads.clone();
    let task = tokio::spawn(async move {
        let mut server = SshServer {
            reads: server_reads,
        };
        let _ = server.run_on_socket(config, &listener).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    req.uris = vec![format!("sftp://user:secret@{addr}/file%20name")];
    req.options.ssh_host_key_md = Some(format!("sha256={fingerprint}"));
    let result = engine::download(req.clone(), CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(result.path).await.unwrap(),
        b"SFTP loopback!"
    );
    let dir2 = tempfile::tempdir().unwrap();
    req.options.dir = dir2.path().to_owned();
    req.options.ssh_host_key_md = Some(format!("sha256={}", "0".repeat(64)));
    assert!(
        engine::download(req.clone(), CancellationToken::new(), progress())
            .await
            .is_err()
    );
    req.options.ssh_host_key_md = Some(format!("sha256={fingerprint}"));
    req.options.ftp_passwd = Some("bad".into());
    assert!(
        engine::download(req.clone(), CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert!(!dir2.path().join("result").exists());
    let known_hosts = dir2.path().join("known_hosts");
    tokio::fs::write(
        &known_hosts,
        format!("[127.0.0.1]:{} {}\n", addr.port(), public_key),
    )
    .await
    .unwrap();
    let private_key = dir2.path().join("client-key");
    tokio::fs::write(&private_key, TEST_KEY).await.unwrap();
    req.options.ssh_host_key_md = None;
    req.options.ssh_known_hosts = Some(known_hosts.clone());
    req.options.ssh_private_key = Some(private_key);
    let result = engine::download(req.clone(), CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(result.path).await.unwrap(),
        b"SFTP loopback!"
    );
    tokio::fs::write(known_hosts, "").await.unwrap();
    req.options.out = Some("unknown".into());
    assert!(
        engine::download(req.clone(), CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert!(!dir2.path().join("unknown").exists());
    let dir3 = tempfile::tempdir().unwrap();
    req.options.dir = dir3.path().to_owned();
    req.options.out = Some("result".into());
    req.options.ssh_known_hosts = None;
    req.options.ssh_host_key_md = Some(format!("sha256={fingerprint}"));
    req.options.ssh_private_key = None;
    req.options.ftp_passwd = Some("secret".into());
    set_pieces(&mut req, b"SFTP loopback!", 4);
    req.options.check_integrity = true;
    tokio::fs::write(dir3.path().join("result"), b"SFTPXXXXpback!")
        .await
        .unwrap();
    reads.lock().unwrap().clear();
    engine::download(req, CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir3.path().join("result")).await.unwrap(),
        b"SFTP loopback!"
    );
    let offsets = reads.lock().unwrap().clone();
    assert_eq!(offsets.len(), 1, "{offsets:?}");
    assert_eq!(offsets[0].0, 4);
    assert!(offsets[0].1 <= 4);
    task.abort();
}

fn set_pieces(req: &mut DownloadRequest, content: &[u8], size: usize) {
    use sha2::{Digest, Sha256};
    req.options.expected_length = Some(content.len() as u64);
    req.options.piece_hashes = Some(ariai::model::PieceHashes {
        length: size as u64,
        algorithm: "sha256".into(),
        hashes: content
            .chunks(size)
            .map(|piece| hex::encode(Sha256::digest(piece)))
            .collect(),
    });
}

#[tokio::test]
async fn ftp_piece_repair_preserves_target_on_failure() {
    let content = b"abcdefghijklm";
    let (addr, commands, server) = ftp_server(content.to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    set_pieces(&mut req, content, 4);
    req.options.check_integrity = true;
    let target = dir.path().join("result");
    tokio::fs::write(&target, b"abcdXXXXijklm").await.unwrap();
    engine::download(req.clone(), CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&target).await.unwrap(), content);
    let calls = commands.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|v| v.starts_with("REST "))
            .collect::<Vec<_>>(),
        vec!["REST 4"]
    );
    tokio::fs::write(&target, b"abcdXXXXijklm").await.unwrap();
    req.options.piece_hashes.as_mut().unwrap().hashes[1] = "0".repeat(64);
    assert!(
        engine::download(req, CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"abcdXXXXijklm");
    server.abort();
}

#[tokio::test]
async fn ftp_checksum_mismatch_fails_over_to_next_mirror() {
    use sha2::{Digest, Sha256};
    let (bad, _, bad_server) = ftp_server(b"bad data".to_vec()).await;
    let good_data = b"good data";
    let (good, _, good_server) = ftp_server(good_data.to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(bad, dir.path());
    req.uris
        .push(format!("ftp://user:secret@{good}/file%20name"));
    req.options.continue_download = true;
    req.options.checksum = Some(format!("sha256={}", hex::encode(Sha256::digest(good_data))));
    let result = engine::download(req, CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), good_data);
    bad_server.abort();
    good_server.abort();
}

#[tokio::test]
async fn ftp_repairs_complete_corrupt_partial_and_tries_next_mirror() {
    use sha2::{Digest, Sha256};
    let content = b"abcdefghijklm";
    let (bad, bad_calls, bad_server) = ftp_server(b"abcdZZZZijklm".to_vec()).await;
    let (good, good_calls, good_server) = ftp_server(content.to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(bad, dir.path());
    req.uris
        .push(format!("ftp://user:secret@{good}/file%20name"));
    req.options.continue_download = true;
    set_pieces(&mut req, content, 4);
    tokio::fs::write(
        dir.path().join("result.ariai.remote.part"),
        b"abcdXXXXijklm",
    )
    .await
    .unwrap();
    tokio::fs::write(
        dir.path().join("result.ariai.remote.json"),
        serde_json::to_vec(&serde_json::json!({
            "uri_hash": hex::encode(Sha256::digest(req.uris[0].as_bytes())),
            "size": content.len(), "modified": "20261028000000"
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let result = engine::download(req, CancellationToken::new(), progress())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), content);
    assert!(bad_calls.lock().unwrap().iter().any(|v| v == "REST 4"));
    assert!(good_calls.lock().unwrap().iter().any(|v| v == "REST 4"));
    bad_server.abort();
    good_server.abort();
}

#[tokio::test]
async fn ftp_repair_rejects_changed_source_and_cancellation() {
    let content = b"abcdefghijklm";
    let (addr, _, server) = ftp_server_with_changed_size(content.to_vec(), true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    set_pieces(&mut req, content, 4);
    req.options.check_integrity = true;
    let target = dir.path().join("result");
    tokio::fs::write(&target, b"abcdXXXXijklm").await.unwrap();
    assert!(
        engine::download(req.clone(), CancellationToken::new(), progress())
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"abcdXXXXijklm");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(engine::download(req, cancelled, progress()).await.is_err());
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"abcdXXXXijklm");
    server.abort();
}

#[tokio::test]
async fn ftp_cancellation_during_piece_repair_keeps_original() {
    let content = b"abcdefghijklm";
    let (addr, calls, server) = ftp_server(content.to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(addr, dir.path());
    set_pieces(&mut req, content, 4);
    req.options.check_integrity = true;
    req.options.max_download_limit = 1;
    let target = dir.path().join("result");
    tokio::fs::write(&target, b"abcdXXXXijklm").await.unwrap();
    let cancel = CancellationToken::new();
    let job = tokio::spawn(engine::download(req, cancel.clone(), progress()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if calls.lock().unwrap().iter().any(|v| v == "REST 4") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(job.await.unwrap().is_err());
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"abcdXXXXijklm");
    server.abort();
}

#[tokio::test]
async fn overall_limit_shares_capacity_between_http_and_ftp_downloads() {
    use ariai::{
        manager::DownloadManager,
        model::{DownloadStatus, ManagerOptions},
    };
    let payload = vec![42; 64 * 1024];
    let (ftp_addr, _, ftp_task) = ftp_server(payload.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    let http_task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let payload = payload.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                use tokio::io::AsyncReadExt;
                let _ = socket.read(&mut request).await;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.write_all(&payload).await;
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 3,
        max_overall_download_limit: 64 * 1024,
        ..Default::default()
    })
    .unwrap();
    let start = std::time::Instant::now();
    for i in 0..3 {
        let mut req = request(ftp_addr, dir.path());
        req.options.out = Some(format!("result{i}"));
        if i < 2 {
            req.uris = vec![format!("http://{http_addr}/file{i}")];
        }
        manager.add(req).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(12), manager.wait_idle())
        .await
        .unwrap();
    let elapsed = start.elapsed();
    println!("three transfers: {} bytes in {:?}", 3 * 64 * 1024, elapsed);
    let snapshots = manager.list().await;
    assert!(
        snapshots
            .iter()
            .all(|s| s.status == DownloadStatus::Complete),
        "{snapshots:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(2800),
        "aggregate rate exceeded limit: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(9),
        "unexpectedly slow: {elapsed:?}"
    );
    for i in 0..3 {
        assert_eq!(
            tokio::fs::read(dir.path().join(format!("result{i}")))
                .await
                .unwrap(),
            vec![42; 64 * 1024]
        );
    }
    ftp_task.abort();
    http_task.abort();
}

#[tokio::test]
async fn ftp_and_sftp_piece_repairs_share_overall_limit() {
    use ariai::{
        manager::DownloadManager,
        model::{DownloadStatus, ManagerOptions},
    };
    use russh::server::Server as _;
    use sha2::{Digest, Sha256};
    let content = b"SFTP loopback!";
    let (ftp_addr, commands, ftp_task) = ftp_server(content.to_vec()).await;
    let key = russh::keys::PrivateKey::from_openssh(TEST_KEY).unwrap();
    let fingerprint = hex::encode(Sha256::digest(key.public_key().to_bytes().unwrap()));
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ssh_addr = listener.local_addr().unwrap();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let server_reads = reads.clone();
    let ssh_task = tokio::spawn(async move {
        let mut server = SshServer {
            reads: server_reads,
        };
        let _ = server.run_on_socket(config, &listener).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 2,
        max_overall_download_limit: 4,
        ..Default::default()
    })
    .unwrap();
    let start = std::time::Instant::now();
    for (name, addr, scheme) in [("ftp", ftp_addr, "ftp"), ("sftp", ssh_addr, "sftp")] {
        tokio::fs::write(dir.path().join(name), b"SFTPXXXXpback!")
            .await
            .unwrap();
        let mut req = request(addr, dir.path());
        req.uris = vec![format!("{scheme}://user:secret@{addr}/file%20name")];
        req.options.out = Some(name.into());
        req.options.ssh_host_key_md = Some(format!("sha256={fingerprint}"));
        req.options.check_integrity = true;
        req.options.max_download_limit = 4;
        set_pieces(&mut req, content, 4);
        manager.add(req).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(8), manager.wait_idle())
        .await
        .unwrap();
    let elapsed = start.elapsed();
    let snapshots = manager.list().await;
    assert!(
        snapshots
            .iter()
            .all(|s| s.status == DownloadStatus::Complete),
        "{snapshots:?}"
    );
    assert!(elapsed >= Duration::from_millis(1800), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(6), "{elapsed:?}");
    for name in ["ftp", "sftp"] {
        assert_eq!(
            tokio::fs::read(dir.path().join(name)).await.unwrap(),
            content
        );
    }
    assert_eq!(*reads.lock().unwrap(), vec![(4, 4)]);
    let commands = commands.lock().unwrap();
    assert_eq!(
        commands.iter().filter(|s| s.starts_with("RETR ")).count(),
        1
    );
    assert!(commands.iter().any(|s| s == "REST 4"));
    println!("FTP and SFTP repairs: 8 missing bytes in {elapsed:?}");
    ftp_task.abort();
    ssh_task.abort();
}

#[tokio::test]
async fn cancelled_transfer_releases_overall_limiter() {
    use ariai::{
        manager::DownloadManager,
        model::{DownloadStatus, ManagerOptions},
    };
    let (addr, _, server) = ftp_server(vec![42; 64 * 1024]).await;
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 2,
        max_overall_download_limit: 16 * 1024,
        ..Default::default()
    })
    .unwrap();
    let mut first = request(addr, dir.path());
    first.options.out = Some("first".into());
    let gid = manager.add(first).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager.get(&gid).await.unwrap().completed_length > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), manager.pause(&gid))
        .await
        .unwrap()
        .unwrap();
    let mut second = request(addr, dir.path());
    second.options.out = Some("second".into());
    let second_gid = manager.add(second).await.unwrap();
    tokio::time::timeout(Duration::from_secs(7), manager.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        manager.get(&second_gid).await.unwrap().status,
        DownloadStatus::Complete
    );
    server.abort();
}

#[test]
fn unsupported_overall_upload_limit_is_rejected() {
    use ariai::{manager::DownloadManager, model::ManagerOptions};
    assert!(
        DownloadManager::new(ManagerOptions {
            max_overall_upload_limit: 1,
            ..Default::default()
        })
        .is_err()
    );
}

#[tokio::test]
async fn overall_download_limit_rejects_torrent() {
    use ariai::{
        manager::DownloadManager,
        model::{DownloadStatus, ManagerOptions},
    };
    let manager = DownloadManager::new(ManagerOptions {
        max_overall_download_limit: 65536,
        ..Default::default()
    })
    .unwrap();
    let req = DownloadRequest {
        uris: vec!["magnet:?xt=urn:btih:0123456789012345678901234567890123456789".into()],
        options: DownloadOptions::default(),
    };
    let gid = manager.add(req).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), manager.wait_idle())
        .await
        .unwrap();
    let result = manager.get(&gid).await.unwrap();
    assert_eq!(result.status, DownloadStatus::Error);
    assert!(
        result
            .error
            .unwrap()
            .contains("global download limit is unsupported for BitTorrent")
    );
}
