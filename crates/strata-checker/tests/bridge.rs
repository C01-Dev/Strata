mod common;

use serde_json::json;
use std::time::Duration;
use strata_checker::bridge::{Bridge, install_script, node};
use strata_core::Error;

const T: Duration = Duration::from_secs(60);

async fn start(tmp: &std::path::Path) -> (Bridge, std::path::PathBuf) {
    let root = common::project(tmp, "app");
    let script = install_script(&tmp.join("home")).unwrap();
    (Bridge::spawn(&node(), &script, &root).await.unwrap(), root)
}

#[tokio::test]
async fn hello_open_and_query() {
    let tmp = tempfile::tempdir().unwrap();
    let (bridge, root) = start(tmp.path()).await;
    assert_eq!(bridge.typescript, "7.1.0-beta");
    let tsconfig = root.join("tsconfig.json");
    let opened = bridge
        .call(
            "openBase",
            json!({ "group": "g", "root": root, "tsconfigs": [tsconfig] }),
            T,
        )
        .await
        .unwrap();
    assert!(opened["files"].as_u64().unwrap() >= 2);
    let wt = tmp.path().join("wt-b.ts");
    std::fs::write(&wt, "export const total: string = 1;\n").unwrap();
    let files = json!({ root.join("src/b.ts").to_str().unwrap(): wt });
    bridge
        .call(
            "createSnapshot",
            json!({ "group": "g", "agent": "a", "files": files, "removed": [] }),
            T,
        )
        .await
        .unwrap();
    let d = bridge
        .call("diagnostics", json!({ "agent": "a" }), T)
        .await
        .unwrap();
    assert_eq!(d["total"], 1);
    assert_eq!(d["items"][0]["code"], 2322);
    assert_eq!(
        d["items"][0]["file"],
        root.join("src/b.ts").to_str().unwrap()
    );
    let err = bridge
        .call(
            "diagnostics",
            json!({ "agent": "a", "file": "/nowhere.ts" }),
            T,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    let err = bridge.call("nope", json!({}), T).await.unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err}");
    bridge.shutdown().await;
}

#[tokio::test]
async fn timeouts_do_not_poison_later_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let (bridge, root) = start(tmp.path()).await;
    let open = json!({ "group": "g", "root": root, "tsconfigs": [root.join("tsconfig.json")] });
    let err = bridge
        .call("openBase", open.clone(), Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
    bridge.call("openBase", open, T).await.unwrap();
    assert!(
        bridge.call("stats", json!({}), T).await.unwrap()["snapshots"]
            .as_u64()
            .unwrap()
            >= 1
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn a_dead_bridge_fails_pending_and_later_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let (bridge, root) = start(tmp.path()).await;
    let open = json!({ "group": "g", "root": root, "tsconfigs": [root.join("tsconfig.json")] });
    let pending = bridge.call("openBase", open, T);
    let kill = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::process::Command::new("kill")
            .args(["-9", &bridge.pid.to_string()])
            .status()
            .unwrap();
    };
    let (result, ()) = tokio::join!(pending, kill);
    assert!(result.unwrap_err().to_string().contains("restarting"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!bridge.alive());
    assert!(
        bridge
            .call("stats", json!({}), T)
            .await
            .unwrap_err()
            .to_string()
            .contains("restarting")
    );
}

#[test]
fn script_is_installed_once_per_build() {
    let home = tempfile::tempdir().unwrap();
    let a = install_script(home.path()).unwrap();
    let b = install_script(home.path()).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        std::fs::read_to_string(a).unwrap(),
        strata_checker::bridge::SCRIPT
    );
}
