//! Blocking HTTP/1.1 client for the daemon's Unix socket. One request per connection
//! (`Connection: close`), JSON in and out; the daemon never streams these responses.

use crate::{Error, Result, strata_home};
use serde_json::Value;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;

pub const SOCKET: &str = "strata.sock";

pub fn get(path: &str) -> Result<Value> {
    request("GET", path, None)
}

pub fn post(path: &str, body: Value) -> Result<Value> {
    request("POST", path, Some(body))
}

pub fn request(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let mut stream = match UnixStream::connect(strata_home().join(SOCKET)) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Err(Error::NotFound(
                "the daemon is not running; start it with `strata daemon start`".into(),
            ));
        }
        // std refuses socket paths longer than sun_path ("path must be shorter than SUN_LEN").
        Err(e) if e.kind() == ErrorKind::InvalidInput => {
            return Err(Error::Refused(format!(
                "the socket path {} is too long for a Unix socket; set STRATA_HOME to a shorter folder",
                strata_home().join(SOCKET).display()
            )));
        }
        Err(e) => return Err(e.into()),
    };
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    // One write: `write!` sends the pieces separately, and a handler that reads no body (such
    // as /v1/shutdown) can answer and close after the headers, failing the rest with EPIPE.
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: strata\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let bad = || {
        Error::Io(std::io::Error::other(format!(
            "bad response from the daemon: {raw:?}"
        )))
    };
    let (head, payload) = raw.split_once("\r\n\r\n").ok_or_else(bad)?;
    let status: u16 = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    let value: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).map_err(|_| bad())?
    };
    let message = || {
        value["error"]
            .as_str()
            .unwrap_or("request failed")
            .to_string()
    };
    match status {
        200..=299 => Ok(value),
        404 => Err(Error::NotFound(message())),
        _ => Err(Error::Refused(message())),
    }
}
