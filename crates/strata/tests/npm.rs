//! PLAN 5.3: the npm packages. The launcher runs the platform package's binary, passes its exit
//! code through, and refuses a binary that does not match the checksum built into it.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str]) -> std::process::Output {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn the_launcher_runs_the_checked_binary() {
    if Command::new("npm").arg("--version").output().is_err() {
        eprintln!("skipped: needs npm");
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let t = tempfile::tempdir().unwrap();
    let platform = String::from_utf8(
        run(
            t.path(),
            "node",
            &["-p", "process.platform + '-' + process.arch"],
        )
        .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let out = t.path().join("out");
    run(
        &repo,
        "node",
        &[
            "npm/build.mjs",
            env!("CARGO_PKG_VERSION"),
            out.to_str().unwrap(),
            &format!("{platform}={}", env!("CARGO_BIN_EXE_strata")),
        ],
    );
    let dest = t.path().to_str().unwrap();
    for pkg in ["strata".to_string(), format!("strata-{platform}")] {
        run(
            &out.join(pkg),
            "npm",
            &["pack", "--silent", "--pack-destination", dest],
        );
    }
    let v = env!("CARGO_PKG_VERSION");
    run(
        t.path(),
        "npm",
        &[
            "install",
            "--prefix",
            "prefix",
            "--offline",
            "--no-audit",
            "--no-fund",
            &format!("./owner-strata-{v}.tgz"),
            &format!("./owner-strata-{platform}-{v}.tgz"),
        ],
    );
    let strata = t.path().join("prefix/node_modules/.bin/strata");
    let version = Command::new(&strata).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("strata {v}")
    );
    let bad = Command::new(&strata)
        .arg("no-such-command")
        .output()
        .unwrap();
    assert_eq!(
        bad.status.code(),
        Some(2),
        "clap's exit code passes through"
    );

    let binary = t.path().join(format!(
        "prefix/node_modules/@owner/strata-{platform}/bin/strata"
    ));
    let mut bytes = std::fs::read(&binary).unwrap();
    bytes.push(0);
    std::fs::write(&binary, bytes).unwrap();
    let tampered = Command::new(&strata).arg("--version").output().unwrap();
    assert!(!tampered.status.success());
    assert!(
        String::from_utf8_lossy(&tampered.stderr).contains("does not match the checksum"),
        "{tampered:?}"
    );
}
