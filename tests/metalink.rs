use ariai::{metalink::parse, model::DownloadOptions};
use url::Url;

const SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

#[test]
fn v4_relative_priority_hash_pieces_and_selection() {
    let xml = format!(
        r#"<?xml version="1.0"?><m:metalink xmlns:m="urn:ietf:params:xml:ns:metalink"><m:file name="nested/a &amp; b.txt"><m:size>3</m:size><m:hash type="sha-1">{SHA1}</m:hash><m:hash type="sha-256"> {SHA256} </m:hash><m:pieces type="sha-256" length="3"><m:hash>{SHA256}</m:hash></m:pieces><m:url priority="5">later</m:url><m:url priority="1">../first?x=1&amp;y=2</m:url></m:file><m:file name="other"><m:url>https://example.test/file</m:url></m:file></m:metalink>"#
    );
    let defaults = DownloadOptions {
        select_file: Some("1".into()),
        split: 2,
        ..Default::default()
    };
    let result = parse(
        xml.as_bytes(),
        Some(&Url::parse("https://example.test/dir/list.meta4").unwrap()),
        &defaults,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    let request = &result[0];
    assert_eq!(
        request.uris,
        [
            "https://example.test/first?x=1&y=2",
            "https://example.test/dir/later"
        ]
    );
    assert_eq!(request.options.out.as_deref(), Some("nested/a & b.txt"));
    assert_eq!(
        request.options.checksum.as_deref(),
        Some(format!("sha-256={SHA256}").as_str())
    );
    assert_eq!(request.options.expected_length, Some(3));
    assert_eq!(
        request.options.piece_hashes.as_ref().unwrap().hashes,
        [SHA256]
    );
    assert!(request.options.select_file.is_none());
    assert_eq!(request.options.split, 2);
}

#[test]
fn v3_resources_and_preference() {
    let xml = format!(
        r#"<metalink xmlns="http://www.metalinker.org/" version="3.0"><files><file name="a.bin"><size>3</size><verification><hash type="sha1">{SHA1}</hash><pieces length="3" type="sha256"><hash piece="0">{SHA256}</hash></pieces></verification><resources><url preference="2">ftp://example.test/a</url><url preference="100">sftp://example.test/a</url></resources></file></files></metalink>"#
    );
    let requests = parse(xml.as_bytes(), None, &DownloadOptions::default()).unwrap();
    assert_eq!(
        requests[0].uris,
        ["sftp://example.test/a", "ftp://example.test/a"]
    );
    assert_eq!(
        requests[0].options.checksum.as_deref(),
        Some(format!("sha-1={SHA1}").as_str())
    );
}

#[test]
fn invalid_documents_are_rejected() {
    let wrap = |file: &str| {
        format!(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">{file}</metalink>"#)
    };
    let invalid = [
        "<metalink><file name='a'><url>https://x/a</url></file></metalink>".to_owned(),
        "<metalink xmlns='urn:ietf:params:xml:ns:metalink' version='3.0'/>".to_owned(),
        "<!DOCTYPE metalink [<!ENTITY x SYSTEM 'file:///secret'>]><metalink xmlns='urn:ietf:params:xml:ns:metalink'>&x;</metalink>".to_owned(),
        wrap("<file name='../evil'><url>https://x/a</url></file>"),
        wrap("<file name='x\\evil'><url>https://x/a</url></file>"),
        wrap("<file name='a'><hash type='sha-256'>bad</hash><url>https://x/a</url></file>"),
        wrap("<file name='a'><hash type='sha-512'>1234</hash><url>https://x/a</url></file>"),
        wrap(&format!("<file name='a'><size>4</size><pieces type='sha-256' length='3'><hash>{SHA256}</hash></pieces><url>https://x/a</url></file>")),
        wrap("<file name='a'><url>relative</url></file>"),
        "<metalink xmlns='urn:ietf:params:xml:ns:metalink'><file></metalink>".to_owned(),
        wrap("<file xmlns='wrong' name='a'><url>https://x/a</url></file>"),
        wrap("<file name='a'><url>https://x/a</url></file><file name='b'>"),
    ];
    for doc in invalid {
        assert!(
            parse(doc.as_bytes(), None, &DownloadOptions::default()).is_err(),
            "accepted {doc}"
        );
    }
    let xml = wrap("<file name='a'><url>https://x/a</url></file>");
    let options = DownloadOptions {
        select_file: Some("2".into()),
        ..Default::default()
    };
    assert!(parse(xml.as_bytes(), None, &options).is_err());
}

#[test]
fn numeric_entities_cdata_and_whitespace() {
    let xml = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="a&#46;txt"><url> https://example.test/<![CDATA[file]]>?a=1&amp;b=2 </url></file></metalink>"#;
    let parsed = parse(xml.as_bytes(), None, &DownloadOptions::default()).unwrap();
    assert_eq!(parsed[0].options.out.as_deref(), Some("a.txt"));
    assert_eq!(parsed[0].uris[0], "https://example.test/file?a=1&b=2");
}

#[tokio::test]
async fn expanded_request_downloads_and_verifies_from_http_mirror() {
    use ariai::{engine::download, model::DownloadProgress};
    use axum::{Router, http::StatusCode, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{net::TcpListener, sync::watch};
    use tokio_util::sync::CancellationToken;
    let hits = Arc::new(AtomicUsize::new(0));
    let bad_hits = hits.clone();
    let app = Router::new()
        .route(
            "/bad",
            get(move || {
                let hits = bad_hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        )
        .route("/good", get(|| async { "abc" }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let xml = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="file.txt"><size>3</size><hash type="sha-256">{SHA256}</hash><pieces type="sha-256" length="3"><hash>{SHA256}</hash></pieces><url priority="1">http://{address}/bad</url><url priority="2">http://{address}/good</url></file></metalink>"#
    );
    let dir = tempfile::tempdir().unwrap();
    let options = DownloadOptions {
        dir: dir.path().to_owned(),
        retry_wait_secs: 0,
        max_tries: 1,
        split: 1,
        ..Default::default()
    };
    let request = parse(xml.as_bytes(), None, &options).unwrap().remove(0);
    let (progress, _) = watch::channel(DownloadProgress::default());
    let result = download(request, CancellationToken::new(), progress)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(result.path).await.unwrap(), b"abc");
    assert!(hits.load(Ordering::SeqCst) > 0);
    for failure in ["length", "whole", "piece"] {
        let mut request = parse(xml.as_bytes(), None, &options).unwrap().remove(0);
        request.options.out = Some(format!("{failure}.txt"));
        match failure {
            "length" => request.options.expected_length = Some(4),
            "whole" => request.options.checksum = Some(format!("sha-256={}", "0".repeat(64))),
            "piece" => request.options.piece_hashes.as_mut().unwrap().hashes[0] = "0".repeat(64),
            _ => unreachable!(),
        }
        let (progress, _) = watch::channel(DownloadProgress::default());
        assert!(
            download(request, CancellationToken::new(), progress)
                .await
                .is_err(),
            "{failure} mismatch accepted"
        );
        assert!(!dir.path().join(format!("{failure}.txt")).exists());
    }
    server.abort();
}
