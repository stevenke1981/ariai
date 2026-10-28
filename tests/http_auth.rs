use ariai::{http::HttpSession, model::DownloadOptions};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
};
use md5::Md5;
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct TestState {
    algorithm: &'static str,
    count: Arc<AtomicUsize>,
    stale: bool,
}
fn field<'a>(auth: &'a str, name: &str) -> Option<&'a str> {
    auth.strip_prefix("Digest ")?.split(", ").find_map(|part| {
        part.strip_prefix(&format!("{name}="))
            .map(|v| v.trim_matches('"'))
    })
}
async fn digest(State(s): State<TestState>, headers: HeaderMap) -> Response {
    s.count.fetch_add(1, Ordering::SeqCst);
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hash = |v: &str| {
        if s.algorithm.starts_with("MD5") {
            hex::encode(Md5::digest(v.as_bytes()))
        } else {
            hex::encode(Sha256::digest(v.as_bytes()))
        }
    };
    let valid = (|| {
        let nonce = field(auth, "nonce")?;
        let uri = field(auth, "uri")?;
        let cnonce = field(auth, "cnonce")?;
        let mut ha1 = hash("alice:test-realm:secret");
        if s.algorithm.ends_with("-sess") {
            ha1 = hash(&format!("{ha1}:{nonce}:{cnonce}"));
        }
        let ha2 = hash(&format!("GET:{uri}"));
        let expected = hash(&format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}"));
        Some(
            field(auth, "username")? == "alice"
                && uri == "/digest?value=1"
                && nonce == "nonce-one"
                && field(auth, "response")? == expected,
        )
    })()
    .unwrap_or(false);
    if valid && !s.stale {
        return Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("ok"))
            .unwrap();
    }
    Response::builder().status(StatusCode::UNAUTHORIZED).header(header::WWW_AUTHENTICATE, format!("Digest realm=\"test-realm\", nonce=\"nonce-one\", algorithm={}, qop=\"auth\", stale={}", s.algorithm, s.stale)).body(Body::empty()).unwrap()
}
async fn digest_redirect() -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/digest?value=1")
        .body(Body::empty())
        .unwrap()
}
async fn start(s: TestState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/digest", get(digest))
                .route("/go", get(digest_redirect))
                .with_state(s),
        )
        .into_future(),
    );
    format!("http://{addr}")
}
#[tokio::test]
async fn digest_algorithms_and_wrong_password() {
    for algorithm in ["MD5", "MD5-sess", "SHA-256", "SHA-256-sess"] {
        let state = TestState {
            algorithm,
            count: Arc::new(AtomicUsize::new(0)),
            stale: false,
        };
        let base = start(state.clone()).await;
        let options = DownloadOptions {
            http_user: Some("alice".into()),
            http_passwd: Some("secret".into()),
            http_auth_challenge: true,
            ..Default::default()
        };
        let session = HttpSession::new(&options).unwrap();
        assert_eq!(
            session
                .send(session.request(reqwest::Method::GET, &format!("{base}/digest?value=1")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "{algorithm}"
        );
        assert_eq!(state.count.load(Ordering::SeqCst), 2);
        let bad = HttpSession::new(&DownloadOptions {
            http_passwd: Some("wrong".into()),
            ..options
        })
        .unwrap();
        assert_eq!(
            bad.send(bad.request(reqwest::Method::GET, &format!("{base}/digest?value=1")))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(state.count.load(Ordering::SeqCst), 4);
    }
}
#[tokio::test]
async fn stale_digest_retry_is_bounded_and_unsupported_is_explicit() {
    for algorithm in ["MD5", "SHA-512"] {
        let state = TestState {
            algorithm,
            count: Arc::new(AtomicUsize::new(0)),
            stale: true,
        };
        let base = start(state.clone()).await;
        let session = HttpSession::new(&DownloadOptions {
            http_user: Some("alice".into()),
            http_passwd: Some("secret".into()),
            http_auth_challenge: true,
            ..Default::default()
        })
        .unwrap();
        let result = session
            .send(session.request(reqwest::Method::GET, &format!("{base}/digest?value=1")))
            .await;
        if algorithm == "MD5" {
            assert_eq!(result.unwrap().status(), StatusCode::UNAUTHORIZED);
            assert_eq!(state.count.load(Ordering::SeqCst), 4);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported Digest algorithm")
            );
            assert_eq!(state.count.load(Ordering::SeqCst), 1);
        }
    }
}
async fn redirect() -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/private/file")
        .header(header::SET_COOKIE, "fresh=present; Path=/private; HttpOnly")
        .body(Body::empty())
        .unwrap()
}
async fn cookie_echo(headers: HeaderMap) -> String {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}
#[tokio::test]
async fn netscape_cookie_scope_redirect_and_roundtrip() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/redirect", get(redirect))
                .route("/private/file", get(cookie_echo))
                .route("/other", get(cookie_echo)),
        )
        .into_future(),
    );
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cookies.txt");
    let output = dir.path().join("saved.txt");
    std::fs::write(&input, "# Netscape HTTP Cookie File\n127.0.0.1\tFALSE\t/private\tFALSE\t0\tloaded\tyes\n127.0.0.1\tFALSE\t/\tTRUE\t0\tsecure\tno\n127.0.0.1\tFALSE\t/\tFALSE\t1\texpired\tno\n").unwrap();
    let options = DownloadOptions {
        load_cookies: Some(input),
        save_cookies: Some(output.clone()),
        ..Default::default()
    };
    let session = HttpSession::new(&options).unwrap();
    let base = format!("http://{addr}");
    let text = session
        .send(session.request(reqwest::Method::GET, &format!("{base}/redirect")))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("loaded=yes") && text.contains("fresh=present"));
    assert!(!text.contains("secure=") && !text.contains("expired="));
    assert!(
        session
            .send(session.request(reqwest::Method::GET, &format!("{base}/other")))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .is_empty()
    );
    session.save_cookies().await.unwrap();
    let saved = std::fs::read_to_string(&output).unwrap();
    assert!(saved.contains("#HttpOnly_127.0.0.1\tFALSE\t/private"));
    let reloaded = HttpSession::new(&DownloadOptions {
        load_cookies: Some(output),
        ..Default::default()
    })
    .unwrap();
    let text = reloaded
        .send(reloaded.request(reqwest::Method::GET, &format!("{base}/private/file")))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("loaded=yes") && text.contains("fresh=present"));
}
#[tokio::test]
async fn engine_uses_digest_session() {
    let state = TestState {
        algorithm: "SHA-256",
        count: Arc::new(AtomicUsize::new(0)),
        stale: false,
    };
    let base = start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let options = DownloadOptions {
        dir: dir.path().to_path_buf(),
        out: Some("result".into()),
        http_user: Some("alice".into()),
        http_passwd: Some("secret".into()),
        http_auth_challenge: true,
        retry_wait_secs: 0,
        ..Default::default()
    };
    let request = ariai::model::DownloadRequest {
        uris: vec![format!("{base}/digest?value=1")],
        options,
    };
    let (progress, _) = tokio::sync::watch::channel(ariai::model::DownloadProgress::default());
    let result = ariai::engine::download(
        request,
        tokio_util::sync::CancellationToken::new(),
        progress,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(result.path).unwrap(), b"ok");
}
async fn cross_redirect(State(target): State<String>) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, target)
        .body(Body::empty())
        .unwrap()
}
async fn auth_echo(headers: HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}
#[tokio::test]
async fn credentials_are_not_forwarded_across_origins() {
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://{}/target", target_listener.local_addr().unwrap());
    tokio::spawn(
        axum::serve(
            target_listener,
            Router::new().route("/target", get(auth_echo)),
        )
        .into_future(),
    );
    let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = format!("http://{}/redirect", source_listener.local_addr().unwrap());
    tokio::spawn(
        axum::serve(
            source_listener,
            Router::new()
                .route("/redirect", get(cross_redirect))
                .with_state(target),
        )
        .into_future(),
    );
    let session = HttpSession::new(&DownloadOptions {
        http_user: Some("alice".into()),
        http_passwd: Some("secret".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(
        session
            .send(session.request(reqwest::Method::GET, &source))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .is_empty()
    );
}
async fn set_parallel(axum::extract::Path(id): axum::extract::Path<usize>) -> Response {
    Response::builder()
        .header(header::SET_COOKIE, format!("c{id}=v{id}; Path=/"))
        .body(Body::empty())
        .unwrap()
}
#[tokio::test]
async fn parallel_responses_update_the_same_cookie_jar() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/set/{id}", get(set_parallel))
                .route("/show", get(cookie_echo)),
        )
        .into_future(),
    );
    let session = HttpSession::new(&DownloadOptions::default()).unwrap();
    let requests = (0..8)
        .map(|i| session.send(session.request(reqwest::Method::GET, &format!("{base}/set/{i}"))));
    for result in futures_util::future::join_all(requests).await {
        assert!(result.unwrap().status().is_success());
    }
    let text = session
        .send(session.request(reqwest::Method::GET, &format!("{base}/show")))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for i in 0..8 {
        assert!(
            text.contains(&format!("c{i}=v{i}")),
            "missing cookie {i}: {text}"
        );
    }
}
#[test]
fn unsupported_cookie_format_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.txt");
    std::fs::write(&file, "[{\"name\":\"bad\"}]").unwrap();
    let result = HttpSession::new(&DownloadOptions {
        load_cookies: Some(file),
        ..Default::default()
    });
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("expected Netscape format")
    );
}

#[tokio::test]
async fn digest_challenge_after_same_origin_redirect() {
    let state = TestState {
        algorithm: "MD5",
        count: Arc::new(AtomicUsize::new(0)),
        stale: false,
    };
    let base = start(state.clone()).await;
    let session = HttpSession::new(&DownloadOptions {
        http_user: Some("alice".into()),
        http_passwd: Some("secret".into()),
        http_auth_challenge: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        session
            .send(session.request(reqwest::Method::GET, &format!("{base}/go")))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(state.count.load(Ordering::SeqCst), 2);
}

async fn digest_auth_int(headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hash = |v: &[u8]| hex::encode(Sha256::digest(v));
    let valid = (|| {
        let nonce = field(auth, "nonce")?;
        let cnonce = field(auth, "cnonce")?;
        let uri = field(auth, "uri")?;
        let ha1 = hash(b"alice:test-realm:secret");
        let ha2 = hash(format!("POST:{uri}:{}", hash(&body)).as_bytes());
        let expected = hash(format!("{ha1}:{nonce}:00000001:{cnonce}:auth-int:{ha2}").as_bytes());
        Some(
            body.as_ref() == b"nonempty body\x00bytes"
                && uri == "/auth-int?x=1"
                && nonce == "int-nonce"
                && field(auth, "qop")? == "auth-int"
                && field(auth, "response")? == expected,
        )
    })()
    .unwrap_or(false);
    if valid {
        return Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .unwrap();
    }
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            header::WWW_AUTHENTICATE,
            "Digest realm=\"test-realm\", nonce=\"int-nonce\", algorithm=SHA-256, qop=\"auth-int\"",
        )
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn digest_auth_int_post_hashes_actual_binary_body_and_rejects_wrong_password() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/auth-int?x=1", listener.local_addr().unwrap());
    tokio::spawn(
        axum::serve(
            listener,
            Router::new().route("/auth-int", axum::routing::post(digest_auth_int)),
        )
        .into_future(),
    );
    for (password, status) in [
        ("secret", StatusCode::OK),
        ("wrong", StatusCode::UNAUTHORIZED),
    ] {
        let session = HttpSession::new(&DownloadOptions {
            http_user: Some("alice".into()),
            http_passwd: Some(password.into()),
            http_auth_challenge: true,
            ..Default::default()
        })
        .unwrap();
        let response = session
            .send(
                session
                    .request(reqwest::Method::POST, &url)
                    .body(&b"nonempty body\x00bytes"[..]),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}

#[tokio::test]
async fn sqlite_browser_cookies_and_wal_are_loaded_with_scope_and_expiry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/private/file", get(cookie_echo))
                .route("/other", get(cookie_echo)),
        )
        .into_future(),
    );
    for (file, expected) in [
        ("firefox.sqlite", "firefox=yes"),
        ("chromium.sqlite", "chromium=yes"),
        ("wal.sqlite", "wal=yes"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/http_cookies")
            .join(file);
        let session = HttpSession::new(&DownloadOptions {
            load_cookies: Some(path),
            ..Default::default()
        })
        .unwrap();
        let text = session
            .send(session.request(reqwest::Method::GET, &format!("{base}/private/file")))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(text.contains(expected), "{file}: {text}");
        assert!(!text.contains("secure=") && !text.contains("expired="));
        assert!(
            session
                .send(session.request(reqwest::Method::GET, &format!("{base}/other")))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn encrypted_chromium_cookie_is_explicitly_rejected() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/http_cookies/encrypted.sqlite");
    let result = HttpSession::new(&DownloadOptions {
        load_cookies: Some(path),
        ..Default::default()
    });
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("encrypted_value requires OS decryption")
    );
}
