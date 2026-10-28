use ariai::{config::apply_option, model::DownloadOptions};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    thread,
};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ariai"))
}

fn server(expected: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        for stream in listener.incoming().take(expected) {
            let mut stream = stream.unwrap();
            let mut buffer = [0; 8192];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .unwrap();
        }
    });
    (format!("http://{address}/file"), handle)
}

#[test]
fn config_options_validate_and_append_headers() {
    let mut options = DownloadOptions::default();
    apply_option(&mut options, "min-split-size", "64KiB").unwrap();
    apply_option(&mut options, "header", "X-A: a").unwrap();
    apply_option(&mut options, "header", "X-B: b").unwrap();
    assert_eq!(options.min_split_size, 65536);
    assert_eq!(options.headers.len(), 2);
    assert!(apply_option(&mut options, "unknown", "true").is_err());
    assert!(apply_option(&mut options, "continue", "yes").is_err());
    assert!(apply_option(&mut options, "min-split-size", "2Z").is_err());
}

#[test]
fn help_version_and_rejections_exit_correctly() {
    for flag in ["--help", "--version"] {
        assert!(
            binary().arg(flag).output().unwrap().status.success(),
            "{flag}"
        );
    }
    for args in [vec!["--bogus"], vec!["gopher://localhost/file"], vec![]] {
        assert!(!binary().args(args).output().unwrap().status.success());
    }
}

#[test]
fn input_precedence_and_mirrors_download() {
    let temp = tempfile::tempdir().unwrap();
    let (uri, handle) = server(2);
    let config = temp.path().join("config");
    std::fs::write(&config, "out=config.txt\nsplit=2\nheader=X-Config: yes\n").unwrap();
    let input = temp.path().join("input");
    std::fs::write(
        &input,
        format!("{uri}\t{uri}\n  out=item.txt\n  split=1\n  header=X-Item: yes\n"),
    )
    .unwrap();
    let output = binary()
        .args([
            "--conf-path",
            config.to_str().unwrap(),
            "--input-file",
            input.to_str().unwrap(),
            "--dir",
            temp.path().to_str().unwrap(),
            "--out=cli.txt",
            "--header=X-Cli: yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    handle.join().unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("item.txt")).unwrap(),
        b"hello"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json[0]["request"]["uris"].as_array().unwrap().len(), 2);
    assert_eq!(
        json[0]["request"]["options"]["headers"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(json[0]["request"]["options"]["split"], 1);
}

#[test]
fn stdin_and_session_are_usable() {
    let temp = tempfile::tempdir().unwrap();
    let (uri, handle) = server(2);
    let session = temp.path().join("session.json");
    let mut process = binary()
        .args([
            "-i",
            "-",
            "-d",
            temp.path().to_str().unwrap(),
            "--split=1",
            "--save-session",
            session.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(process.stdin.take().unwrap(), "{uri}\n  out=stdin.txt").unwrap();
    let result = process.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    handle.join().unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("stdin.txt")).unwrap(),
        b"hello"
    );
    let result = binary()
        .args(["--load-session", session.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn example_config_and_bad_config_are_checked() {
    let example = binary()
        .args([
            "--conf-path",
            "examples/ariai.conf",
            "gopher://localhost/file",
        ])
        .output()
        .unwrap();
    assert!(!example.status.success());
    assert!(String::from_utf8_lossy(&example.stderr).contains("protocol gopher"));
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bad.conf");
    std::fs::write(&path, "unsupported-key=true\n").unwrap();
    let bad = binary()
        .args(["--conf-path", path.to_str().unwrap(), "--enable-rpc"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unsupported option"));
}

#[test]
fn http_failure_exits_nonzero() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let output = binary()
        .args(["--max-tries=1", &format!("http://{address}/file")])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn local_and_stdin_metalink_download_through_cli() {
    for stdin in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (uri, server) = server(2);
        let xml = format!(
            r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="selected.bin"><size>5</size><url>{uri}</url></file></metalink>"#
        );
        let metadata = temp.path().join("source.meta4");
        std::fs::write(&metadata, &xml).unwrap();
        let mut command = binary();
        command.args([
            "--split=1",
            "-d",
            temp.path().to_str().unwrap(),
            "-M",
            if stdin {
                "-"
            } else {
                metadata.to_str().unwrap()
            },
        ]);
        let result = if stdin {
            let mut process = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            process
                .stdin
                .take()
                .unwrap()
                .write_all(xml.as_bytes())
                .unwrap();
            process.wait_with_output().unwrap()
        } else {
            command.output().unwrap()
        };
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        server.join().unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("selected.bin")).unwrap(),
            b"hello"
        );
    }
}

#[test]
fn remote_metalink_redirect_uses_final_base_url() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for stream in listener.incoming().take(4) {
            let mut stream = stream.unwrap();
            let mut buffer = [0; 8192];
            let n = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..n]);
            let path = request.split_whitespace().nth(1).unwrap();
            match path {
                "/redirect.meta4"=>stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /dir/source.meta4\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(),
                "/dir/source.meta4"=>{let xml=r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="relative.bin"><url>file</url></file></metalink>"#;write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",xml.len(),xml).unwrap();},
                "/dir/file"=>stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").unwrap(),
                _=>panic!("unexpected relative resolution: {request}"),
            }
        }
    });
    let result = binary()
        .args([
            "--split=1",
            "--max-tries=1",
            "-d",
            temp.path().to_str().unwrap(),
            &format!("http://{address}/redirect.meta4"),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    server.join().unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("relative.bin")).unwrap(),
        b"hello"
    );
}

#[test]
fn session_format_is_explicit_and_defaults_to_json() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.session");
    std::fs::write(
        &source,
        "http://example.org/a\n gid=0123456789abcdef\n pause=true\n",
    )
    .unwrap();
    for (format, suffix) in [(None, "json"), (Some("aria2"), "text")] {
        let output = temp.path().join(suffix);
        let mut cmd = binary();
        cmd.args([
            "--load-session",
            source.to_str().unwrap(),
            "--save-session",
            output.to_str().unwrap(),
        ]);
        if let Some(format) = format {
            cmd.args(["--save-session-format", format]);
        }
        let result = cmd.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let saved = std::fs::read_to_string(output).unwrap();
        assert_eq!(saved.trim_start().starts_with('{'), format.is_none());
        if format.is_some() {
            assert!(saved.contains(" gid=0123456789abcdef\n"));
        }
    }
    assert!(
        !binary()
            .args([
                "--save-session-format",
                "bad",
                "--load-session",
                source.to_str().unwrap()
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
}
