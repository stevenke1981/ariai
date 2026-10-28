use crate::model::{DownloadOptions, DownloadRequest};
use anyhow::{Context, Result};
use std::time::Duration;

pub fn is_metalink(source: &str) -> bool {
    let path = url::Url::parse(source)
        .map(|u| u.path().to_owned())
        .unwrap_or_else(|_| source.into())
        .to_ascii_lowercase();
    path.ends_with(".meta4") || path.ends_with(".metalink")
}

pub async fn metalink(source: &str, options: &DownloadOptions) -> Result<Vec<DownloadRequest>> {
    let mut parsed = url::Url::parse(source)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    let bytes = if let Some(url) = &parsed {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(options.timeout_secs))
            .connect_timeout(Duration::from_secs(options.connect_timeout_secs))
            .user_agent(&options.user_agent);
        if let Some(proxy) = &options.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy)?);
        }
        let mut request = builder.build()?.get(url.clone());
        for header in &options.headers {
            let (name, value) = header.split_once(':').context("invalid header")?;
            request = request.header(name.trim(), value.trim());
        }
        if let Some(user) = &options.http_user {
            request = request.basic_auth(user, options.http_passwd.as_ref());
        }
        let response = request.send().await?.error_for_status()?;
        parsed = Some(response.url().clone());
        response.bytes().await?.to_vec()
    } else if source == "-" {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        tokio::fs::read(source)
            .await
            .with_context(|| format!("reading Metalink {source}"))?
    };
    crate::metalink::parse(&bytes, parsed.as_ref(), options)
}
