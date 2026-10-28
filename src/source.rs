use crate::model::{DownloadOptions, DownloadRequest};
use anyhow::{Context, Result};

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
        let session = crate::http::HttpSession::new(options)?;
        let response = session
            .send(session.request(reqwest::Method::GET, url.as_str()))
            .await;
        session.save_cookies().await?;
        let response = response?.error_for_status()?;
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
