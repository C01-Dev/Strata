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
    /// Where agent worktrees live, relative to the repo root. Default: see `default_worktree_dir`.
    pub worktree_dir: Option<PathBuf>,
    /// Write `.claude/rules/strata.md` in each agent's worktree, telling the agent to use
    /// Strata's tools instead of its own `tsc` and dev server.
    pub agent_note: bool,
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
    /// Check every two agents' changes together for type errors neither has alone.
    pub conflicts: bool,
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
            agent_note: true,
        }
    }
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            paths: vec!["**/node_modules".into()],
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
            conflicts: true,
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
            None => normalize(&root.join(default_worktree_dir(root, &crate::strata_home()))),
        }
    }
}

/// The default `repo.worktree_dir`, relative to the repo root: `../.strata/<repo folder>`, or
/// `../.strata/worktrees/<repo folder>` when that `.strata` is Strata's own home (`home`, for a
/// repo in the home folder), so worktrees never land among its `logs/`, `shared/` and
/// `tsbridge/`.
pub fn default_worktree_dir(root: &Path, home: &Path) -> String {
    let folder = root.file_name().unwrap_or_default().to_string_lossy();
    let beside = root.parent().unwrap_or(root).join(".strata");
    let same = beside == home
        || matches!((beside.canonicalize(), home.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if same {
        format!("../.strata/worktrees/{folder}")
    } else {
        format!("../.strata/{folder}")
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
/// A TOML string literal. Not Rust's `{:?}`: that writes some characters as `\u{afb}`, which
/// TOML rejects (it wants `\u0AFB`), so a repo folder with one broke `strata init`.
fn quote(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

pub fn template(base: &str, worktree_dir: &str, dev_command: Option<&str>, app: &str) -> String {
    let (base, worktree_dir, app) = (quote(base), quote(worktree_dir), quote(app));
    let dev = match dev_command {
        Some(cmd) => format!("command = {}", quote(cmd)),
        None => "# command = \"npm run dev\"       # no dev script found in package.json".into(),
    };
    format!(
        r#"[repo]
base = {base}                     # branch Strata's own base worktree follows
worktree_dir = {worktree_dir} # where agent worktrees live
agent_note = true                 # .claude/rules/strata.md in each worktree: use Strata, not tsc

[seed]
paths = ["**/node_modules"]       # ignored folders cloned from a donor (`*` = one folder, `**` = any depth)
fallback = "install"              # after clone: install | copy | hardlink
warm_pool = 1

[checker]
typescript = "auto"               # the repo's own TypeScript; must be 7.1 or later
tsconfig = ["tsconfig.json"]
idle_dispose_secs = 900
huge_diff_ratio = 0.2             # above this share of changed files, the agent gets its own snapshot
group_memory_mb = 6144            # above this, dispose least recently used snapshots
extra_paths = []                  # per-worktree files under node_modules to layer
conflicts = true                  # warn when two agents' changes break each other's types

[dev]
{dev}
port_env = "PORT"
restart = false                   # restart dev servers after a reboot
port_range = [4100, 4999]
proxy_port = 7355
app = {app}                       # agents get <agent>.<app>.localhost

[budget]
per_agent_mb = 4096
action = "warn"                   # warn | stop
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Lines a hand-edited strata.toml might hold: known and unknown sections and keys, with
    /// values of every TOML type, or not TOML at all.
    fn toml_ish() -> impl Strategy<Value = String> {
        let section = prop::sample::select(vec![
            "[repo]",
            "[seed]",
            "[checker]",
            "[dev]",
            "[budget]",
            "[nope]",
            "[[dev]]",
            "",
        ]);
        let key = prop::sample::select(vec![
            "base",
            "worktree_dir",
            "paths",
            "fallback",
            "warm_pool",
            "typescript",
            "tsconfig",
            "huge_diff_ratio",
            "port_range",
            "proxy_port",
            "app",
            "per_agent_mb",
            "action",
            "x",
        ]);
        let value = prop_oneof![
            any::<i64>().prop_map(|n| n.to_string()),
            any::<f64>().prop_map(|f| f.to_string()),
            any::<String>().prop_map(|s| format!("{s:?}")),
            Just("[1, 2]".to_string()),
            Just("[\"a\", 3]".to_string()),
            Just("{ a = 1 }".to_string()),
            Just("true".to_string()),
            any::<String>(),
        ];
        prop::collection::vec((section, key, value), 0..12).prop_map(|lines| {
            lines
                .into_iter()
                .map(|(s, k, v)| format!("{s}\n{k} = {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    proptest! {
        #[test]
        fn any_name_becomes_a_hostname_label(name in any::<String>()) {
            prop_assert!(is_dns_label(&to_dns_label(&name)));
        }

        #[test]
        fn parsing_never_panics(text in any::<String>()) {
            let _ = Config::parse(&text);
        }

        #[test]
        fn hand_edited_files_parse_or_say_why(text in toml_ish()) {
            if let Err(e) = Config::parse(&text) {
                prop_assert!(!e.is_empty());
            }
        }

        /// Whatever the branch, folder and dev command are, `strata init` writes a file that
        /// parses back to them (the app is a hostname label, as `init` makes it).
        #[test]
        fn the_template_always_parses_back(
            base in any::<String>().prop_filter("a branch", |b| !b.trim().is_empty()),
            dir in any::<String>(),
            folder in any::<String>(),
            dev in proptest::option::of(any::<String>()),
        ) {
            let app = to_dns_label(&folder);
            let c = Config::parse(&template(&base, &dir, dev.as_deref(), &app)).unwrap();
            prop_assert_eq!(&c.repo.base, &base);
            prop_assert_eq!(c.repo.worktree_dir.as_deref(), Some(std::path::Path::new(&dir)));
            prop_assert_eq!(c.dev.app.as_deref(), Some(app.as_str()));
            prop_assert_eq!(c.dev.command.as_deref(), dev.as_deref());
        }
    }

    #[test]
    fn empty_file_gives_plan_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.repo.base, "main");
        assert!(c.repo.agent_note);
        assert!(
            !Config::parse("[repo]\nagent_note = false")
                .unwrap()
                .repo
                .agent_note
        );
        assert!(c.checker.conflicts);
        assert!(
            !Config::parse("[checker]\nconflicts = false")
                .unwrap()
                .checker
                .conflicts
        );
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
        assert!(c.repo.agent_note);
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
    fn worktrees_beside_stratas_home_go_in_a_folder_of_their_own() {
        let home = Path::new("/home/u/.strata");
        assert_eq!(
            default_worktree_dir(Path::new("/src/web"), home),
            "../.strata/web"
        );
        assert_eq!(
            default_worktree_dir(Path::new("/home/u/logs"), home),
            "../.strata/worktrees/logs"
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
