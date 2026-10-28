use crate::model::DownloadOptions;
use anyhow::{Context, Result, anyhow, ensure};
use cookie_store::{CookieDomain, CookieExpiration, CookieStore};
use md5::Md5;
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode,
    header::{AUTHORIZATION, COOKIE, LOCATION, REFERER, SET_COOKIE, WWW_AUTHENTICATE},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

#[derive(Clone)]
pub struct HttpSession {
    client: Client,
    headers: reqwest::header::HeaderMap,
    cookies: Arc<Mutex<CookieStore>>,
    save: Option<PathBuf>,
    user: Option<String>,
    password: Option<String>,
    challenge: bool,
}

impl HttpSession {
    pub fn new(options: &DownloadOptions) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        for line in &options.headers {
            let (name, value) = line.split_once(':').context("invalid header")?;
            headers.append(
                reqwest::header::HeaderName::from_bytes(name.trim().as_bytes())?,
                reqwest::header::HeaderValue::from_str(value.trim())?,
            );
        }
        if let Some(referer) = &options.referer {
            headers.insert(REFERER, referer.parse()?);
        }
        let mut builder = Client::builder()
            .user_agent(&options.user_agent)
            .timeout(Duration::from_secs(options.timeout_secs))
            .connect_timeout(Duration::from_secs(options.connect_timeout_secs))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = &options.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy)?);
        }
        let mut cookies = CookieStore::default();
        if let Some(path) = &options.load_cookies {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("reading cookies: {}", path.display()))?;
            for (index, line) in data.lines().enumerate() {
                let line = line.trim_end_matches('\r');
                let (http_only, line) = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
                    (true, rest)
                } else {
                    (false, line)
                };
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let fields: Vec<_> = line.split('\t').collect();
                ensure!(
                    fields.len() == 7,
                    "unsupported cookie file format at line {} (expected Netscape format)",
                    index + 1
                );
                let [domain, include, path, secure, expires, name, value] = fields.as_slice()
                else {
                    unreachable!()
                };
                ensure!(
                    matches!(*include, "TRUE" | "FALSE")
                        && matches!(*secure, "TRUE" | "FALSE")
                        && path.starts_with('/')
                        && !name.is_empty(),
                    "invalid Netscape cookie at line {}",
                    index + 1
                );
                let expiry: u64 = expires
                    .parse()
                    .with_context(|| format!("invalid cookie expiry at line {}", index + 1))?;
                if expiry != 0
                    && UNIX_EPOCH
                        .checked_add(Duration::from_secs(expiry))
                        .is_none_or(|t| t <= SystemTime::now())
                {
                    continue;
                }
                let host = domain.trim_start_matches('.');
                let url = Url::parse(&format!("https://{host}{path}"))
                    .with_context(|| format!("invalid cookie domain at line {}", index + 1))?;
                let mut raw = format!("{name}={value}; Path={path}");
                if *include == "TRUE" {
                    raw.push_str(&format!("; Domain={host}"));
                }
                if *secure == "TRUE" {
                    raw.push_str("; Secure");
                }
                if http_only {
                    raw.push_str("; HttpOnly");
                }
                if expiry != 0 {
                    raw.push_str(&format!(
                        "; Expires={}",
                        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(expiry))
                    ));
                }
                cookies
                    .parse(&raw, &url)
                    .map_err(|e| anyhow!("invalid Netscape cookie at line {}: {e}", index + 1))?;
            }
        }
        Ok(Self {
            client: builder.build()?,
            headers,
            cookies: Arc::new(Mutex::new(cookies)),
            save: options.save_cookies.clone(),
            user: options.http_user.clone(),
            password: options.http_passwd.clone(),
            challenge: options.http_auth_challenge,
        })
    }
    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client
            .request(method, url)
            .headers(self.headers.clone())
    }
    pub async fn send(&self, builder: RequestBuilder) -> Result<Response> {
        let mut request = builder.build()?;
        let original = request.url().origin();
        let mut auth_attempts = 0;
        let mut redirects = 0;
        let mut auth = if !self.challenge {
            self.user
                .as_ref()
                .map(|u| basic(u, self.password.as_deref().unwrap_or("")))
        } else {
            None
        };
        loop {
            let mut next = request
                .try_clone()
                .context("HTTP request body is not replayable")?;
            if let Some(value) = &auth {
                next.headers_mut().insert(AUTHORIZATION, value.parse()?);
            }
            let cookie = self
                .cookies
                .lock()
                .unwrap()
                .matches(next.url())
                .into_iter()
                .filter(|cookie| {
                    next.url().scheme() == "https" || !cookie.secure().unwrap_or(false)
                })
                .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
                .collect::<Vec<_>>()
                .join("; ");
            if !cookie.is_empty() && !next.headers().contains_key(COOKIE) {
                next.headers_mut().insert(COOKIE, cookie.parse()?);
            }
            let response = self.client.execute(next).await?;
            {
                let mut jar = self.cookies.lock().unwrap();
                for value in response.headers().get_all(SET_COOKIE) {
                    if let Ok(value) = value.to_str() {
                        let _ = jar.parse(value, response.url());
                    }
                }
            }
            if response.status().is_redirection() {
                let Some(location) = response.headers().get(LOCATION) else {
                    return Ok(response);
                };
                redirects += 1;
                ensure!(redirects <= 10, "too many HTTP redirects");
                let destination = request.url().join(location.to_str()?)?;
                ensure!(
                    matches!(destination.scheme(), "http" | "https"),
                    "unsupported redirect protocol"
                );
                if destination.origin() != request.url().origin() {
                    auth = None;
                    request.headers_mut().remove(AUTHORIZATION);
                    request.headers_mut().remove(COOKIE);
                }
                *request.url_mut() = destination;
                auth_attempts = 0;
                continue;
            }
            if response.status() == StatusCode::UNAUTHORIZED
                && request.url().origin() == original
                && self.user.is_some()
                && auth_attempts < 3
            {
                let mut basic_offered = false;
                let mut digest = None;
                for value in response.headers().get_all(WWW_AUTHENTICATE) {
                    let value = value.to_str()?;
                    if value
                        .get(..7)
                        .is_some_and(|v| v.eq_ignore_ascii_case("Digest "))
                    {
                        digest = Some(&value[7..]);
                    } else if value
                        .get(..5)
                        .is_some_and(|v| v.eq_ignore_ascii_case("Basic"))
                    {
                        basic_offered = true;
                    }
                }
                if let Some(challenge) = digest {
                    let params = parse_challenge(challenge)?;
                    if auth_attempts > 0
                        && !params
                            .get("stale")
                            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
                    {
                        return Ok(response);
                    }
                    auth = Some(digest_auth(
                        &params,
                        request.method(),
                        request.url(),
                        self.user.as_deref().unwrap(),
                        self.password.as_deref().unwrap_or(""),
                    )?);
                    auth_attempts += 1;
                    continue;
                }
                if basic_offered && auth.is_none() {
                    auth = Some(basic(
                        self.user.as_deref().unwrap(),
                        self.password.as_deref().unwrap_or(""),
                    ));
                    auth_attempts += 1;
                    continue;
                }
            }
            return Ok(response);
        }
    }
    pub async fn save_cookies(&self) -> Result<()> {
        let Some(path) = &self.save else {
            return Ok(());
        };
        let data = {
            let jar = self.cookies.lock().unwrap();
            let mut data = String::from("# Netscape HTTP Cookie File\n");
            for cookie in jar.iter_unexpired() {
                let (domain, include) = match &cookie.domain {
                    CookieDomain::HostOnly(host) => (host.as_str(), "FALSE"),
                    CookieDomain::Suffix(host) => (host.as_str(), "TRUE"),
                    _ => continue,
                };
                let expires = match &cookie.expires {
                    CookieExpiration::SessionEnd => 0,
                    CookieExpiration::AtUtc(time) => time.unix_timestamp().max(0),
                };
                let prefix = if cookie.http_only().unwrap_or(false) {
                    "#HttpOnly_"
                } else {
                    ""
                };
                data.push_str(&format!(
                    "{prefix}{domain}\t{include}\t{}\t{}\t{expires}\t{}\t{}\n",
                    cookie.path.as_ref(),
                    if cookie.secure().unwrap_or(false) {
                        "TRUE"
                    } else {
                        "FALSE"
                    },
                    cookie.name(),
                    cookie.value()
                ));
            }
            data
        };
        tokio::fs::write(path, data)
            .await
            .with_context(|| format!("saving cookies: {}", path.display()))?;
        Ok(())
    }
}
fn basic(user: &str, password: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}
fn parse_challenge(input: &str) -> Result<HashMap<String, String>> {
    let mut chars = input.chars().peekable();
    let mut values = HashMap::new();
    while chars.peek().is_some() {
        while chars
            .peek()
            .is_some_and(|c| c.is_ascii_whitespace() || *c == ',')
        {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        ensure!(!key.trim().is_empty(), "invalid Digest challenge");
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            let mut value = String::new();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '"' {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    value.push(chars.next().context("invalid Digest escape")?);
                } else {
                    value.push(c);
                }
            }
            ensure!(closed, "unterminated Digest challenge");
            value
        } else {
            chars
                .by_ref()
                .take_while(|c| *c != ',')
                .collect::<String>()
                .trim()
                .to_owned()
        };
        values.insert(key.trim().to_ascii_lowercase(), value);
    }
    Ok(values)
}
fn digest_auth(
    params: &HashMap<String, String>,
    method: &Method,
    url: &Url,
    user: &str,
    password: &str,
) -> Result<String> {
    let realm = params
        .get("realm")
        .context("Digest challenge missing realm")?;
    let nonce = params
        .get("nonce")
        .context("Digest challenge missing nonce")?;
    let algorithm = params.get("algorithm").map(String::as_str).unwrap_or("MD5");
    let normalized = algorithm.to_ascii_uppercase();
    ensure!(
        matches!(
            normalized.as_str(),
            "MD5" | "MD5-SESS" | "SHA-256" | "SHA-256-SESS"
        ),
        "unsupported Digest algorithm: {algorithm}"
    );
    let qop = params.get("qop");
    if let Some(qop) = qop {
        ensure!(
            qop.split(',')
                .any(|x| x.trim().eq_ignore_ascii_case("auth")),
            "unsupported Digest qop: {qop}"
        );
    }
    let hash = |v: &str| {
        if normalized.starts_with("MD5") {
            hex::encode(Md5::digest(v.as_bytes()))
        } else {
            hex::encode(Sha256::digest(v.as_bytes()))
        }
    };
    let cnonce = uuid::Uuid::new_v4().simple().to_string();
    let uri = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    let mut ha1 = hash(&format!("{user}:{realm}:{password}"));
    if normalized.ends_with("-SESS") {
        ha1 = hash(&format!("{ha1}:{nonce}:{cnonce}"));
    }
    let ha2 = hash(&format!("{method}:{uri}"));
    let response = if qop.is_some() {
        hash(&format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}"))
    } else {
        hash(&format!("{ha1}:{nonce}:{ha2}"))
    };
    let quote = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let mut value = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{response}\", algorithm={algorithm}",
        quote(user),
        quote(realm),
        quote(nonce),
        quote(&uri)
    );
    if let Some(opaque) = params.get("opaque") {
        value.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
    }
    if qop.is_some() {
        value.push_str(&format!(", qop=auth, nc=00000001, cnonce=\"{cnonce}\""));
    } else if normalized.ends_with("-SESS") {
        value.push_str(&format!(", cnonce=\"{cnonce}\""));
    }
    Ok(value)
}
