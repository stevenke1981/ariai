use std::{convert::Infallible, time::Duration};

use ariai::{
    manager::DownloadManager,
    model::{DownloadOptions, DownloadRequest, DownloadStatus, ManagerOptions},
    rpc,
};
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Response, header},
    routing::get,
};
use futures_util::stream;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

async fn slow_file() -> Response<Body> {
    let chunks = stream::unfold(0, |n| async move {
        if n == 32 {
            None
        } else {
            sleep(Duration::from_millis(30)).await;
            Some((Ok::<_, Infallible>(Bytes::from(vec![42; 1024])), n + 1))
        }
    });
    Response::builder()
        .header(header::CONTENT_LENGTH, "32768")
        .body(Body::from_stream(chunks))
        .unwrap()
}
async fn file_server() -> (String, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/file", get(slow_file)))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    (url, cancel)
}
async fn rpc_server(manager: DownloadManager) -> (String, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    tokio::spawn(rpc::serve(
        listener,
        manager,
        Some("secret".into()),
        cancel.clone(),
    ));
    (url, cancel)
}
async fn call(url: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn wait_status(manager: &DownloadManager, gid: &str, wanted: DownloadStatus) {
    timeout(Duration::from_secs(10), async {
        loop {
            if manager.get(gid).await.unwrap().status == wanted {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn request(url: &str, dir: &std::path::Path, name: &str) -> DownloadRequest {
    DownloadRequest {
        uris: vec![url.to_owned()],
        options: DownloadOptions {
            dir: dir.into(),
            out: Some(name.into()),
            split: 1,
            allow_overwrite: true,
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn rpc_download_authentication_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (url, file_cancel) = file_server().await;
    let manager = DownloadManager::new(ManagerOptions {
        download_options: DownloadOptions {
            dir: dir.path().into(),
            split: 1,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let (rpc_url, rpc_cancel) = rpc_server(manager.clone()).await;
    assert_eq!(
        call(&rpc_url, "aria2.getVersion", json!(["token:wrong"])).await["error"]["code"],
        1
    );
    assert_eq!(
        call(&rpc_url, "aria2.missing", json!(["token:secret"])).await["error"]["code"],
        -32601
    );
    assert_eq!(
        call(&rpc_url, "aria2.tellStatus", json!(["token:secret", 3])).await["error"]["code"],
        -32602
    );
    let methods = call(&rpc_url, "system.listMethods", json!(["token:secret"])).await;
    assert!(
        methods["result"]
            .as_array()
            .unwrap()
            .contains(&json!("aria2.addUri"))
    );
    let response = call(
        &rpc_url,
        "aria2.addUri",
        json!(["token:secret", [url], {"out":"one.bin"}]),
    )
    .await;
    let gid = response["result"].as_str().unwrap();
    let response = call(&rpc_url, "aria2.tellStatus", json!(["token:secret", gid])).await;
    assert_eq!(response["result"]["gid"], gid);
    timeout(Duration::from_secs(10), manager.wait_idle())
        .await
        .unwrap();
    let response = call(&rpc_url, "aria2.tellStatus", json!(["token:secret", gid])).await;
    assert_eq!(response["result"]["status"], "complete");
    assert_eq!(
        std::fs::metadata(dir.path().join("one.bin")).unwrap().len(),
        32768
    );
    manager.shutdown().await.unwrap();
    rpc_cancel.cancel();
    file_cancel.cancel();
}

#[tokio::test]
async fn queue_pause_resume_remove_and_session_reload() {
    let dir = tempfile::tempdir().unwrap();
    let (url, file_cancel) = file_server().await;
    let manager = DownloadManager::new(ManagerOptions {
        max_concurrent_downloads: 1,
        ..Default::default()
    })
    .unwrap();
    let a = manager
        .add(request(&url, dir.path(), "a.bin"))
        .await
        .unwrap();
    let b = manager
        .add(request(&url, dir.path(), "b.bin"))
        .await
        .unwrap();
    assert_eq!(
        manager.get(&a).await.unwrap().status,
        DownloadStatus::Active
    );
    assert_eq!(
        manager.get(&b).await.unwrap().status,
        DownloadStatus::Waiting
    );
    manager.pause(&b).await.unwrap();
    let copy = manager.clone();
    let id = a.clone();
    let pause = tokio::spawn(async move {
        copy.pause(&id).await.unwrap();
    });
    wait_status(&manager, &a, DownloadStatus::Paused).await;
    manager.unpause(&a).await.unwrap();
    pause.await.unwrap();
    wait_status(&manager, &a, DownloadStatus::Active).await;
    manager.remove(&a).await.unwrap();
    assert_eq!(
        manager.get(&a).await.unwrap().status,
        DownloadStatus::Removed
    );
    assert!(manager.unpause(&a).await.is_err());
    let session = dir.path().join("session.json");
    manager.save_session(&session).await.unwrap();
    let restored = DownloadManager::new(ManagerOptions::default()).unwrap();
    assert_eq!(restored.load_session(&session).await.unwrap(), 1);
    assert_eq!(
        restored.get(&b).await.unwrap().status,
        DownloadStatus::Paused
    );
    assert!(restored.load_session(&session).await.is_err());
    restored.unpause(&b).await.unwrap();
    timeout(Duration::from_secs(10), restored.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        restored.get(&b).await.unwrap().status,
        DownloadStatus::Complete
    );
    manager.shutdown().await.unwrap();
    restored.shutdown().await.unwrap();
    file_cancel.cancel();
}
