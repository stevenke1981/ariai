use crate::model::{DownloadOptions, DownloadRequest, PieceHashes, selected_indices};
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use std::collections::HashMap;
use url::Url;

const V4: &[u8] = b"urn:ietf:params:xml:ns:metalink";
const V3: &[u8] = b"http://www.metalinker.org/";

#[derive(Default)]
struct Node {
    name: String,
    ns: Vec<u8>,
    attrs: HashMap<String, String>,
    text: String,
    children: Vec<Node>,
}
impl Node {
    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children
            .iter()
            .filter(move |node| node.name == name && node.ns == self.ns)
    }
    fn attr(&self, name: &str) -> Result<&str> {
        self.attrs
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("missing {name} attribute on {}", self.name))
    }
}

fn algorithm(name: &str) -> Option<(&'static str, usize, u8)> {
    match name.to_ascii_lowercase().as_str() {
        "sha-256" | "sha256" => Some(("sha-256", 64, 3)),
        "sha-1" | "sha1" => Some(("sha-1", 40, 2)),
        "md5" => Some(("md5", 32, 1)),
        _ => None,
    }
}
fn digest(text: &str, len: usize) -> Result<String> {
    let text = text.trim();
    ensure!(
        text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid Metalink hash"
    );
    Ok(text.to_ascii_lowercase())
}

pub fn parse(
    bytes: &[u8],
    base: Option<&Url>,
    defaults: &DownloadOptions,
) -> Result<Vec<DownloadRequest>> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        match reader.read_event()? {
            Event::Start(event) => {
                let (ns, local) = reader.resolver().resolve_element(event.name());
                let ns = match ns {
                    ResolveResult::Bound(ns) => ns.as_ref().as_bytes().to_vec(),
                    ResolveResult::Unbound => Vec::new(),
                    ResolveResult::Unknown(_) => bail!("unknown XML namespace prefix"),
                };
                let name = local.as_ref().to_owned();
                let mut node = Node {
                    name,
                    ns,
                    ..Node::default()
                };
                for attr in event.attributes() {
                    let attr = attr?;
                    let key = attr.key.as_ref();
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        continue;
                    }
                    if !key.contains(':') {
                        node.attrs.insert(
                            key.to_owned(),
                            attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                                .into_owned(),
                        );
                    }
                }
                ensure!(root.is_none(), "multiple XML root elements");
                stack.push(node);
            }
            Event::End(_) => {
                let node = stack.pop().context("unexpected closing element")?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    ensure!(root.replace(node).is_none(), "multiple XML root elements");
                }
            }
            Event::Text(text) => {
                let text = text.xml_content(quick_xml::XmlVersion::Implicit1_0);
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text);
                } else {
                    ensure!(text.trim().is_empty(), "text outside XML root");
                }
            }
            Event::CData(text) => {
                let text = text.xml_content(quick_xml::XmlVersion::Implicit1_0);
                stack
                    .last_mut()
                    .context("CDATA outside XML root")?
                    .text
                    .push_str(&text);
            }
            Event::GeneralRef(reference) => {
                let raw = reference.as_ref();
                let encoded = format!("&{raw};");
                let value = quick_xml::escape::unescape(&encoded)?;
                stack
                    .last_mut()
                    .context("entity outside XML root")?
                    .text
                    .push_str(&value);
            }
            Event::DocType(_) => bail!("DTD is forbidden in Metalink"),
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            _ => bail!("unexpected XML event"),
        }
    }
    ensure!(stack.is_empty(), "unclosed XML element");
    let root = root.context("missing Metalink root")?;
    ensure!(
        root.name == "metalink" && (root.ns == V3 || root.ns == V4),
        "unsupported Metalink root or namespace"
    );
    let v3 = root.ns == V3;
    if let Some(version) = root.attrs.get("version") {
        ensure!(
            (v3 && version == "3.0") || (!v3 && version == "4.0"),
            "unsupported Metalink version"
        );
    }
    let files: Vec<&Node> = if v3 {
        root.children("files")
            .flat_map(|n| n.children("file"))
            .collect()
    } else {
        root.children("file").collect()
    };
    ensure!(!files.is_empty(), "Metalink has no files");
    let selected = selected_indices(defaults.select_file.as_deref(), Some(files.len()))?;
    let mut requests = Vec::with_capacity(selected.len());
    for index in selected {
        let file = files[index];
        let name = file.attr("name")?;
        ensure!(
            !name.is_empty()
                && !name.contains(['\\', ':', '\0'])
                && name
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "unsafe Metalink file name"
        );
        let mut options = defaults.clone();
        options.out = Some(name.to_owned());
        options.select_file = None;
        let sizes: Vec<_> = file.children("size").collect();
        ensure!(sizes.len() <= 1, "multiple Metalink sizes");
        if let Some(size) = sizes.first() {
            options.expected_length =
                Some(size.text.trim().parse().context("invalid Metalink size")?);
        }
        let verification: Vec<&Node> = if v3 {
            file.children("verification").collect()
        } else {
            vec![file]
        };
        let mut best_hash: Option<(u8, String)> = None;
        let mut best_pieces: Option<(u8, PieceHashes)> = None;
        let mut declared_hash = false;
        for verification in verification {
            for hash in verification.children("hash") {
                declared_hash = true;
                if let Some((name, len, rank)) = algorithm(hash.attr("type")?) {
                    let value = format!("{name}={}", digest(&hash.text, len)?);
                    if best_hash.as_ref().is_none_or(|(old, _)| rank > *old) {
                        best_hash = Some((rank, value));
                    }
                }
            }
            for pieces in verification.children("pieces") {
                declared_hash = true;
                if let Some((name, len, rank)) = algorithm(pieces.attr("type")?) {
                    let length: u64 = pieces
                        .attr("length")?
                        .parse()
                        .context("invalid piece length")?;
                    ensure!(length > 0, "piece length must be positive");
                    let mut hashes = Vec::new();
                    for hash in pieces.children("hash") {
                        if let Some(piece) = hash.attrs.get("piece") {
                            ensure!(
                                piece.parse::<usize>()? == hashes.len(),
                                "out-of-order Metalink piece index"
                            );
                        }
                        hashes.push(digest(&hash.text, len)?);
                    }
                    ensure!(!hashes.is_empty(), "empty Metalink piece list");
                    if let Some(size) = options.expected_length {
                        ensure!(
                            size.div_ceil(length) == hashes.len() as u64,
                            "Metalink piece count does not match size"
                        );
                    }
                    if best_pieces.as_ref().is_none_or(|(old, _)| rank > *old) {
                        best_pieces = Some((
                            rank,
                            PieceHashes {
                                algorithm: name.to_owned(),
                                length,
                                hashes,
                            },
                        ));
                    }
                }
            }
        }
        ensure!(
            !declared_hash || best_hash.is_some() || best_pieces.is_some(),
            "Metalink has no supported verification hashes"
        );
        if let Some((_, hash)) = best_hash {
            options.checksum = Some(hash);
        }
        if let Some((_, pieces)) = best_pieces {
            options.piece_hashes = Some(pieces);
        }
        let urls: Vec<&Node> = if v3 {
            file.children("resources")
                .flat_map(|n| n.children("url"))
                .collect()
        } else {
            file.children("url").collect()
        };
        let mut mirrors = Vec::new();
        for url in urls {
            let value = url.text.trim();
            let parsed = match Url::parse(value) {
                Ok(url) => url,
                Err(url::ParseError::RelativeUrlWithoutBase) => base
                    .context("relative Metalink URL requires a base")?
                    .join(value)?,
                Err(e) => return Err(e.into()),
            };
            if !matches!(parsed.scheme(), "http" | "https" | "ftp" | "sftp") {
                continue;
            }
            ensure!(parsed.host_str().is_some(), "Metalink URL requires a host");
            let priority = if v3 {
                url.attrs
                    .get("preference")
                    .map(|p| p.parse::<u32>())
                    .transpose()?
                    .unwrap_or(0)
            } else {
                url.attrs
                    .get("priority")
                    .map(|p| p.parse::<u32>())
                    .transpose()?
                    .unwrap_or(u32::MAX)
            };
            mirrors.push((priority, parsed.to_string()));
        }
        if v3 {
            mirrors.sort_by_key(|a| std::cmp::Reverse(a.0));
        } else {
            mirrors.sort_by_key(|a| a.0);
        }
        let request = DownloadRequest {
            uris: mirrors.into_iter().map(|(_, url)| url).collect(),
            options,
        };
        request.validate()?;
        requests.push(request);
    }
    Ok(requests)
}
