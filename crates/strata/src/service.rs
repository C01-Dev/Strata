//! Per-user background service: a LaunchAgent on macOS, a `systemd --user` unit on Linux.
//! The files are written directly (PLAN 1.6 allows it; the Linux unit needs `Delegate=yes`).
//! Both restart the daemon only when it fails, so `strata daemon stop` stays stopped.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use strata_core::{Error, Result, strata_home};

pub const LABEL: &str = "dev.strata.daemon";

/// Writes the service file for this binary and `$STRATA_HOME`, then loads and starts it.
pub fn install() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let file = platform::file()?;
    fs::create_dir_all(file.parent().expect("has a parent"))?;
    fs::write(&file, platform::render(&exe, &strata_home(), &path_env))?;
    platform::load(&file)?;
    Ok(file)
}

pub fn uninstall() -> Result<()> {
    let file = platform::file()?;
    platform::unload()?;
    match fs::remove_file(&file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(cmd).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(Error::Refused(format!(
        "{cmd} {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

fn user_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

// Both renderers compile on both platforms so each platform's tests check both files.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn plist(exe: &Path, home: &Path, path_env: &str) -> String {
    let (exe, home, path_env) = (
        xml(&exe.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(path_env),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>daemon</string><string>start</string><string>--foreground</string></array>
  <key>EnvironmentVariables</key>
  <dict><key>STRATA_HOME</key><string>{home}</string><key>PATH</key><string>{path_env}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>{home}/logs/daemon.out</string>
  <key>StandardErrorPath</key><string>{home}/logs/daemon.out</string>
</dict>
</plist>
"#
    )
}

/// systemd quoting: one double-quoted word, `%` doubled (specifiers).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn quoted(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn unit(exe: &Path, home: &Path, path_env: &str) -> String {
    format!(
        "[Unit]
Description=Strata daemon (shared type checker and worktree runtime)

[Service]
ExecStart={} daemon start --foreground
Environment={}
Environment={}
Restart=on-failure
# Lets the daemon create one child cgroup per agent (Phase 3 memory budgets).
Delegate=yes

[Install]
WantedBy=default.target
",
        quoted(&exe.to_string_lossy()),
        quoted(&format!("STRATA_HOME={}", home.display())),
        quoted(&format!("PATH={path_env}")),
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub fn file() -> Result<PathBuf> {
        Ok(user_home()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    pub fn render(exe: &Path, home: &Path, path_env: &str) -> String {
        plist(exe, home, path_env)
    }

    fn domain() -> String {
        // SAFETY: getuid has no preconditions and cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    pub fn load(file: &Path) -> Result<()> {
        let _ = unload(); // reinstall: replace a loaded copy
        run(
            "launchctl",
            &["bootstrap", &domain(), &file.to_string_lossy()],
        )
    }

    pub fn unload() -> Result<()> {
        run("launchctl", &["bootout", &format!("{}/{LABEL}", domain())])
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn file() -> Result<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".config"));
        Ok(config.join("systemd/user/strata.service"))
    }

    pub fn render(exe: &Path, home: &Path, path_env: &str) -> String {
        unit(exe, home, path_env)
    }

    pub fn load(_file: &Path) -> Result<()> {
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "strata.service"])?;
        run("systemctl", &["--user", "restart", "strata.service"])
    }

    pub fn unload() -> Result<()> {
        let _ = run(
            "systemctl",
            &["--user", "disable", "--now", "strata.service"],
        );
        run("systemctl", &["--user", "daemon-reload"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/Applications/My Tools/strata";
    const HOME: &str = "/Users/a&b/.strata";

    #[test]
    fn plist_is_valid_and_escaped() {
        let text = plist(
            Path::new(EXE),
            Path::new(HOME),
            "/opt/homebrew/bin:/usr/bin",
        );
        assert!(text.contains("<string>/Users/a&amp;b/.strata</string>"));
        assert!(text.contains("<key>SuccessfulExit</key><false/>"));
        if Path::new("/usr/bin/plutil").exists() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("x.plist");
            fs::write(&file, &text).unwrap();
            let out = Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }

    #[test]
    fn unit_quotes_paths_and_delegates() {
        let text = unit(Path::new(EXE), Path::new("/home/a/50%/.strata"), "/usr/bin");
        assert!(
            text.contains("ExecStart=\"/Applications/My Tools/strata\" daemon start --foreground")
        );
        assert!(text.contains("Environment=\"STRATA_HOME=/home/a/50%%/.strata\""));
        assert!(text.contains("Delegate=yes") && text.contains("Restart=on-failure"));
    }

    /// Installs the real service for this user. Only with STRATA_SYSTEM_TESTS=1 (CI).
    #[test]
    fn install_and_uninstall() {
        if std::env::var_os("STRATA_SYSTEM_TESTS").is_none() {
            return;
        }
        let file = install().unwrap();
        assert!(file.exists());
        let mut up = false;
        for _ in 0..100 {
            if crate::client::get("/v1/status").is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        uninstall().unwrap();
        assert!(up, "the service started the daemon");
        assert!(!file.exists());
    }
}
