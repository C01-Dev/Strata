//! The `.localhost` reverse proxy (PLAN 3.4). Loopback only (127.0.0.1 and ::1). A request for
//! `<agent>.<app>.localhost:<port>` goes to that agent's dev server, with WebSocket upgrades
//! passed through (hot reload). An unknown host gets a page listing live agents; a request that
//! already went through the proxy gets 508 (a route pointing back at the proxy); a dead dev
//! server gets 502. The original Host header is kept, so the app sees the URL the browser used.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HOST, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use tokio::net::{TcpListener, TcpStream};

/// Host label (`<agent>.<app>`) -> where its dev server listens.
pub type Routes = Arc<RwLock<HashMap<String, SocketAddr>>>;

type Body = BoxBody<Bytes, hyper::Error>;

/// Marks requests the proxy forwarded, so one that comes back is a loop.
const HOP: &str = "x-strata-proxy";

/// Binds `port` on both loopback addresses. If another program holds it, the error names it.
pub async fn bind(port: u16) -> Result<Vec<TcpListener>, String> {
    let held = |e: std::io::Error| {
        let holder = listeners::get_process_by_port(port, listeners::Protocol::TCP)
            .map(|p| format!("{} (pid {})", p.name, p.pid))
            .unwrap_or_else(|_| "another program".into());
        format!(
            "proxy port {port} is held by {holder} ({e}); stop it or set dev.proxy_port in strata.toml"
        )
    };
    let mut out = vec![TcpListener::bind(("127.0.0.1", port)).await.map_err(held)?];
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
        Ok(l) => out.push(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {} // no IPv6 here
        Err(e) => return Err(held(e)),
    }
    Ok(out)
}

/// Serves on the listeners until the task is dropped.
pub async fn serve(listeners: Vec<TcpListener>, routes: Routes) {
    let port = listeners
        .first()
        .and_then(|l| l.local_addr().ok())
        .map_or(0, |a| a.port());
    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let routes = routes.clone();
        tasks.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(req, routes.clone(), port));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
        });
    }
    while tasks.join_next().await.is_some() {}
}

fn full(status: StatusCode, content_type: &str, text: String) -> Response<Body> {
    let mut resp = Response::new(
        Full::new(Bytes::from(text))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("text/plain")),
    );
    resp
}

/// `a.app.localhost:7355` -> `a.app`.
fn label(host: &str) -> String {
    let host = host
        .rsplit_once(':')
        .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
        .map_or(host, |(h, _)| h);
    host.trim_end_matches('.')
        .to_ascii_lowercase()
        .trim_end_matches(".localhost")
        .to_string()
}

fn listing(routes: &Routes, port: u16, host: &str) -> Response<Body> {
    let mut names: Vec<String> = routes.read().unwrap().keys().cloned().collect();
    names.sort();
    let items: String = names
        .iter()
        .map(|n| format!("<li><a href=\"http://{n}.localhost:{port}/\">{n}</a></li>"))
        .collect();
    let body = format!(
        "<!doctype html><title>Strata</title><h1>No dev server for {host}</h1>{}",
        if names.is_empty() {
            "<p>No agent has a dev server running. Start one with <code>strata dev &lt;agent&gt;</code>.</p>".to_string()
        } else {
            format!("<p>Live agents:</p><ul>{items}</ul>")
        }
    );
    full(StatusCode::NOT_FOUND, "text/html; charset=utf-8", body)
}

async fn handle(
    mut req: Request<Incoming>,
    routes: Routes,
    port: u16,
) -> Result<Response<Body>, Infallible> {
    if req.headers().contains_key(HOP) {
        return Ok(full(
            StatusCode::LOOP_DETECTED,
            "text/plain",
            "loop: this request already went through the Strata proxy\n".into(),
        ));
    }
    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()))
        .unwrap_or("")
        .to_string();
    let Some(backend) = routes.read().unwrap().get(&label(&host)).copied() else {
        return Ok(listing(&routes, port, &host));
    };
    let bad_gateway = |e: &dyn std::fmt::Display| {
        full(
            StatusCode::BAD_GATEWAY,
            "text/plain",
            format!("dev server at {backend} is not answering: {e}\n"),
        )
    };
    let stream = match TcpStream::connect(backend).await {
        Ok(s) => s,
        Err(e) => return Ok(bad_gateway(&e)),
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(pair) => pair,
        Err(e) => return Ok(bad_gateway(&e)),
    };
    tokio::spawn(conn.with_upgrades());
    let upgrading = req.headers().contains_key(hyper::header::UPGRADE);
    let client_side = upgrading.then(|| hyper::upgrade::on(&mut req));
    let headers = req.headers_mut();
    headers.insert(HOP, HeaderValue::from_static("1"));
    if let Ok(v) = HeaderValue::from_str(&host) {
        headers.insert("x-forwarded-host", v);
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    let mut resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => return Ok(bad_gateway(&e)),
    };
    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_side) = client_side
    {
        let backend_side = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(client), Ok(server)) = tokio::join!(client_side, backend_side) {
                let _ = tokio::io::copy_bidirectional(
                    &mut TokioIo::new(client),
                    &mut TokioIo::new(server),
                )
                .await;
            }
        });
    }
    Ok(resp.map(|b| b.boxed()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_labels() {
        assert_eq!(
            super::label("Fix-Login.App.localhost:7355"),
            "fix-login.app"
        );
        assert_eq!(super::label("a.app.localhost"), "a.app");
        assert_eq!(super::label("localhost:7355"), "localhost");
    }
}
