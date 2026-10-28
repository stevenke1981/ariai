use ariai::model::{DownloadOptions, DownloadRequest};

#[test]
fn future_protocols_fail_explicitly() {
    for uri in [
        "gopher://example.com/f",
        "smb://example.com/f",
        "ed2k://example.com/f",
    ] {
        let error = DownloadRequest {
            uris: vec![uri.into()],
            options: DownloadOptions::default(),
        }
        .validate()
        .unwrap_err();
        assert!(error.to_string().contains("not implemented yet"));
    }
}

#[test]
fn reject_invalid_checksum_before_network_or_file_io() {
    for checksum in [
        "sha256=deadbeef",
        "crc32=12345678",
        "garbage",
        "sha1=zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        let options = DownloadOptions {
            checksum: Some(checksum.into()),
            ..Default::default()
        };
        assert!(options.validate().is_err());
    }
}

#[test]
fn reject_output_traversal_and_header_injection() {
    for out in ["../file", "/file", ""] {
        assert!(
            DownloadOptions {
                out: Some(out.into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        DownloadOptions {
            headers: vec!["x-test: hello\r\nx-other: injected".into()],
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn phase3_options_validate_and_old_sessions_keep_defaults() {
    use ariai::config::apply_option;
    let mut options: DownloadOptions = serde_json::from_str("{}").unwrap();
    assert!(!options.seed_time_explicit);
    assert_eq!(options.seed_ratio, None);
    apply_option(&mut options, "seed-time", "0").unwrap();
    assert!(options.seed_time_explicit);
    apply_option(&mut options, "max-upload-limit", "32K").unwrap();
    assert_eq!(options.max_upload_limit, 32768);
    apply_option(&mut options, "seed-ratio", "0").unwrap();
    options.validate().unwrap();
    for value in ["NaN", "inf", "-1"] {
        apply_option(&mut options, "seed-ratio", value).unwrap();
        assert!(options.validate().is_err());
    }
}
