use ariai::{
    manager::DownloadManager,
    model::{DownloadOptions, ManagerOptions},
    rpc,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn call(method: &str, params: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodCall><methodName>{method}</methodName><params>{params}</params></methodCall>"
    )
}
fn param(value: &str) -> String {
    format!("<param><value>{value}</value></param>")
}
async fn post(url: &str, xml: &str) -> String {
    let response = reqwest::Client::new()
        .post(url)
        .header("content-type", "text/xml")
        .body(xml.to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/xml")
    );
    response.text().await.unwrap()
}
#[tokio::test]
async fn xml_rpc_authorization_metadata_multicall_and_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(ManagerOptions {
        download_options: DownloadOptions {
            dir: dir.path().into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/rpc", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let task = tokio::spawn(rpc::serve(
        listener,
        manager.clone(),
        Some("secret".into()),
        cancel.clone(),
    ));
    let unauthorized = post(&url, &call("aria2.getVersion", "")).await;
    assert!(unauthorized.contains("<fault>"));
    assert!(unauthorized.contains("Unauthorized"));
    let version = post(
        &url,
        &call("aria2.getVersion", &param("<string>token:secret</string>")),
    )
    .await;
    assert!(version.contains("<struct>"));
    assert!(version.contains("enabledFeatures"));
    let list = post(&url, &call("system.listMethods", "")).await;
    assert!(list.contains("system.multicall"));
    let metainfo = "<metalink xmlns=\"urn:ietf:params:xml:ns:metalink\"><file name=\"x&amp;y.bin\"><size>4</size><url>http://127.0.0.1:9/file</url></file></metalink>";
    let encoded = STANDARD.encode(metainfo);
    let options =
        "<struct><member><name>split</name><value><string>2</string></value></member></struct>";
    let request = call(
        "aria2.addMetalink",
        &format!(
            "{}{}{}",
            param("<string>token:secret</string>"),
            param(&format!("<base64>{encoded}</base64>")),
            param(options)
        ),
    );
    let result = post(&url, &request).await;
    assert!(result.contains("<array>"), "{result}");
    assert_eq!(manager.list().await.len(), 1);
    assert_eq!(manager.list().await[0].request.options.split, 2);
    assert_eq!(
        manager.list().await[0].request.options.out.as_deref(),
        Some("x&y.bin")
    );
    let call_one = "<value><struct><member><name>methodName</name><value><string>aria2.getVersion</string></value></member><member><name>params</name><value><array><data><value><string>token:secret</string></value></data></array></value></member></struct></value>";
    let call_two = "<value><struct><member><name>methodName</name><value><string>aria2.getVersion</string></value></member><member><name>params</name><value><array><data/></array></value></member></struct></value>";
    let multicall = post(
        &url,
        &call(
            "system.multicall",
            &param(&format!("<array><data>{call_one}{call_two}</data></array>")),
        ),
    )
    .await;
    assert!(multicall.contains("enabledFeatures"));
    assert!(multicall.contains("faultCode"));
    assert!(multicall.contains("Unauthorized"));
    let date = "<dateTime.iso8601>20240229T12:34:56</dateTime.iso8601>";
    let bad_date = post(
        &url,
        &call(
            "aria2.tellStatus",
            &format!("{}{}", param("<string>token:secret</string>"), param(date)),
        ),
    )
    .await;
    assert!(
        bad_date.contains("dateTime.iso8601 is not supported"),
        "{bad_date}"
    );
    let nil = post(
        &url,
        &call(
            "aria2.tellStatus",
            &format!(
                "{}{}",
                param("<string>token:secret</string>"),
                param("<nil/>")
            ),
        ),
    )
    .await;
    assert!(nil.contains("Invalid params"), "{nil}");
    let typed_call = format!(
        "<value><struct><member><name>methodName</name><value><string>aria2.tellStatus</string></value></member><member><name>params</name><value><array><data><value><string>token:secret</string></value><value>{date}</value></data></array></value></member></struct></value>"
    );
    let typed_multi = post(
        &url,
        &call(
            "system.multicall",
            &param(&format!(
                "<array><data>{typed_call}{call_one}</data></array>"
            )),
        ),
    )
    .await;
    assert!(
        typed_multi.contains("dateTime.iso8601 is not supported"),
        "{typed_multi}"
    );
    assert!(typed_multi.contains("enabledFeatures"), "{typed_multi}");
    assert!(post(&url, "<methodCall>").await.contains("Parse error"));
    cancel.cancel();
    task.await.unwrap().unwrap();
}
