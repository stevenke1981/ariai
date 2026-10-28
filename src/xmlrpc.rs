use base64::{Engine, engine::general_purpose::STANDARD};
use quick_xml::{Reader, events::Event};
use serde_json::{Map, Number, Value};

struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

fn parse_tree(bytes: &[u8]) -> Result<Node, ()> {
    std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut nodes = 0usize;
    loop {
        match reader.read_event().map_err(|_| ())? {
            Event::Start(e) => {
                nodes += 1;
                if nodes > 10_000
                    || stack.len() >= 64
                    || e.attributes().next().is_some()
                    || (stack.is_empty() && root.is_some())
                {
                    return Err(());
                }
                stack.push(Node {
                    name: e.name().as_ref().to_owned(),
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Event::End(_) => {
                let node = stack.pop().ok_or(())?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    root = Some(node);
                }
            }
            Event::Text(e) => {
                let content = e.xml_content(quick_xml::XmlVersion::Implicit1_0);
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&content);
                } else if !content.trim().is_empty() {
                    return Err(());
                }
            }
            Event::CData(e) => {
                let content = e.xml_content(quick_xml::XmlVersion::Implicit1_0);
                stack.last_mut().ok_or(())?.text.push_str(&content);
            }
            Event::GeneralRef(e) => {
                let name = e.xml_content(quick_xml::XmlVersion::Implicit1_0);
                let encoded = format!("&{name};");
                let decoded = quick_xml::escape::unescape(&encoded).map_err(|_| ())?;
                stack.last_mut().ok_or(())?.text.push_str(&decoded);
            }
            Event::Decl(_) | Event::Comment(_) => {}
            Event::Eof => break,
            _ => return Err(()),
        }
    }
    if !stack.is_empty() {
        return Err(());
    }
    root.ok_or(())
}

fn children<'a>(node: &'a Node, name: &str) -> Result<&'a [Node], ()> {
    if node.name != name || !node.text.trim().is_empty() {
        return Err(());
    }
    Ok(&node.children)
}
fn scalar<'a>(node: &'a Node, name: &str) -> Result<&'a str, ()> {
    if node.name != name || !node.children.is_empty() {
        return Err(());
    }
    Ok(&node.text)
}
fn value(node: &Node) -> Result<Value, ()> {
    if node.name != "value" {
        return Err(());
    }
    if node.children.is_empty() {
        return Ok(Value::String(node.text.clone()));
    }
    if node.children.len() != 1 || !node.text.trim().is_empty() {
        return Err(());
    }
    let n = &node.children[0];
    match n.name.as_str() {
        "string" => Ok(Value::String(scalar(n, "string")?.to_owned())),
        "base64" => {
            let s: String = scalar(n, "base64")?
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            let bytes = STANDARD.decode(s).map_err(|_| ())?;
            Ok(Value::String(STANDARD.encode(bytes)))
        }
        "int" | "i4" => Ok(Value::Number(Number::from(
            scalar(n, &n.name)?.trim().parse::<i32>().map_err(|_| ())?,
        ))),
        "i8" => Ok(Value::Number(Number::from(
            scalar(n, "i8")?.trim().parse::<i64>().map_err(|_| ())?,
        ))),
        "double" => Ok(Value::Number(
            Number::from_f64(scalar(n, "double")?.trim().parse::<f64>().map_err(|_| ())?)
                .ok_or(())?,
        )),
        "boolean" => match scalar(n, "boolean")?.trim() {
            "0" => Ok(Value::Bool(false)),
            "1" => Ok(Value::Bool(true)),
            _ => Err(()),
        },
        "array" => {
            let array = children(n, "array")?;
            if array.len() != 1 {
                return Err(());
            }
            Ok(Value::Array(
                children(&array[0], "data")?
                    .iter()
                    .map(value)
                    .collect::<Result<_, _>>()?,
            ))
        }
        "struct" => {
            let mut map = Map::new();
            for member in children(n, "struct")? {
                let pair = children(member, "member")?;
                if pair.len() != 2 {
                    return Err(());
                }
                let key = scalar(&pair[0], "name")?.to_owned();
                if map.insert(key, value(&pair[1])?).is_some() {
                    return Err(());
                }
            }
            Ok(Value::Object(map))
        }
        _ => Err(()),
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<(String, Vec<Value>), ()> {
    let root = parse_tree(bytes)?;
    let fields = children(&root, "methodCall")?;
    if fields.is_empty() || fields.len() > 2 {
        return Err(());
    }
    let method = scalar(&fields[0], "methodName")?.trim();
    if method.is_empty() {
        return Err(());
    }
    let mut params = Vec::new();
    if fields.len() == 2 {
        for param in children(&fields[1], "params")? {
            let v = children(param, "param")?;
            if v.len() != 1 {
                return Err(());
            }
            params.push(value(&v[0])?);
        }
    }
    Ok((method.to_owned(), params))
}

fn write_value(out: &mut String, v: &Value) {
    out.push_str("<value>");
    match v {
        Value::String(s) => {
            out.push_str("<string>");
            out.push_str(&quick_xml::escape::escape(s));
            out.push_str("</string>");
        }
        Value::Number(n) => {
            let tag = if n.is_f64() { "double" } else { "i8" };
            out.push_str(&format!("<{tag}>{n}</{tag}>"));
        }
        Value::Bool(b) => out.push_str(if *b {
            "<boolean>1</boolean>"
        } else {
            "<boolean>0</boolean>"
        }),
        Value::Array(a) => {
            out.push_str("<array><data>");
            for v in a {
                write_value(out, v);
            }
            out.push_str("</data></array>");
        }
        Value::Object(m) => {
            out.push_str("<struct>");
            for (k, v) in m {
                out.push_str("<member><name>");
                out.push_str(&quick_xml::escape::escape(k));
                out.push_str("</name>");
                write_value(out, v);
                out.push_str("</member>");
            }
            out.push_str("</struct>");
        }
        Value::Null => out.push_str("<string></string>"),
    }
    out.push_str("</value>");
}
pub(crate) fn response(result: Result<Value, (i32, String)>) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><methodResponse>");
    match result {
        Ok(v) => {
            out.push_str("<params><param>");
            write_value(&mut out, &v);
            out.push_str("</param></params>");
        }
        Err((code, message)) => {
            out.push_str("<fault>");
            write_value(
                &mut out,
                &serde_json::json!({"faultCode":code,"faultString":message}),
            );
            out.push_str("</fault>");
        }
    }
    out.push_str("</methodResponse>");
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_and_rejection() {
        let doc = br#"<methodCall><methodName>test</methodName><params><param><value><array><data><value><int>-1</int></value><value><i4>2</i4></value><value><i8>2147483648</i8></value><value><double>1.5</double></value><value><boolean>1</boolean></value><value><string>&lt;&amp;&#65;</string></value><value><base64>YQ==</base64></value><value><struct><member><name>a</name><value>x</value></member></struct></value></data></array></value></param></params></methodCall>"#;
        let (method, params) = parse(doc).unwrap();
        assert_eq!(method, "test");
        assert_eq!(
            params,
            vec![json!([-1, 2, 2147483648_i64, 1.5, true, "<&A", "YQ==", {"a":"x"}])]
        );
        for bad in [
            "<methodCall>",
            "<!DOCTYPE x><methodCall/>",
            "<methodCall><methodName>x</methodName><params><param><value><boolean>2</boolean></value></param></params></methodCall>",
            "<methodCall><methodName>x</methodName><params><param><value><int>2147483648</int></value></param></params></methodCall>",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse(format!("<methodCall><methodName>x</methodName><params><param>{}<value>x</value>{}</param></params></methodCall>", "<value><array><data>".repeat(65), "</data></array></value>".repeat(65)).as_bytes()).is_err());
        let encoded = response(Ok(json!({"<key>":"<&\""})));
        assert!(encoded.contains("&lt;key&gt;"));
        assert!(encoded.contains("&lt;&amp;&quot;"));
    }
}
