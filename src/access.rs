//! Remote-access gate.
//!
//! The dashboard serves the whole memory graph, so anything beyond loopback
//! sits behind a shared access key: a remote browser opens `?key=…` once, the
//! response sets a session cookie (which also rides the WebSocket upgrade),
//! and loopback clients — same trust domain as `~/.kumiho` — always pass.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::path::PathBuf;

const COOKIE_NAME: &str = "kb_key";
const KEY_FILENAME: &str = "kumiho-brain.key";

/// Whether the configured bind address stays on this machine.
pub fn is_loopback_bind(bind: &str) -> bool {
    if bind.eq_ignore_ascii_case("localhost") {
        return true;
    }
    bind.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Resolve the active access key for a non-loopback bind: explicit config,
/// else the persisted key file, else generate one and persist it (0600) so
/// remote bookmarks survive restarts.
pub fn resolve_key(configured: Option<&str>) -> String {
    if let Some(k) = configured {
        return k.to_string();
    }
    let path = key_path();
    if let Ok(k) = std::fs::read_to_string(&path) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return k;
        }
    }
    let key: String = {
        let bytes: [u8; 16] = rand::random();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, &key).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        tracing::info!("access key persisted at {}", path.display());
    }
    key
}

fn key_path() -> PathBuf {
    if let Ok(dir) = std::env::var("KUMIHO_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(KEY_FILENAME);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".kumiho").join(KEY_FILENAME)
}

/// Best-effort LAN address for the startup banner (no packets are sent).
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Axum middleware: gate non-loopback clients behind the access key.
pub async fn guard(
    State(key): State<Option<std::sync::Arc<String>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let Some(key) = key else {
        return next.run(req).await; // no key configured (loopback-only or --no-auth)
    };
    if peer.ip().is_loopback() {
        return next.run(req).await;
    }
    let cookies = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if cookie_key(cookies).map(|v| ct_eq(v, &key)).unwrap_or(false) {
        return next.run(req).await;
    }
    if let Some(qk) = query_key(req.uri().query()) {
        if ct_eq(&qk, &key) {
            let cookie =
                format!("{COOKIE_NAME}={key}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000");
            // Page navigations redirect so the key leaves the address bar;
            // API/tooling calls pass straight through with the cookie set.
            if req.uri().path() == "/" {
                return Response::builder()
                    .status(StatusCode::SEE_OTHER)
                    .header(header::LOCATION, "/")
                    .header(header::SET_COOKIE, cookie)
                    .body(axum::body::Body::empty())
                    .unwrap()
                    .into_response();
            }
            let mut res = next.run(req).await;
            if let Ok(v) = header::HeaderValue::from_str(&cookie) {
                res.headers_mut().append(header::SET_COOKIE, v);
            }
            return res;
        }
    }
    (
        StatusCode::UNAUTHORIZED,
        "KUMIHO BRAIN — access key required. Open http://<host>:<port>/?key=<access-key> \
         (printed on the server's startup banner).",
    )
        .into_response()
}

fn cookie_key(cookies: &str) -> Option<&str> {
    cookies
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix("kb_key="))
}

fn query_key(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .find_map(|p| p.strip_prefix("key=").map(str::to_string))
}

/// Constant-time comparison — the key is the only credential here.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("localhost"));
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("192.168.1.7"));
        assert!(!is_loopback_bind("::"));
    }

    #[test]
    fn key_extraction() {
        assert_eq!(query_key(Some("key=abc&x=1")), Some("abc".into()));
        assert_eq!(query_key(Some("x=1")), None);
        assert_eq!(query_key(None), None);
        assert_eq!(cookie_key("a=1; kb_key=deadbeef; b=2"), Some("deadbeef"));
        assert_eq!(cookie_key("a=1"), None);
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abcd", "abcd"));
        assert!(!ct_eq("abcd", "abce"));
        assert!(!ct_eq("abcd", "abcde"));
        assert!(!ct_eq("", "a"));
        assert!(ct_eq("", ""));
    }
}
