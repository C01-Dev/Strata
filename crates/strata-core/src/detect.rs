//! What `strata init` and the checker need to know about a repo: package manager, Yarn PnP,
//! the repo's own TypeScript version and its dev script.

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub fn name(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }

    pub fn run(self, script: &str) -> String {
        match self {
            Self::Npm | Self::Bun => format!("{} run {script}", self.name()),
            Self::Pnpm | Self::Yarn => format!("{} {script}", self.name()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Detected {
    pub package_manager: Option<PackageManager>,
    pub yarn_pnp: bool,
    /// Version of `node_modules/typescript`, if installed.
    pub typescript: Option<String>,
    pub dev_command: Option<String>,
}

impl Detected {
    /// Why the shared checker is off for this repo, if it is.
    pub fn checker_off_reason(&self) -> Option<String> {
        if self.yarn_pnp {
            return Some("Yarn Plug'n'Play has no node_modules, so the shared checker is off; worktrees and processes still work".into());
        }
        match &self.typescript {
            None => Some("TypeScript is not installed in node_modules, so the shared checker is off (it needs 7.1 or later)".into()),
            Some(v) if !supports_api(v) => {
                Some(format!("TypeScript {v} is older than 7.1, so the shared checker is off; every other feature works"))
            }
            Some(_) => None,
        }
    }
}

/// 7.1 or later, prereleases included (7.1.0-beta has the API).
pub fn supports_api(version: &str) -> bool {
    let mut parts = version
        .split(['.', '-'])
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    major > 7 || (major == 7 && minor >= 1)
}

pub fn detect(root: &Path) -> Detected {
    let package_json: serde_json::Value = fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let exists = |f: &str| root.join(f).exists();
    let package_manager = if exists("pnpm-lock.yaml") {
        Some(PackageManager::Pnpm)
    } else if exists("bun.lock") || exists("bun.lockb") {
        Some(PackageManager::Bun)
    } else if exists("yarn.lock") {
        Some(PackageManager::Yarn)
    } else if exists("package-lock.json") || exists("npm-shrinkwrap.json") {
        Some(PackageManager::Npm)
    } else {
        // No lockfile yet: trust `packageManager`, else npm if there is a package.json at all.
        match package_json["packageManager"]
            .as_str()
            .and_then(|s| s.split('@').next())
        {
            Some("pnpm") => Some(PackageManager::Pnpm),
            Some("yarn") => Some(PackageManager::Yarn),
            Some("bun") => Some(PackageManager::Bun),
            _ if exists("package.json") => Some(PackageManager::Npm),
            _ => None,
        }
    };
    let typescript = fs::read_to_string(root.join("node_modules/typescript/package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["version"].as_str().map(String::from));
    let dev_command = match (package_manager, package_json["scripts"]["dev"].is_string()) {
        (Some(pm), true) => Some(pm.run("dev")),
        _ => None,
    };
    Detected {
        package_manager,
        yarn_pnp: package_manager == Some(PackageManager::Yarn) && yarn_pnp(root),
        typescript,
        dev_command,
    }
}

/// Yarn Berry defaults to Plug'n'Play unless `.yarnrc.yml` sets `nodeLinker: node-modules`.
/// Yarn 1 (no `.yarnrc.yml`) always uses node_modules.
fn yarn_pnp(root: &Path) -> bool {
    if root.join(".pnp.cjs").exists() {
        return true;
    }
    let Ok(rc) = fs::read_to_string(root.join(".yarnrc.yml")) else {
        return false;
    };
    let linker = rc
        .lines()
        .find_map(|l| l.trim().strip_prefix("nodeLinker:"))
        .map(|v| v.trim().trim_matches(['"', '\'']));
    linker.is_none_or(|v| v == "pnp")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn package_manager_from_lockfile() {
        for (lock, pm) in [
            ("pnpm-lock.yaml", PackageManager::Pnpm),
            ("bun.lock", PackageManager::Bun),
            ("bun.lockb", PackageManager::Bun),
            ("yarn.lock", PackageManager::Yarn),
            ("package-lock.json", PackageManager::Npm),
        ] {
            let r = repo(&[
                ("package.json", r#"{"scripts":{"dev":"next dev"}}"#),
                (lock, ""),
            ]);
            let d = detect(r.path());
            assert_eq!(d.package_manager, Some(pm), "{lock}");
            assert_eq!(d.dev_command, Some(pm.run("dev")));
        }
        assert_eq!(
            detect(repo(&[("package.json", r#"{"packageManager":"pnpm@10.0.0"}"#)]).path())
                .package_manager,
            Some(PackageManager::Pnpm)
        );
        assert_eq!(detect(repo(&[]).path()).package_manager, None);
    }

    #[test]
    fn yarn_linkers() {
        let berry_default = repo(&[
            ("yarn.lock", ""),
            (".yarnrc.yml", "yarnPath: .yarn/releases/yarn.cjs\n"),
        ]);
        let berry_nm = repo(&[
            ("yarn.lock", ""),
            (".yarnrc.yml", "nodeLinker: \"node-modules\"\n"),
        ]);
        let classic = repo(&[("yarn.lock", "")]);
        assert!(detect(berry_default.path()).yarn_pnp);
        assert!(!detect(berry_nm.path()).yarn_pnp);
        assert!(!detect(classic.path()).yarn_pnp);
        assert!(
            detect(berry_default.path())
                .checker_off_reason()
                .unwrap()
                .contains("Plug'n'Play")
        );
    }

    #[test]
    fn typescript_version_gates_the_checker() {
        let ts = |v: &str| {
            repo(&[
                ("package-lock.json", ""),
                (
                    "node_modules/typescript/package.json",
                    &format!(r#"{{"version":"{v}"}}"#),
                ),
            ])
        };
        assert_eq!(detect(ts("7.1.0-beta").path()).checker_off_reason(), None);
        assert_eq!(detect(ts("8.0.0").path()).checker_off_reason(), None);
        assert!(
            detect(ts("7.0.2").path())
                .checker_off_reason()
                .unwrap()
                .contains("older than 7.1")
        );
        assert!(
            detect(ts("5.9.3").path())
                .checker_off_reason()
                .unwrap()
                .contains("older than 7.1")
        );
        assert!(
            detect(repo(&[("package-lock.json", "")]).path())
                .checker_off_reason()
                .unwrap()
                .contains("not installed")
        );
    }
}
