//! What `strata init` and the checker need to know about a repo: package manager, Yarn PnP,
//! the repo's own TypeScript version and its dev script.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

    /// The install command for a checkout at `root`: frozen to the lockfile when there is one;
    /// without one, told not to write a lockfile (that would dirty the worktree and change its
    /// dependency key).
    pub fn install(self, root: &Path) -> Vec<&'static str> {
        let has = |f: &str| root.join(f).exists();
        match self {
            Self::Npm if has("package-lock.json") || has("npm-shrinkwrap.json") => {
                vec!["npm", "ci"]
            }
            Self::Npm => vec!["npm", "install", "--no-package-lock"],
            Self::Pnpm if has("pnpm-lock.yaml") => vec!["pnpm", "install", "--frozen-lockfile"],
            Self::Pnpm => vec!["pnpm", "install", "--lockfile=false"],
            Self::Yarn if has(".yarnrc.yml") => vec!["yarn", "install", "--immutable"],
            Self::Yarn => vec!["yarn", "install", "--frozen-lockfile"],
            Self::Bun if has("bun.lock") || has("bun.lockb") => {
                vec!["bun", "install", "--frozen-lockfile"]
            }
            Self::Bun => vec!["bun", "install", "--no-save"],
        }
    }

    /// Rebuilds the named native addons in place (PLAN 4.4). Yarn 1 and bun have no rebuild
    /// command; npm's works on any `node_modules`.
    pub fn rebuild(self, root: &Path, names: &[String]) -> Vec<String> {
        let base = match self {
            Self::Pnpm => ["pnpm", "rebuild"],
            Self::Yarn if root.join(".yarnrc.yml").exists() => ["yarn", "rebuild"],
            _ => ["npm", "rebuild"],
        };
        base.iter()
            .map(|a| a.to_string())
            .chain(names.iter().cloned())
            .collect()
    }

    pub fn run(self, script: &str) -> String {
        match self {
            Self::Npm | Self::Bun => format!("{} run {script}", self.name()),
            Self::Pnpm | Self::Yarn => format!("{} {script}", self.name()),
        }
    }
}

/// Dev-server frameworks that need help with the port (PLAN 3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    /// `next dev` reads `PORT`.
    Next,
    /// Ignores `PORT`; needs `--port` (and `--strictPort`, so a taken port fails instead of moving).
    Vite,
    /// Ignores `PORT`; needs `--port`.
    Astro,
    /// Assumed to read `PORT`.
    Generic,
}

/// From the dependencies in `<root>/package.json`.
pub fn framework(root: &Path) -> Framework {
    let pkg: serde_json::Value = fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let has = |name: &str| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|k| pkg[k].get(name).is_some())
    };
    if has("next") {
        Framework::Next
    } else if has("astro") {
        Framework::Astro
    } else if has("vite") || has("@sveltejs/kit") {
        Framework::Vite
    } else {
        Framework::Generic
    }
}

/// The dev script's command with the framework's port flags. npm and bun need `--` before
/// arguments for the script; pnpm and yarn pass them through.
pub fn dev_command(pm: PackageManager, framework: Framework, port: u16) -> String {
    let base = pm.run("dev");
    let sep = if matches!(pm, PackageManager::Npm | PackageManager::Bun) {
        " --"
    } else {
        ""
    };
    match framework {
        Framework::Vite => format!("{base}{sep} --port {port} --strictPort"),
        Framework::Astro => format!("{base}{sep} --port {port}"),
        Framework::Next | Framework::Generic => base,
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
        // No lockfile yet: trust `packageManager`, then a pnpm workspace file, else npm if
        // there is a package.json at all.
        match package_json["packageManager"]
            .as_str()
            .and_then(|s| s.split('@').next())
        {
            Some("pnpm") => Some(PackageManager::Pnpm),
            None if exists("pnpm-workspace.yaml") => Some(PackageManager::Pnpm),
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

/// The `node` to run: `STRATA_NODE`, else `node` on `PATH`.
pub fn node() -> PathBuf {
    std::env::var_os("STRATA_NODE").map_or_else(|| PathBuf::from("node"), PathBuf::from)
}

/// Node ABI and platform (`"127 darwin-arm64"`), part of every `dep_key`: native addons built
/// for one do not load in another. Asked once per process.
pub fn node_runtime() -> crate::Result<String> {
    static RUNTIME: OnceLock<String> = OnceLock::new();
    if let Some(r) = RUNTIME.get() {
        return Ok(r.clone());
    }
    let out = std::process::Command::new(node())
        .args([
            "-p",
            "process.versions.modules + ' ' + process.platform + '-' + process.arch",
        ])
        .output()
        .map_err(|e| crate::Error::Refused(format!("cannot run node ({e})")))?;
    let r = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(RUNTIME.get_or_init(|| r).clone())
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
            detect(repo(&[("package.json", "{}"), ("pnpm-workspace.yaml", "")]).path())
                .package_manager,
            Some(PackageManager::Pnpm)
        );
        assert_eq!(
            detect(repo(&[("package.json", r#"{"packageManager":"pnpm@10.0.0"}"#)]).path())
                .package_manager,
            Some(PackageManager::Pnpm)
        );
        assert_eq!(detect(repo(&[]).path()).package_manager, None);
    }

    #[test]
    fn install_commands_follow_the_lockfile() {
        let r = repo(&[("package.json", "{}")]);
        assert_eq!(
            PackageManager::Npm.install(r.path()),
            ["npm", "install", "--no-package-lock"]
        );
        let r = repo(&[("package.json", "{}"), ("package-lock.json", "{}")]);
        assert_eq!(PackageManager::Npm.install(r.path()), ["npm", "ci"]);
        let r = repo(&[
            ("yarn.lock", ""),
            (".yarnrc.yml", "nodeLinker: node-modules\n"),
        ]);
        assert_eq!(
            PackageManager::Yarn.install(r.path()),
            ["yarn", "install", "--immutable"]
        );
        let r = repo(&[("pnpm-lock.yaml", "")]);
        assert_eq!(
            PackageManager::Pnpm.install(r.path()),
            ["pnpm", "install", "--frozen-lockfile"]
        );
    }

    #[test]
    fn presets() {
        let r = |deps: &str| repo(&[("package.json", deps)]);
        assert_eq!(
            framework(r(r#"{"dependencies":{"next":"16.4.4"}}"#).path()),
            Framework::Next
        );
        assert_eq!(
            framework(r(r#"{"devDependencies":{"vite":"7"}}"#).path()),
            Framework::Vite
        );
        assert_eq!(
            framework(r(r#"{"devDependencies":{"@sveltejs/kit":"2"}}"#).path()),
            Framework::Vite
        );
        assert_eq!(
            framework(r(r#"{"dependencies":{"astro":"5","vite":"7"}}"#).path()),
            Framework::Astro
        );
        assert_eq!(framework(r("{}").path()), Framework::Generic);
        assert_eq!(
            dev_command(PackageManager::Npm, Framework::Next, 4100),
            "npm run dev"
        );
        assert_eq!(
            dev_command(PackageManager::Npm, Framework::Vite, 4100),
            "npm run dev -- --port 4100 --strictPort"
        );
        assert_eq!(
            dev_command(PackageManager::Pnpm, Framework::Vite, 4100),
            "pnpm dev --port 4100 --strictPort"
        );
        assert_eq!(
            dev_command(PackageManager::Bun, Framework::Astro, 4100),
            "bun run dev -- --port 4100"
        );
        assert_eq!(
            dev_command(PackageManager::Yarn, Framework::Astro, 4100),
            "yarn dev --port 4100"
        );
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
