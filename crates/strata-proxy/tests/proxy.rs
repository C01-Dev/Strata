use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use strata_proxy::{Routes, bind, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

/// A plain HTTP/1.1 backend that answers with the Host header it received.
async fn http_backend() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0; 8192];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let host = req
                    .lines()
                    .find_map(|l| l.strip_prefix("host: "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let body = format!("backend saw host {host}");
                s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            });
        }
    });
    addr
}

/// A WebSocket backend that echoes text messages.
async fn ws_backend() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if msg.is_text() {
                        ws.send(msg).await.unwrap();
                    }
                }
            });
        }
    });
    addr
}

async fn proxy() -> (u16, Routes) {
    let routes: Routes = Default::default();
    let listeners = bind(0).await.unwrap();
    let port = listeners[0].local_addr().unwrap().port();
    // bind(0) gives each family its own port; the tests use the IPv4 one.
    tokio::spawn(serve(listeners, routes.clone()));
    (port, routes)
}

async fn get(port: u16, host: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(format!("GET / HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split(' ').nth(1).unwrap().parse().unwrap();
    (status, out.split_once("\r\n\r\n").unwrap().1.to_string())
}

#[tokio::test]
async fn routes_by_host_and_keeps_the_host_header() {
    let (port, routes) = proxy().await;
    let backend = http_backend().await;
    routes.write().unwrap().insert("a.app".into(), backend);
    let (status, body) = get(port, &format!("a.app.localhost:{port}")).await;
    assert_eq!(status, 200);
    assert_eq!(body, format!("backend saw host a.app.localhost:{port}"));
}

#[tokio::test]
async fn unknown_hosts_get_a_list_of_live_agents() {
    let (port, routes) = proxy().await;
    let backend = http_backend().await;
    routes.write().unwrap().insert("b.app".into(), backend);
    let (status, body) = get(port, &format!("nope.app.localhost:{port}")).await;
    assert_eq!(status, 404);
    assert!(
        body.contains(&format!("href=\"http://b.app.localhost:{port}/\"")),
        "{body}"
    );
}

#[tokio::test]
async fn loops_and_dead_backends() {
    let (port, routes) = proxy().await;
    routes
        .write()
        .unwrap()
        .insert("loop.app".into(), SocketAddr::from(([127, 0, 0, 1], port)));
    assert_eq!(get(port, "loop.app.localhost").await.0, 508);
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap(); // closed again
    routes.write().unwrap().insert("dead.app".into(), dead);
    let (status, body) = get(port, "dead.app.localhost").await;
    assert_eq!(status, 502);
    assert!(body.contains("not answering"), "{body}");
}

#[tokio::test]
async fn websockets_pass_through() {
    let (port, routes) = proxy().await;
    let backend = ws_backend().await;
    routes.write().unwrap().insert("ws.app".into(), backend);
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!("ws://ws.app.localhost:{port}/_next/webpack-hmr");
    let (mut ws, resp) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    for text in ["hello", "hot reload"] {
        ws.send(Message::text(text)).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap(), Message::text(text));
    }
}

#[tokio::test]
async fn a_held_port_names_its_holder() {
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = holder.local_addr().unwrap().port();
    let err = bind(port).await.unwrap_err();
    assert!(
        err.contains(&format!("proxy port {port} is held by"))
            && err.contains(&format!("(pid {})", std::process::id())),
        "{err}"
    );
    assert!(err.contains("dev.proxy_port"), "{err}");
}
