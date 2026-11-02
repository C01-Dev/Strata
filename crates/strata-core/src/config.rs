//! `strata.toml` in the repo root.

use crate::{Error, Result};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

pub const FILE: &str = "strata.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub repo: Repo,
    pub seed: Seed,
    pub checker: Checker,
    pub dev: Dev,
    pub budget: Budget,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Repo {
    /// Branch Strata's own base worktree follows.
    pub base: String,
    /// Where agent worktrees live, relative to the repo root. Default `../.strata/<repo folder>`.
    pub worktree_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Seed {
    pub paths: Vec<String>,
    pub fallback: Fallback,
    pub warm_pool: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    Install,
    Copy,
    Hardlink,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Checker {
    pub typescript: String,
    pub tsconfig: Vec<PathBuf>,
    pub idle_dispose_secs: u64,
    pub huge_diff_ratio: f64,
    pub group_memory_mb: u64,
    pub extra_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dev {
    pub command: Option<String>,
    pub port_env: String,
    pub restart: bool,
    pub port_range: [u16; 2],
    pub proxy_port: u16,
    pub app: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub per_agent_mb: u64,
    pub action: BudgetAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetAction {
    Warn,
    Stop,
}

impl Default for Repo {
    fn default() -> Self {
        Self {
            base: "main".into(),
            worktree_dir: None,
        }
    }
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            paths: vec!["node_modules".into()],
            fallback: Fallback::Install,
            warm_pool: 1,
        }
    }
}

impl Default for Checker {
    fn default() -> Self {
        Self {
            typescript: "auto".into(),
            tsconfig: vec!["tsconfig.json".into()],
            idle_dispose_secs: 900,
            huge_diff_ratio: 0.2,
            group_memory_mb: 6144,
            extra_paths: vec![],
        }
    }
}

impl Default for Dev {
    fn default() -> Self {
        Self {
            command: None,
            port_env: "PORT".into(),
            restart: false,
            port_range: [4100, 4999],
            proxy_port: 7355,
            app: None,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            per_agent_mb: 4096,
            action: BudgetAction::Warn,
        }
    }
}

impl Config {
    /// Reads and validates `<root>/strata.toml`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Config {
                    path,
                    message: "not found; run `strata init` in the repo".into(),
                });
            }
            Err(e) => {
                return Err(Error::Config {
                    path,
                    message: e.to_string(),
                });
            }
        };
        Self::parse(&text).map_err(|message| Error::Config { path, message })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let [lo, hi] = self.dev.port_range;
        if self.repo.base.trim().is_empty() {
            return Err("repo.base must name a branch".into());
        }
        if lo > hi {
            return Err(format!("dev.port_range [{lo}, {hi}] is reversed"));
        }
        if (lo..=hi).contains(&self.dev.proxy_port) {
            return Err(format!(
                "dev.proxy_port {} is inside dev.port_range",
                self.dev.proxy_port
            ));
        }
        if !(self.checker.huge_diff_ratio > 0.0 && self.checker.huge_diff_ratio <= 1.0) {
            return Err("checker.huge_diff_ratio must be above 0 and at most 1".into());
        }
        if self.checker.tsconfig.is_empty() {
            return Err("checker.tsconfig needs at least one path".into());
        }
        if let Some(app) = &self.dev.app
            && !is_dns_label(app)
        {
            return Err(format!(
                "dev.app {app:?} must be lowercase letters, digits and hyphens (it becomes a hostname)"
            ));
        }
        Ok(())
    }

    /// Absolute folder for agent worktrees (lexically normalized: it may not exist yet).
    pub fn worktree_dir(&self, root: &Path) -> PathBuf {
        match &self.repo.worktree_dir {
            Some(dir) => normalize(&root.join(dir)),
            None => root
                .parent()
                .unwrap_or(root)
                .join(".strata")
                .join(root.file_name().unwrap_or_default()),
        }
    }
}

/// A lowercase DNS label, because agent and app names end up in `<agent>.<app>.localhost`.
pub fn is_dns_label(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// `name` made into a DNS label (folder names become `dev.app` and adopted agent names).
pub fn to_dns_label(name: &str) -> String {
    let label: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = label.trim_matches('-');
    let label = label[..label.len().min(63)].trim_end_matches('-');
    if label.is_empty() {
        "app".into()
    } else {
        label.into()
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// The file `strata init` writes, with detected values filled in.
pub fn template(base: &str, worktree_dir: &str, dev_command: Option<&str>, app: &str) -> String {
    let dev = match dev_command {
        Some(cmd) => format!("command = {cmd:?}"),
        None => "# command = \"npm run dev\"       # no dev script found in package.json".into(),
    };
    format!(
        r#"[repo]
base = {base:?}                     # branch Strata's own base worktree follows
worktree_dir = {worktree_dir:?} # where agent worktrees live

[seed]
paths = ["node_modules", "apps/*/node_modules", "packages/*/node_modules"]
fallback = "install"              # after clone: install | copy | hardlink
warm_pool = 1

[checker]
typescript = "auto"               # the repo's own TypeScript; must be 7.1 or later
tsconfig = ["tsconfig.json"]
idle_dispose_secs = 900
huge_diff_ratio = 0.2             # above this share of changed files, the agent gets its own snapshot
group_memory_mb = 6144            # above this, dispose least recently used snapshots
extra_paths = []                  # per-worktree files under node_modules to layer

[dev]
{dev}
port_env = "PORT"
restart = false                   # restart dev servers after a reboot
port_range = [4100, 4999]
proxy_port = 7355
app = {app:?}                     # gives <agent>.{app}.localhost

[budget]
per_agent_mb = 4096
action = "warn"                   # warn | stop
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_plan_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.repo.base, "main");
        assert_eq!(c.dev.proxy_port, 7355);
        assert_eq!(c.dev.port_range, [4100, 4999]);
        assert_eq!(c.checker.idle_dispose_secs, 900);
        assert_eq!(c.budget.action, BudgetAction::Warn);
        assert_eq!(c.seed.fallback, Fallback::Install);
    }

    #[test]
    fn template_round_trips() {
        let c = Config::parse(&template(
            "trunk",
            "../.strata/web",
            Some("pnpm dev"),
            "web",
        ))
        .unwrap();
        assert_eq!(c.repo.base, "trunk");
        assert_eq!(c.dev.command.as_deref(), Some("pnpm dev"));
        assert_eq!(c.dev.app.as_deref(), Some("web"));
        assert_eq!(
            c.worktree_dir(Path::new("/src/web")),
            Path::new("/src/.strata/web")
        );
        assert!(
            Config::parse(&template("main", "../wt", None, "x"))
                .unwrap()
                .dev
                .command
                .is_none()
        );
    }

    #[test]
    fn default_worktree_dir_is_next_to_the_repo() {
        assert_eq!(
            Config::default().worktree_dir(Path::new("/src/web")),
            Path::new("/src/.strata/web")
        );
    }

    #[test]
    fn rejects_bad_values() {
        for (toml, needle) in [
            ("[repo]\nbsae = \"main\"", "unknown field"),
            ("[seed]\nfallback = \"rsync\"", "unknown variant"),
            ("[dev]\nport_range = [5000, 4000]", "reversed"),
            ("[dev]\nproxy_port = 4500", "inside dev.port_range"),
            ("[checker]\nhuge_diff_ratio = 0", "huge_diff_ratio"),
            ("[dev]\napp = \"My App\"", "hostname"),
            ("[repo]\nbase = \"\"", "repo.base"),
        ] {
            let err = Config::parse(toml).unwrap_err();
            assert!(err.contains(needle), "{toml:?} -> {err}");
        }
    }

    #[test]
    fn missing_file_says_run_init() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Config::load(dir.path())
                .unwrap_err()
                .to_string()
                .contains("strata init")
        );
    }

    #[test]
    fn to_dns_label_from_folder_names() {
        assert_eq!(to_dns_label("My_App.v2"), "my-app-v2");
        assert_eq!(to_dns_label("日本"), "app");
        assert!(is_dns_label(&to_dns_label(&format!(
            "{}-x",
            "a".repeat(62)
        ))));
    }

    #[test]
    fn dns_labels() {
        assert!(is_dns_label("fix-login2"));
        for bad in ["", "-a", "a-", "A", "a_b", "a.b", &"a".repeat(64)] {
            assert!(!is_dns_label(bad), "{bad}");
        }
    }
}
