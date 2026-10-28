use ariai::{
    manager::DownloadManager,
    model::{DownloadOptions, DownloadRequest, DownloadStatus, ManagerOptions},
    rpc,
};
use axum::{Router, routing::get};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::time::Duration;
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

async fn setup() -> (
    tempfile::TempDir,
    DownloadManager,
    String,
    String,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let data_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let data_url = format!("http://{}", data_listener.local_addr().unwrap());
    let app = Router::new()
        .route("/file", get(|| async { vec![42u8; 1024] }))
        .route(
            "/hold",
            get(|| async { std::future::pending::<String>().await }),
        );
    let data_task = tokio::spawn(async move {
        axum::serve(data_listener, app).await.unwrap();
    });
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 1,
        download_options: DownloadOptions {
            dir: dir.path().into(),
            split: 1,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc_url = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    tokio::spawn(rpc::serve(
        listener,
        manager.clone(),
        Some("secret".into()),
        cancel.clone(),
    ));
    (dir, manager, rpc_url, data_url, cancel, data_task)
}
async fn call(url: &str, method: &str, params: Vec<Value>) -> Value {
    let mut authenticated = vec![json!("token:secret")];
    authenticated.extend(params);
    reqwest::Client::new()
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":authenticated}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn rpc_metadata_queue_options_and_download() {
    let (dir, manager, rpc_url, data_url, cancel, data_task) = setup().await;
    let blocker = manager
        .add(DownloadRequest {
            uris: vec![format!("{data_url}/hold")],
            options: manager.download_options(),
        })
        .await
        .unwrap();
    let xml = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="meta.bin"><size>1024</size><url>{data_url}/file</url></file></metalink>"#
    );
    let response = call(
        &rpc_url,
        "aria2.addMetalink",
        vec![json!(STANDARD.encode(xml)), json!({}), json!(0)],
    )
    .await;
    let meta = response["result"][0]
        .as_str()
        .expect("metalink gid")
        .to_owned();
    let response = call(
        &rpc_url,
        "aria2.addUri",
        vec![
            json!([format!("{data_url}/file")]),
            json!({"out":"other.bin"}),
            json!(0),
        ],
    )
    .await;
    let other = response["result"].as_str().unwrap().to_owned();
    assert_eq!(
        call(
            &rpc_url,
            "aria2.changePosition",
            vec![json!(meta), json!(0), json!("POS_SET")]
        )
        .await["result"],
        0
    );
    assert_eq!(
        call(
            &rpc_url,
            "aria2.tellWaiting",
            vec![json!(0), json!(10), json!(["gid"])]
        )
        .await["result"][0]["gid"],
        meta
    );
    assert_eq!(
        call(
            &rpc_url,
            "aria2.changeOption",
            vec![json!(other), json!({"out":"changed.bin","split":"2"})]
        )
        .await["result"],
        "OK"
    );
    assert_eq!(
        call(&rpc_url, "aria2.getOption", vec![json!(other)]).await["result"]["split"],
        "2"
    );
    assert!(
        call(
            &rpc_url,
            "aria2.changeOption",
            vec![json!(blocker), json!({"out":"bad.bin"})]
        )
        .await
        .get("error")
        .is_some()
    );
    let bad = call(&rpc_url, "aria2.addMetalink", vec![json!("not base64")]).await;
    assert!(bad.get("error").is_some());
    assert_eq!(
        call(&rpc_url, "aria2.forceRemove", vec![json!(blocker)]).await["result"],
        blocker
    );
    timeout(Duration::from_secs(10), manager.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("meta.bin")).unwrap(),
        vec![42u8; 1024]
    );
    assert_eq!(
        std::fs::read(dir.path().join("changed.bin")).unwrap(),
        vec![42u8; 1024]
    );
    let files = call(&rpc_url, "aria2.getFiles", vec![json!(meta)]).await;
    assert_eq!(files["result"][0]["length"], "1024");
    assert_eq!(
        manager.get(&other).await.unwrap().status,
        DownloadStatus::Complete
    );
    manager.shutdown().await.unwrap();
    cancel.cancel();
    data_task.abort();
}

#[tokio::test]
async fn websocket_auth_batch_and_download_notifications() {
    let (_dir, manager, rpc_url, data_url, cancel, data_task) = setup().await;
    let (mut socket, _) = connect_async(rpc_url.replacen("http:", "ws:", 1))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            json!({"jsonrpc":"2.0","id":1,"method":"aria2.getVersion","params":["token:bad"]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let message = socket.next().await.unwrap().unwrap();
    let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(value["error"]["message"], "Unauthorized");
    manager
        .add(DownloadRequest {
            uris: vec![format!("{data_url}/file")],
            options: DownloadOptions {
                out: Some("unauth.bin".into()),
                ..manager.download_options()
            },
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), manager.wait_idle())
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(50), socket.next())
            .await
            .is_err()
    );
    socket.send(Message::Text(json!([{"jsonrpc":"2.0","id":2,"method":"aria2.getVersion","params":["token:secret"]},{"jsonrpc":"2.0","id":3,"method":"system.listNotifications","params":["token:secret"]}]).to_string().into())).await.unwrap();
    let message = socket.next().await.unwrap().unwrap();
    let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(value[0]["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        value[1]["result"]
            .as_array()
            .unwrap()
            .contains(&json!("aria2.onDownloadComplete"))
    );
    let gid = manager
        .add(DownloadRequest {
            uris: vec![format!("{data_url}/file")],
            options: DownloadOptions {
                out: Some("authenticated.bin".into()),
                ..manager.download_options()
            },
        })
        .await
        .unwrap();
    let mut methods = Vec::new();
    timeout(Duration::from_secs(5), async {
        while methods.len() < 2 {
            let message = socket.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(value["params"][0]["gid"], gid);
            methods.push(value["method"].as_str().unwrap().to_owned());
        }
    })
    .await
    .unwrap();
    assert_eq!(
        methods,
        ["aria2.onDownloadStart", "aria2.onDownloadComplete"]
    );
    socket.close(None).await.unwrap();
    manager.shutdown().await.unwrap();
    cancel.cancel();
    data_task.abort();
}

#[tokio::test]
async fn rpc_torrent_metadata_persists_and_existing_data_is_verified() {
    let (dir, manager, rpc_url, _data_url, cancel, data_task) = setup().await;
    let data = vec![42u8; 1024];
    std::fs::write(dir.path().join("seed.bin"), &data).unwrap();
    let mut metadata =
        b"d4:infod6:lengthi1024e4:name8:seed.bin12:piece lengthi16384e6:pieces20:".to_vec();
    metadata.extend(Sha1::digest(&data));
    metadata.extend(b"ee");
    let response = call(
        &rpc_url,
        "aria2.addTorrent",
        vec![
            json!(STANDARD.encode(&metadata)),
            json!([]),
            json!({"continue":"true","enable-dht":"false"}),
        ],
    )
    .await;
    let gid = response["result"].as_str().unwrap();
    timeout(Duration::from_secs(10), manager.wait_idle())
        .await
        .unwrap();
    let snapshot = manager.get(gid).await.unwrap();
    assert_eq!(
        snapshot.status,
        DownloadStatus::Complete,
        "{:?}",
        snapshot.error
    );
    assert!(std::path::Path::new(&snapshot.request.uris[0]).exists());
    let files = call(&rpc_url, "aria2.getFiles", vec![json!(gid)]).await;
    assert_eq!(files["result"][0]["length"], "1024");
    assert_eq!(files["result"][0]["completedLength"], "1024");
    assert!(
        call(
            &rpc_url,
            "aria2.addTorrent",
            vec![
                json!(STANDARD.encode(&metadata)),
                json!(["http://unsupported/webseed"])
            ]
        )
        .await
        .get("error")
        .is_some()
    );
    assert!(
        call(&rpc_url, "aria2.addTorrent", vec![json!("invalid base64")])
            .await
            .get("error")
            .is_some()
    );
    manager.shutdown().await.unwrap();
    cancel.cancel();
    data_task.abort();
}

#[tokio::test]
async fn multicall_authenticates_each_nested_call() {
    let (_dir, _manager, rpc_url, _data_url, cancel, data_task) = setup().await;
    let body = json!({"jsonrpc":"2.0", "id":7, "method":"system.multicall", "params":[[
        {"methodName":"aria2.getVersion", "params":["token:secret"]},
        {"methodName":"aria2.getVersion", "params":[]},
        {"methodName":"system.listMethods", "params":[]},
        {"methodName":"system.multicall", "params":[[]]},
        {"methodName":"missing", "params":["token:secret"]},
        {"methodName":"aria2.getVersion", "params":"bad"}
    ]]});
    let response: Value = reqwest::Client::new()
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let result = response["result"].as_array().unwrap();
    assert!(result[0][0]["version"].is_string());
    assert_eq!(result[1]["faultString"], "Unauthorized");
    assert!(
        result[2][0]
            .as_array()
            .unwrap()
            .contains(&json!("system.multicall"))
    );
    assert_eq!(result[3]["faultCode"], -32600);
    assert_eq!(result[4]["faultCode"], -32601);
    assert_eq!(result[5]["faultCode"], -32602);
    cancel.cancel();
    data_task.abort();
}
