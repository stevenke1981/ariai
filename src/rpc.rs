use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use axum::{
    Router,
    body::Bytes,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    config::apply_option,
    manager::DownloadManager,
    model::{DownloadOptions, DownloadRequest, DownloadSnapshot, DownloadStatus},
};

#[derive(Clone)]
struct RpcState {
    manager: DownloadManager,
    secret: Option<String>,
    shutdown: CancellationToken,
    session_id: String,
}

pub async fn serve(
    listener: TcpListener,
    manager: DownloadManager,
    secret: Option<String>,
    shutdown: CancellationToken,
) -> Result<()> {
    let state = Arc::new(RpcState {
        manager,
        secret,
        shutdown: shutdown.clone(),
        session_id: uuid::Uuid::new_v4().simple().to_string(),
    });
    let app = Router::new()
        .route("/jsonrpc", post(handle).get(websocket))
        .with_state(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;
    Ok(())
}
async fn handle(State(state): State<Arc<RpcState>>, body: Bytes) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::OK,
                axum::Json(error(Value::Null, -32700, "Parse error")),
            )
                .into_response();
        }
    };
    let mut authenticated = state.secret.is_none();
    match process(&state, value, &mut authenticated).await {
        Some(value) => (StatusCode::OK, axum::Json(value)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
async fn process(state: &RpcState, value: Value, authenticated: &mut bool) -> Option<Value> {
    if let Value::Array(batch) = value {
        if batch.is_empty() {
            Some(error(Value::Null, -32600, "Invalid Request"))
        } else {
            let mut responses = Vec::new();
            for request in batch {
                if let Some(result) = dispatch(state, request, authenticated).await {
                    responses.push(result);
                }
            }
            if responses.is_empty() {
                None
            } else {
                Some(Value::Array(responses))
            }
        }
    } else {
        dispatch(state, value, authenticated).await
    }
}
async fn websocket(State(state): State<Arc<RpcState>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| socket_loop(state, socket))
}
async fn socket_loop(state: Arc<RpcState>, mut socket: WebSocket) {
    let mut events = state.manager.subscribe();
    let mut authenticated = state.secret.is_none();
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else {break;};
                let bytes = match message { Message::Text(text)=>text.as_bytes().to_vec(), Message::Binary(bytes)=>bytes.to_vec(), Message::Close(_)=>break, _=>continue };
                let response = match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value)=>process(&state,value,&mut authenticated).await,
                    Err(_)=>Some(error(Value::Null,-32700,"Parse error")),
                };
                if let Some(response)=response && socket.send(Message::Text(response.to_string().into())).await.is_err() {break;}
            }
            event = events.recv() => {
                let Ok((gid,status)) = event else {break;};
                if !authenticated {continue;}
                let method = match status { DownloadStatus::Active=>"aria2.onDownloadStart",DownloadStatus::Paused=>"aria2.onDownloadPause",DownloadStatus::Removed=>"aria2.onDownloadStop",DownloadStatus::Complete=>"aria2.onDownloadComplete",DownloadStatus::Error=>"aria2.onDownloadError",DownloadStatus::Waiting=>continue };
                let notification=json!({"jsonrpc":"2.0","method":method,"params":[{"gid":gid}]});
                if socket.send(Message::Text(notification.to_string().into())).await.is_err() {break;}
            }
        }
    }
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
async fn dispatch(state: &RpcState, request: Value, authenticated: &mut bool) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc") != Some(&json!("2.0"))
        || !object.get("method").is_some_and(Value::is_string)
        || id
            .as_ref()
            .is_some_and(|v| !(v.is_string() || v.is_number() || v.is_null()))
    {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    }
    let mut params = match object.get("params") {
        None => Vec::new(),
        Some(Value::Array(v)) => v.clone(),
        Some(_) => return id.map(|id| error(id, -32602, "Invalid params")),
    };
    if let Some(secret) = &state.secret {
        let expected = format!("token:{secret}");
        if params.first().and_then(Value::as_str) != Some(expected.as_str()) {
            return id.map(|id| error(id, 1, "Unauthorized"));
        }
        params.remove(0);
    }
    *authenticated = true;
    let method = object["method"].as_str().unwrap_or_default();
    let result = execute(state, method, &params).await;
    id.map(|id| match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err((code, message)) => error(id, code, &message),
    })
}
type RpcResult = std::result::Result<Value, (i32, String)>;
fn invalid() -> (i32, String) {
    (-32602, "Invalid params".into())
}
fn failure(error: impl std::fmt::Display) -> (i32, String) {
    (1, error.to_string())
}
fn gid(params: &[Value]) -> std::result::Result<&str, (i32, String)> {
    params.first().and_then(Value::as_str).ok_or_else(invalid)
}
fn count(params: &[Value], min: usize, max: usize) -> std::result::Result<(), (i32, String)> {
    if (min..=max).contains(&params.len()) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn keys(value: Option<&Value>) -> std::result::Result<Option<Vec<&str>>, (i32, String)> {
    value
        .map(|v| {
            v.as_array()
                .ok_or_else(invalid)?
                .iter()
                .map(|x| x.as_str().ok_or_else(invalid))
                .collect()
        })
        .transpose()
}
fn status(snapshot: DownloadSnapshot, keys: Option<&[&str]>) -> Value {
    let name = match snapshot.status {
        DownloadStatus::Waiting => "waiting",
        DownloadStatus::Active => "active",
        DownloadStatus::Paused => "paused",
        DownloadStatus::Complete => "complete",
        DownloadStatus::Error => "error",
        DownloadStatus::Removed => "removed",
    };
    let path = snapshot
        .path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let uris: Vec<Value> = snapshot
        .request
        .uris
        .iter()
        .map(|uri| json!({"uri":uri,"status":"used"}))
        .collect();
    let mut map = serde_json::Map::new();
    map.insert("gid".into(), json!(snapshot.gid));
    map.insert("status".into(), json!(name));
    map.insert(
        "totalLength".into(),
        json!(snapshot.total_length.unwrap_or(0).to_string()),
    );
    map.insert(
        "completedLength".into(),
        json!(snapshot.completed_length.to_string()),
    );
    map.insert(
        "downloadSpeed".into(),
        json!(snapshot.download_speed.to_string()),
    );
    map.insert("uploadLength".into(), json!("0"));
    map.insert("uploadSpeed".into(), json!("0"));
    map.insert("connections".into(), json!("0"));
    map.insert(
        "dir".into(),
        json!(snapshot.request.options.dir.to_string_lossy()),
    );
    let files = if snapshot.files.is_empty() {
        json!([{"index":"1","path":path,"length":snapshot.total_length.unwrap_or(0).to_string(),"completedLength":snapshot.completed_length.to_string(),"selected":"true","uris":uris}])
    } else {
        json!(snapshot.files.iter().map(|file|json!({"index":file.index.to_string(),"path":file.path.to_string_lossy(),"length":file.length.to_string(),"completedLength":file.completed_length.to_string(),"selected":file.selected.to_string(),"uris":[]})).collect::<Vec<_>>())
    };
    map.insert("files".into(), files);
    if let Some(err) = snapshot.error {
        map.insert("errorCode".into(), json!("1"));
        map.insert("errorMessage".into(), json!(err));
    }
    if let Some(keys) = keys {
        map.retain(|key, _| keys.contains(&key.as_str()));
    }
    Value::Object(map)
}
fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
fn options(
    manager: &DownloadManager,
    value: Option<&Value>,
) -> std::result::Result<DownloadOptions, (i32, String)> {
    let mut options = manager.download_options();
    if let Some(value) = value {
        for (key, value) in value.as_object().ok_or_else(invalid)? {
            apply_option(&mut options, key, value.as_str().ok_or_else(invalid)?)
                .map_err(failure)?;
        }
    }
    options.validate().map_err(failure)?;
    Ok(options)
}
fn position(value: Option<&Value>) -> std::result::Result<Option<usize>, (i32, String)> {
    value
        .map(|v| {
            integer(v)
                .filter(|v| *v >= 0)
                .map(|v| v as usize)
                .ok_or_else(invalid)
        })
        .transpose()
}
fn option_values(options: &DownloadOptions) -> Value {
    json!({"dir":options.dir.to_string_lossy(),"out":options.out.as_deref().unwrap_or(""),"split":options.split.to_string(),"min-split-size":options.min_split_size.to_string(),"continue":options.continue_download.to_string(),"max-tries":options.max_tries.to_string(),"retry-wait":options.retry_wait_secs.to_string(),"timeout":options.timeout_secs.to_string(),"connect-timeout":options.connect_timeout_secs.to_string(),"max-download-limit":options.max_download_limit.to_string(),"allow-overwrite":options.allow_overwrite.to_string(),"auto-file-renaming":options.auto_file_renaming.to_string(),"user-agent":options.user_agent,"header":options.headers.join("\n"),"referer":options.referer.as_deref().unwrap_or(""),"all-proxy":options.proxy.as_deref().unwrap_or(""),"checksum":options.checksum.as_deref().unwrap_or(""),"ftp-pasv":options.ftp_pasv.to_string(),"select-file":options.select_file.as_deref().unwrap_or(""),"bt-tracker":options.bt_trackers.join(","),"enable-dht":options.bt_enable_dht.to_string(),"seed-time":(options.seed_time_secs as f64/60.0).to_string(),"follow-torrent":options.follow_torrent.to_string(),"follow-metalink":options.follow_metalink.to_string()})
}
async fn execute(state: &RpcState, method: &str, params: &[Value]) -> RpcResult {
    let manager = &state.manager;
    match method {
        "aria2.addUri" => {
            count(params, 1, 3)?;
            let uris: Vec<String> = params[0]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
                .collect::<std::result::Result<_, _>>()?;
            let options = options(manager, params.get(1))?;
            let position = position(params.get(2))?;
            let gid = manager
                .add_at(DownloadRequest { uris, options }, position)
                .await
                .map_err(failure)?;
            Ok(json!(gid))
        }
        "aria2.addMetalink" => {
            count(params, 1, 3)?;
            let bytes = STANDARD
                .decode(params[0].as_str().ok_or_else(invalid)?)
                .map_err(failure)?;
            let options = options(manager, params.get(1))?;
            let requests = crate::metalink::parse(&bytes, None, &options).map_err(failure)?;
            let position = position(params.get(2))?;
            let mut gids = Vec::new();
            for (i, request) in requests.into_iter().enumerate() {
                gids.push(
                    manager
                        .add_at(request, position.map(|p| p.saturating_add(i)))
                        .await
                        .map_err(failure)?,
                );
            }
            Ok(json!(gids))
        }
        "aria2.addTorrent" => {
            count(params, 1, 4)?;
            let bytes = STANDARD
                .decode(params[0].as_str().ok_or_else(invalid)?)
                .map_err(failure)?;
            if let Some(uris) = params.get(1)
                && !uris.as_array().ok_or_else(invalid)?.is_empty()
            {
                return Err(failure("torrent web-seed URI injection is not implemented"));
            }
            let options = options(manager, params.get(2))?;
            let position = position(params.get(3))?;
            let folder = options.dir.join(".ariai-metadata");
            tokio::fs::create_dir_all(&folder).await.map_err(failure)?;
            let path = folder.join(format!("{}.torrent", hex::encode(Sha256::digest(&bytes))));
            tokio::fs::write(&path, bytes).await.map_err(failure)?;
            let path = tokio::fs::canonicalize(path).await.map_err(failure)?;
            let gid = manager
                .add_at(
                    DownloadRequest {
                        uris: vec![path.to_string_lossy().into_owned()],
                        options,
                    },
                    position,
                )
                .await
                .map_err(failure)?;
            Ok(json!(gid))
        }
        "aria2.getUris" | "aria2.getFiles" | "aria2.getOption" => {
            count(params, 1, 1)?;
            let item = manager
                .get(gid(params)?)
                .await
                .ok_or_else(|| failure("download not found"))?;
            if method == "aria2.getOption" {
                return Ok(option_values(&item.request.options));
            }
            if method == "aria2.getUris" {
                return Ok(json!(
                    item.request
                        .uris
                        .iter()
                        .map(|uri| json!({"uri":uri,"status":"used"}))
                        .collect::<Vec<_>>()
                ));
            }
            Ok(status(item, None)["files"].clone())
        }
        "aria2.getGlobalOption" => {
            count(params, 0, 0)?;
            Ok(option_values(&manager.download_options()))
        }
        "aria2.changeOption" => {
            count(params, 2, 2)?;
            let gid = gid(params)?;
            let mut options = manager
                .get(gid)
                .await
                .ok_or_else(|| failure("download not found"))?
                .request
                .options;
            for (key, value) in params[1].as_object().ok_or_else(invalid)? {
                apply_option(&mut options, key, value.as_str().ok_or_else(invalid)?)
                    .map_err(failure)?;
            }
            manager
                .change_options(gid, options)
                .await
                .map_err(failure)?;
            Ok(json!("OK"))
        }
        "aria2.changePosition" => {
            count(params, 3, 3)?;
            let index = manager
                .change_position(
                    gid(params)?,
                    integer(&params[1]).ok_or_else(invalid)?,
                    params[2].as_str().ok_or_else(invalid)?,
                )
                .await
                .map_err(failure)?;
            Ok(json!(index))
        }
        "aria2.tellStatus" => {
            count(params, 1, 2)?;
            let keys = keys(params.get(1))?;
            let item = manager
                .get(gid(params)?)
                .await
                .ok_or_else(|| failure("download not found"))?;
            Ok(status(item, keys.as_deref()))
        }
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => {
            let active = method == "aria2.tellActive";
            count(
                params,
                if active { 0 } else { 2 },
                if active { 1 } else { 3 },
            )?;
            let requested_keys = keys(params.get(if active { 0 } else { 2 }))?;
            let mut items: Vec<_> = manager
                .list()
                .await
                .into_iter()
                .filter(|s| match method {
                    "aria2.tellActive" => s.status == DownloadStatus::Active,
                    "aria2.tellWaiting" => {
                        matches!(s.status, DownloadStatus::Waiting | DownloadStatus::Paused)
                    }
                    _ => matches!(
                        s.status,
                        DownloadStatus::Complete | DownloadStatus::Error | DownloadStatus::Removed
                    ),
                })
                .collect();
            if !active {
                let offset = integer(&params[0]).ok_or_else(invalid)?;
                let length = integer(&params[1])
                    .filter(|n| *n >= 0)
                    .ok_or_else(invalid)? as usize;
                let start = if offset < 0 {
                    (items.len() as i64 + offset).max(0) as usize
                } else {
                    offset as usize
                };
                items = items.into_iter().skip(start).take(length).collect();
            }
            Ok(Value::Array(
                items
                    .into_iter()
                    .map(|item| status(item, requested_keys.as_deref()))
                    .collect(),
            ))
        }
        "aria2.pause" | "aria2.forcePause" | "aria2.unpause" | "aria2.remove"
        | "aria2.forceRemove" => {
            count(params, 1, 1)?;
            let gid = gid(params)?;
            match method {
                "aria2.pause" | "aria2.forcePause" => manager.pause(gid).await,
                "aria2.unpause" => manager.unpause(gid).await,
                _ => manager.remove(gid).await,
            }
            .map_err(failure)?;
            Ok(json!(gid))
        }
        "aria2.pauseAll" | "aria2.forcePauseAll" | "aria2.unpauseAll" => {
            count(params, 0, 0)?;
            for item in manager.list().await {
                if method != "aria2.unpauseAll"
                    && matches!(
                        item.status,
                        DownloadStatus::Active | DownloadStatus::Waiting
                    )
                {
                    manager.pause(&item.gid).await.map_err(failure)?;
                }
                if method == "aria2.unpauseAll" && item.status == DownloadStatus::Paused {
                    manager.unpause(&item.gid).await.map_err(failure)?;
                }
            }
            Ok(json!("OK"))
        }
        "aria2.removeDownloadResult" => {
            count(params, 1, 1)?;
            manager.remove_result(gid(params)?).await.map_err(failure)?;
            Ok(json!("OK"))
        }
        "aria2.purgeDownloadResult" => {
            count(params, 0, 0)?;
            manager.purge_results().await;
            Ok(json!("OK"))
        }
        "aria2.getGlobalStat" => {
            count(params, 0, 0)?;
            let mut counters: HashMap<&str, u64> = HashMap::new();
            let mut speed = 0;
            for item in manager.list().await {
                let key = match item.status {
                    DownloadStatus::Active => "numActive",
                    DownloadStatus::Waiting | DownloadStatus::Paused => "numWaiting",
                    _ => "numStopped",
                };
                *counters.entry(key).or_default() += 1;
                speed += item.download_speed;
            }
            Ok(
                json!({"downloadSpeed":speed.to_string(),"uploadSpeed":"0","numActive":counters.get("numActive").unwrap_or(&0).to_string(),"numWaiting":counters.get("numWaiting").unwrap_or(&0).to_string(),"numStopped":counters.get("numStopped").unwrap_or(&0).to_string()}),
            )
        }
        "system.listMethods" => {
            count(params, 0, 0)?;
            Ok(json!([
                "system.listMethods",
                "aria2.addUri",
                "aria2.addMetalink",
                "aria2.addTorrent",
                "aria2.getUris",
                "aria2.getFiles",
                "aria2.getOption",
                "aria2.getGlobalOption",
                "aria2.changeOption",
                "aria2.changePosition",
                "aria2.forcePause",
                "aria2.forcePauseAll",
                "aria2.forceRemove",
                "aria2.forceShutdown",
                "system.listNotifications",
                "aria2.tellStatus",
                "aria2.tellActive",
                "aria2.tellWaiting",
                "aria2.tellStopped",
                "aria2.pause",
                "aria2.unpause",
                "aria2.remove",
                "aria2.pauseAll",
                "aria2.unpauseAll",
                "aria2.removeDownloadResult",
                "aria2.purgeDownloadResult",
                "aria2.getGlobalStat",
                "aria2.getVersion",
                "aria2.getSessionInfo",
                "aria2.saveSession",
                "aria2.shutdown"
            ]))
        }
        "system.listNotifications" => {
            count(params, 0, 0)?;
            Ok(json!([
                "aria2.onDownloadStart",
                "aria2.onDownloadPause",
                "aria2.onDownloadStop",
                "aria2.onDownloadComplete",
                "aria2.onDownloadError"
            ]))
        }
        "aria2.getVersion" => {
            count(params, 0, 0)?;
            Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"enabledFeatures":["HTTPS","SFTP","BitTorrent","Metalink","WebSocket"]}),
            )
        }
        "aria2.getSessionInfo" => {
            count(params, 0, 0)?;
            Ok(json!({"sessionId":state.session_id}))
        }
        "aria2.saveSession" => {
            count(params, 0, 0)?;
            manager.save_default_session().await.map_err(failure)?;
            Ok(json!("OK"))
        }
        "aria2.shutdown" | "aria2.forceShutdown" => {
            count(params, 0, 0)?;
            manager.shutdown().await.map_err(failure)?;
            state.shutdown.cancel();
            Ok(json!("OK"))
        }
        _ => Err((-32601, "Method not found".into())),
    }
}
