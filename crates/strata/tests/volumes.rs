//! Edge cases that need a second volume: a worktree folder on another volume (EXDEV) and a
//! disk that fills while cloning (ENOSPC). macOS only: a small APFS disk image, which
//! `hdiutil` makes and mounts without root.
#![cfg(target_os = "macos")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// A mounted APFS image of `mb` megabytes, detached on drop.
struct Volume {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Volume {
    fn new(mb: u32) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (img, dir) = (tmp.path().join("v.dmg"), tmp.path().join("v"));
        std::fs::create_dir(&dir).unwrap();
        let ok = |c: &mut Command| assert!(c.status().unwrap().success(), "{c:?}");
        ok(Command::new("hdiutil")
            .args([
                "create",
                "-quiet",
                "-fs",
                "APFS",
                "-volname",
                "strata-test",
                "-size",
            ])
            .arg(format!("{mb}m"))
            .arg(&img));
        ok(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&dir)
            .arg(&img));
        let dir = dir.canonicalize().unwrap();
        Self { dir, _tmp: tmp }
    }

    /// Fills the volume to within `left` bytes of full.
    fn fill(&self, left: u64) {
        use std::io::Write;
        let path = self.dir.join("fill");
        let mut f = std::fs::File::create(&path).unwrap();
        let chunk = vec![1u8; 1 << 20];
        while f.write_all(&chunk).is_ok() {}
        let size = std::fs::metadata(&path).unwrap().len();
        f.set_len(size.saturating_sub(left)).unwrap();
        f.sync_all().unwrap();
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.dir)
            .status();
    }
}

fn stderr_of(e: &common::Env, dir: &Path, args: &[&str]) -> String {
    let out = e.strata_in(dir, args);
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn a_worktree_folder_on_another_volume_warns_and_installs() {
    let vol = Volume::new(64);
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        ("package.json", r#"{"name":"app","private":true}"#),
    ]);
    std::fs::create_dir_all(e.root.join("node_modules/pkg")).unwrap();
    let wts = vol.dir.join("worktrees");
    std::fs::write(
        e.root.join("strata.toml"),
        format!(
            "[repo]\nworktree_dir = \"{}\"\n\n[seed]\nwarm_pool = 0\n",
            wts.display()
        ),
    )
    .unwrap();
    let out = stderr_of(&e, &e.root, &["init", "--no-service"]);
    assert!(out.contains("is on another volume than the repo"), "{out}");
    assert!(out.contains("dependencies: install"), "{out}");
    let doctor = String::from_utf8_lossy(&e.strata_in(&e.root, &["doctor"]).stdout).into_owned();
    assert!(
        doctor.contains("on another volume than the repo"),
        "{doctor}"
    );
}

#[test]
fn a_disk_that_fills_while_cloning_stops_cleanly() {
    let vol = Volume::new(64);
    let root = vol.dir.join("app");
    std::fs::create_dir(&root).unwrap();
    common::git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".gitignore"), "node_modules\n").unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"app","private":true}"#,
    )
    .unwrap();
    std::fs::write(root.join("strata.toml"), "[seed]\nwarm_pool = 0\n").unwrap();
    common::git(&root, &["add", "."]);
    common::git(&root, &["commit", "-q", "-m", "init"]);
    // 20,000 entries: about 4 MB of metadata for every clone.
    for p in 0..200 {
        let dir = root.join(format!("node_modules/p{p}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            std::fs::write(dir.join(format!("f{f}")), "").unwrap();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let e = common::Env {
        root: root.canonicalize().unwrap(),
        home: tmp.path().canonicalize().unwrap().join("home"),
        tmp,
    };
    let out = stderr_of(&e, &e.root, &["init", "--no-service"]);
    assert!(
        out.contains("dependencies: clone"),
        "the base clones while there is room: {out}"
    );
    common::own_ports(&e);

    vol.fill(2 << 20);
    let out = e.strata_in(&e.root, &["new", "a"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the worktree itself is made: {stderr}"
    );
    assert!(
        stderr.contains("disk full while cloning dependencies"),
        "{stderr}"
    );
    assert!(stderr.contains("MB free"), "{stderr}");
    assert_eq!(e.agent("a").unwrap()["seed_tier"], "clone-failed");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    assert!(!wt.join("node_modules").exists(), "no half-cloned folder");
    assert!(
        !wt.join(".strata-seed-node_modules").exists(),
        "no temporary folder"
    );
    std::fs::remove_file(vol.dir.join("fill")).unwrap();
    e.strata(&["rm", "a", "--force"]);
}
