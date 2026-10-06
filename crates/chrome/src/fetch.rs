//! URLs: their host, resolving a relative one, reading a `data:` URL and
//! fetching an `http:` one (source maps of a local server).

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};

use crate::sourcemap::decode_base64;

/// The host of an `http(s)` URL, lowercase and without the port.
pub fn host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or(inner)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    Some(host.to_ascii_lowercase())
}

/// Whether a host is in the list: `name` is that host, `*.name` its
/// subdomains.
pub fn host_matches(host: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| match pattern.strip_prefix("*.") {
        Some(domain) => host.len() > domain.len() + 1 && host.ends_with(&format!(".{domain}")),
        None => host == pattern,
    })
}

/// `relative` resolved against `base`, as a browser does for the URLs it
/// knows: absolute, protocol-relative, root-relative or path-relative.
pub fn resolve_url(base: &str, relative: &str) -> String {
    let has_scheme = relative.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    if has_scheme {
        return relative.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return relative.to_string();
    };
    if let Some(rest) = relative.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..authority_end]);
    let path = rest[authority_end..].split(['?', '#']).next().unwrap_or("");
    let joined = if relative.starts_with('/') {
        relative.to_string()
    } else {
        let dir = &path[..path.rfind('/').map(|at| at + 1).unwrap_or(0)];
        let dir = if dir.is_empty() { "/" } else { dir };
        format!("{dir}{relative}")
    };
    let (path, query) = match joined.find(['?', '#']) {
        Some(at) => (&joined[..at], &joined[at..]),
        None => (joined.as_str(), ""),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/').skip(1) {
        match part {
            "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("{origin}/{}{query}", parts.join("/"))
}

/// Where an `http(s)` URL's server listens.
pub fn address(url: &str) -> Result<SocketAddr> {
    let (rest, default_port) = if let Some(rest) = url.strip_prefix("http://") {
        (rest, 80)
    } else if let Some(rest) = url.strip_prefix("https://") {
        (rest, 443)
    } else {
        bail!("{} is not an http(s) URL", short(url));
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = host(url).context("a URL without a host")?;
    let port_text = if authority.starts_with('[') {
        authority.split_once("]:").map(|(_, port)| port)
    } else {
        authority.rsplit_once(':').map(|(_, port)| port)
    };
    let port = match port_text {
        Some(port) => port.parse::<u16>().with_context(|| format!("{port:?} is not a port"))?,
        None => default_port,
    };
    // Chrome resolves localhost and its subdomains to the loopback itself;
    // the system resolver may not know `app.localhost`.
    let connect_host =
        if host == "localhost" || host.ends_with(".localhost") { "127.0.0.1".to_string() } else { host.clone() };
    (connect_host.as_str(), port)
        .to_socket_addrs()
        .with_context(|| format!("resolve {connect_host}"))?
        .next()
        .with_context(|| format!("{connect_host} has no address"))
}

/// The text of a `data:` or `http:` URL.
pub fn load(url: &str) -> Result<String> {
    if let Some(data) = url.strip_prefix("data:") {
        return read_data_url(data);
    }
    if url.starts_with("http://") {
        return http_get(url);
    }
    bail!("only data: and http: source maps are read, not {}", short(url))
}

fn read_data_url(data: &str) -> Result<String> {
    let (meta, payload) = data.split_once(',').context("a data: URL without a comma")?;
    let bytes = if meta.split(';').any(|part| part.eq_ignore_ascii_case("base64")) {
        decode_base64(payload).context("the data: URL is not valid base64")?
    } else {
        percent_decode(payload)?
    };
    String::from_utf8(bytes).context("the data: URL is not UTF-8")
}

fn percent_decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3).context("a % escape is cut short")?;
            let byte = u8::from_str_radix(hex, 16).with_context(|| format!("%{hex} is not an escape"))?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Ok(out)
}

fn http_get(url: &str) -> Result<String> {
    let rest = &url["http://".len()..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path = rest[authority_end..].split('#').next().unwrap_or("");
    let path = if path.is_empty() { "/" } else { path };
    let address = address(url)?;
    let timeout = Duration::from_secs(10);
    let mut stream =
        TcpStream::connect_timeout(&address, timeout).with_context(|| format!("connect to {authority}"))?;
    stream.set_read_timeout(Some(timeout)).context("set the read timeout")?;
    stream.set_write_timeout(Some(timeout)).context("set the write timeout")?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nAccept-Encoding: identity\r\nUser-Agent: den-chrome\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).context("send the request")?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).context("read the response")?;

    let head_end = find(&response, b"\r\n\r\n").context("a response without headers")?;
    let head = String::from_utf8_lossy(&response[..head_end]).to_string();
    let body = &response[head_end + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let code = status.split(' ').nth(1).unwrap_or("");
    if code != "200" {
        bail!("{} answered {status:?}", short(url));
    }
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.parse::<usize>().with_context(|| format!("content-length {value:?}"))?);
        } else if name.eq_ignore_ascii_case("content-encoding") && !value.eq_ignore_ascii_case("identity") {
            bail!("{} came encoded as {value}", short(url));
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(length) = length {
        body.get(..length).context("the response is shorter than its content-length")?.to_vec()
    } else {
        body.to_vec()
    };
    String::from_utf8(body).context("the response is not UTF-8")
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = find(body, b"\r\n").context("a chunk without its size line")?;
        let size_text = String::from_utf8_lossy(&body[..line_end]).to_string();
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).with_context(|| format!("chunk size {size_text:?}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(body.get(..size).context("a chunk is cut short")?);
        body = body.get(size + 2..).context("a chunk is cut short")?;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A text for a message, cut: a `data:` URL can be megabytes.
pub fn short(text: &str) -> String {
    const MAX: usize = 300;
    if text.len() <= MAX {
        return text.to_string();
    }
    let cut = (0..=MAX).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
    format!("{}…", &text[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("http://localhost:9092/a").as_deref(), Some("localhost"));
        assert_eq!(host("https://App.Localhost/").as_deref(), Some("app.localhost"));
        assert_eq!(host("http://[::1]:80/").as_deref(), Some("::1"));
        assert_eq!(host("about:blank"), None);
        let list = vec!["localhost".to_string(), "*.localhost".to_string()];
        assert!(host_matches("localhost", &list));
        assert!(host_matches("a.localhost", &list));
        assert!(!host_matches("evillocalhost", &list));
        assert!(!host_matches("example.com", &list));
    }

    #[test]
    fn resolves_urls() {
        let base = "http://localhost:8080/js/app.js?v=1";
        assert_eq!(resolve_url(base, "app.js.map"), "http://localhost:8080/js/app.js.map");
        assert_eq!(resolve_url(base, "../maps/a.map"), "http://localhost:8080/maps/a.map");
        assert_eq!(resolve_url(base, "/a.map"), "http://localhost:8080/a.map");
        assert_eq!(resolve_url(base, "//other/a.map"), "http://other/a.map");
        assert_eq!(resolve_url(base, "data:application/json,{}"), "data:application/json,{}");
    }

    #[test]
    fn reads_data_urls() {
        assert_eq!(load("data:application/json;base64,eyJhIjoxfQ==").unwrap(), "{\"a\":1}");
        assert_eq!(load("data:application/json;charset=utf-8,%7B%22a%22%3A1%7D").unwrap(), "{\"a\":1}");
        assert!(load("https://example.com/a.map").is_err());
    }
}
