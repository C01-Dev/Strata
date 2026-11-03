mod common;

use std::os::unix::fs::PermissionsExt;

#[test]
fn start_status_stop() {
    let e = common::env(&[]);
    assert!(e.strata_err(&["daemon", "status"]).contains("not running"));
    assert!(e.strata(&["daemon", "start"]).contains("started"));
    assert!(e.strata(&["daemon", "status"]).contains("running (pid"));
    assert!(e.strata(&["daemon", "start"]).contains("already running"));
    let mode = std::fs::metadata(e.home.join("strata.sock"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "socket mode {mode:o}");
    let status: serde_json::Value = serde_json::from_str(&e.strata(&["status", "--json"])).unwrap();
    assert!(status["pid"].as_u64().unwrap() > 0);
    assert!(e.strata(&["status"]).contains("no repos"));
    assert!(e.home.join("logs/daemon.log").exists());
    assert!(e.strata(&["daemon", "stop"]).contains("stopped"));
    assert!(!e.home.join("strata.sock").exists());
    assert!(e.strata(&["daemon", "stop"]).contains("not running"));
}

#[test]
fn a_second_daemon_exits_with_a_message() {
    let e = common::env(&[]);
    e.strata(&["daemon", "start"]);
    let err = e.strata_err(&["daemon", "start", "--foreground"]);
    assert!(err.contains("another Strata daemon is running"), "{err}");
    assert!(
        e.strata(&["daemon", "status"]).contains("running"),
        "the first daemon must survive"
    );
}

#[test]
fn restart_after_sigkill_reclaims_the_socket() {
    let e = common::env(&[]);
    e.strata(&["daemon", "start"]);
    let status: serde_json::Value = serde_json::from_str(&e.strata(&["status", "--json"])).unwrap();
    let pid = status["pid"].to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        e.home.join("strata.sock").exists(),
        "a SIGKILLed daemon leaves its socket behind"
    );
    assert!(e.strata(&["daemon", "start"]).contains("started"));
    let again: serde_json::Value = serde_json::from_str(&e.strata(&["status", "--json"])).unwrap();
    assert_ne!(again["pid"].to_string(), pid);
}

#[test]
fn socket_path_too_long_is_refused() {
    let e = common::env(&[]);
    let long = e.tmp.path().join("h".repeat(110));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["daemon", "start", "--foreground"])
        .env("STRATA_HOME", &long)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("longer than 103 bytes"));
}

#[test]
fn status_watch_redraws_until_stopped() {
    let e = common::env(&[]);
    e.strata(&["daemon", "start"]);
    let mut watch = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["status", "--watch"])
        .env("STRATA_HOME", &e.home)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3000));
    watch.kill().unwrap();
    let out = String::from_utf8(watch.wait_with_output().unwrap().stdout).unwrap();
    assert!(out.matches("\x1b[2J").count() >= 2, "{out}");
    assert!(out.matches("no repos").count() >= 2, "{out}");
    assert!(
        e.strata_err(&["status", "--watch", "--json"])
            .contains("cannot be used with")
    );
}
