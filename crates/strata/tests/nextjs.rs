//! Gate 3: five Next.js dev servers behind the proxy, hot reload checked over each one's HMR
//! WebSocket (through the proxy), and no process or port left after `strata rm`. Ignored by
//! default: the base installs Next with `npm ci` (from the npm cache after the first run).
//! Run: cargo test -p strata --test nextjs -- --ignored --nocapture

mod common;

use common::{set_ports, wait_secs, write_file};
use serde_json::Value;
use std::net::{Ipv6Addr, TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

macro_rules! fixture {
    ($($file:literal),*) => {
        [$(($file, include_str!(concat!("../../../tests/fixtures/nextjs/", $file)))),*]
    };
}

const AGENTS: [&str; 5] = ["a1", "a2", "a3", "a4", "a5"];
const PROXY: u16 = 47150;

fn host(agent: &str) -> String {
    format!("{agent}.app.localhost:{PROXY}")
}

/// The page body through the proxy.
fn page(agent: &str) -> String {
    let out = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "60",
            "-H",
            &format!("Host: {}", host(agent)),
        ])
        .arg(format!("http://127.0.0.1:{PROXY}/"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Next's HMR socket, opened through the proxy the way the browser does (Origin is the page).
fn hmr(agent: &str) -> tungstenite::WebSocket<TcpStream> {
    let stream = TcpStream::connect(("127.0.0.1", PROXY)).unwrap();
    let mut req = format!("ws://{}/_next/hmr", host(agent))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", format!("http://{}", host(agent)).parse().unwrap());
    let (ws, resp) = tungstenite::client(req, stream).unwrap();
    assert_eq!(resp.status(), 101);
    ws.get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    ws
}

/// Reads HMR messages until one of type `kind` arrives (60 s at most).
fn wait_message(ws: &mut tungstenite::WebSocket<TcpStream>, kind: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < deadline, "no {kind} message within 60 s");
        match ws.read() {
            Ok(Message::Text(t))
                if serde_json::from_str::<Value>(t.as_str()).is_ok_and(|v| v["type"] == kind) =>
            {
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("waiting for {kind}: {e}"),
        }
    }
}

/// Every process in the dev server's group, plus whatever listens on its port.
fn pids(pgid: &Value, port: u64) -> Vec<String> {
    let mut out = String::new();
    for cmd in [
        vec!["pgrep".to_string(), "-g".into(), pgid.to_string()],
        vec![
            "lsof".into(),
            "-t".into(),
            "-nP".into(),
            format!("-iTCP:{port}"),
            "-sTCP:LISTEN".into(),
        ],
    ] {
        let o = Command::new(&cmd[0]).args(&cmd[1..]).output().unwrap();
        out += &String::from_utf8_lossy(&o.stdout);
    }
    let mut pids: Vec<String> = out.split_whitespace().map(String::from).collect();
    pids.sort();
    pids.dedup();
    pids
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
        && TcpListener::bind((Ipv6Addr::LOCALHOST, port)).is_ok()
}

fn secs(t: Instant) -> String {
    format!("{:.2} s", t.elapsed().as_secs_f64())
}

#[test]
#[ignore = "installs Next.js in five worktrees; see the module docs"]
fn five_next_servers_hot_reload_behind_the_proxy_and_rm_leaves_nothing() {
    let e = common::env(&fixture!(
        ".gitignore",
        "package.json",
        "package-lock.json",
        "next.config.mjs",
        "app/layout.js",
        "app/page.js"
    ));
    let init = e.strata_with(
        &e.root,
        &["init", "--no-service"],
        &[("NEXT_TELEMETRY_DISABLED", "1")],
    );
    assert!(init.status.success(), "{init:?}");
    set_ports(&e, 47100, 47149, PROXY);

    for agent in AGENTS {
        let t = Instant::now();
        e.strata(&["new", agent]);
        println!("strata new {agent}: {}", secs(t));
    }
    let t = Instant::now();
    for agent in AGENTS {
        e.strata(&["dev", agent]);
    }
    wait_secs("five routes", 30, || {
        e.status()["routes"]
            .as_object()
            .is_some_and(|r| r.len() == 5)
    });
    println!("five dev servers started and routed: {}", secs(t));

    for agent in AGENTS {
        let t = Instant::now();
        let body = page(agent);
        assert!(body.contains("Hello from version 1"), "{agent}: {body}");
        println!("{agent}: first page through the proxy {}", secs(t));
        let mut ws = hmr(agent);
        wait_message(&mut ws, "turbopack-connected");
        let t = Instant::now();
        write_file(
            &e.wt(agent),
            "app/page.js",
            &format!("export default function Page() {{ return <h1>Hello from {agent}</h1>; }}\n"),
        );
        wait_message(&mut ws, "serverComponentChanges");
        println!("{agent}: edit to HMR message {}", secs(t));
        let body = page(agent);
        assert!(
            body.contains(&format!("Hello from {agent}")),
            "{agent}: {body}"
        );
    }
    for agent in AGENTS {
        assert!(
            page(agent).contains(&format!("Hello from {agent}")),
            "{agent} serves its own edit"
        );
    }

    let mut held = vec![];
    for agent in AGENTS {
        let a = e.agent(agent).unwrap();
        let port = a["port"].as_u64().unwrap();
        let pids = pids(&a["processes"][0]["pid"], port);
        println!(
            "{agent}: port {port}, {} processes, {} MB",
            pids.len(),
            a["memory_bytes"].as_u64().unwrap_or(0) / 1_000_000
        );
        held.push((agent, port as u16, pids));
    }
    for (agent, port, pids) in &held {
        let t = Instant::now();
        e.strata(&["rm", agent, "--force"]);
        println!("strata rm {agent}: {}", secs(t));
        let left: Vec<&String> = pids.iter().filter(|p| alive(p)).collect();
        assert!(left.is_empty(), "{agent}: still running {left:?}");
        assert!(port_free(*port), "{agent}: port {port} still held");
    }
    let s = e.status();
    assert!(s["routes"].as_object().unwrap().is_empty(), "{s}");
    assert!(
        s["repos"][0]["agents"].as_array().unwrap().is_empty(),
        "{s}"
    );
}

/// System-wide file-backed (page cache) bytes, from `vm_stat`. macOS only.
fn file_backed() -> i64 {
    let out = Command::new("vm_stat").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let page: i64 = text
        .split("page size of ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap();
    let pages: i64 = text
        .lines()
        .find_map(|l| l.strip_prefix("File-backed pages:"))
        .map(|s| s.trim().trim_end_matches('.').parse().unwrap())
        .unwrap();
    pages * page
}

/// Five Next servers, each worktree seeded by `clone` or by its own `install`: their metered
/// footprint and the growth of the system's page cache while they start and serve a page.
fn five_servers(install: bool) -> (u64, i64) {
    let e = common::env(&fixture!(
        ".gitignore",
        "package.json",
        "package-lock.json",
        "next.config.mjs",
        "app/layout.js",
        "app/page.js"
    ));
    let mut envs = vec![("NEXT_TELEMETRY_DISABLED", "1")];
    if install {
        envs.push(("STRATA_FAULT", "no-clone"));
    }
    let init = e.strata_with(&e.root, &["init", "--no-service"], &envs);
    assert!(init.status.success(), "{init:?}");
    set_ports(&e, 47100, 47149, PROXY);
    common::no_pool(&e);
    for agent in AGENTS {
        e.strata(&["new", agent]);
        let tier = e.agent(agent).unwrap()["seed_tier"].clone();
        assert_eq!(tier, if install { "install" } else { "clone" });
    }
    let before = file_backed();
    for agent in AGENTS {
        e.strata(&["dev", agent]);
    }
    wait_secs("five routes", 30, || {
        e.status()["routes"]
            .as_object()
            .is_some_and(|r| r.len() == 5)
    });
    for agent in AGENTS {
        assert!(page(agent).contains("Hello from version 1"));
    }
    std::thread::sleep(Duration::from_secs(15));
    let grown = file_backed() - before;
    let footprint = AGENTS
        .iter()
        .map(|a| e.agent(a).unwrap()["memory_bytes"].as_u64().unwrap_or(0))
        .sum();
    (footprint, grown)
}

/// PLAN 4.7: do cloned `node_modules` share page cache? Ignored like Gate 3; prints numbers.
#[test]
#[ignore = "five Next servers twice; prints memory numbers"]
fn memory_of_five_servers_cloned_vs_installed() {
    for round in 1..=2 {
        for install in [false, true] {
            let (footprint, grown) = five_servers(install);
            println!(
                "round {round}, {}: footprint {} MB, page cache {:+} MB",
                if install { "installed" } else { "cloned" },
                footprint / 1_000_000,
                grown / 1_000_000
            );
        }
    }
}
