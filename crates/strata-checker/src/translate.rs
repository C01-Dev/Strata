//! Two-way path translation between an agent's worktree and the base checkout whose paths the
//! shared snapshot uses. Each root is matched in its given and its real (symlink-resolved) form,
//! and only at path-component boundaries, so `/x/.base2` is never mistaken for `/x/.base`.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Translator {
    base: PathBuf,
    wt: PathBuf,
    /// Spellings of each root as they may appear in tsgo's output, longest first.
    base_forms: Vec<String>,
    wt_forms: Vec<String>,
}

fn forms(root: &Path) -> Vec<String> {
    let mut out = vec![root.to_string_lossy().into_owned()];
    if let Ok(real) = root.canonicalize() {
        out.push(real.to_string_lossy().into_owned());
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out.dedup();
    out
}

/// A character that can continue a path component, so a match ending or starting next to it
/// is part of a longer name.
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | '+' | '~')
}

/// Replaces `from` with `to` wherever `from` is a whole root: preceded by start or a non-name
/// character, and followed by `/`, end, or a non-name character.
fn replace_root(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(from) {
        let before = rest[..i]
            .chars()
            .next_back()
            .or_else(|| out.chars().next_back());
        let before_ok = before.is_none_or(|c| !name_char(c));
        let after = &rest[i + from.len()..];
        let after_ok = after
            .chars()
            .next()
            .is_none_or(|c| c == '/' || !name_char(c));
        out.push_str(&rest[..i]);
        out.push_str(if before_ok && after_ok { to } else { from });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn strip_root<'a>(path: &'a str, forms: &[String]) -> Option<&'a str> {
    forms
        .iter()
        .find_map(|root| match path.strip_prefix(root.as_str()) {
            Some("") => Some(""),
            Some(rest) if rest.starts_with('/') => Some(rest),
            _ => None,
        })
}

impl Translator {
    pub fn new(base: &Path, wt: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            wt: wt.to_path_buf(),
            base_forms: forms(base),
            wt_forms: forms(wt),
        }
    }

    /// For snapshots at the worktree's own paths (unshared): only relativizes.
    pub fn identity(root: &Path) -> Self {
        Self::new(root, root)
    }

    pub fn worktree(&self) -> &Path {
        &self.wt
    }

    /// A worktree path (absolute, or relative to the worktree) as the base path the snapshot uses.
    pub fn to_base(&self, path: &Path) -> PathBuf {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.wt.join(path)
        };
        match strip_root(&abs.to_string_lossy(), &self.wt_forms) {
            Some(rest) => PathBuf::from(format!("{}{rest}", self.base.display())),
            None => abs,
        }
    }

    /// A path from the snapshot as the agent's own absolute path; paths outside base unchanged.
    pub fn to_wt(&self, path: &str) -> String {
        match strip_root(path, &self.base_forms) {
            Some(rest) => format!("{}{rest}", self.wt.display()),
            None => path.to_string(),
        }
    }

    /// As `to_wt`, then relative to the worktree when inside it (MCP's convention).
    pub fn to_agent(&self, path: &str) -> String {
        let wt = self.to_wt(path);
        match strip_root(&wt, &self.wt_forms) {
            Some(rest) => rest.trim_start_matches('/').to_string(),
            None => wt,
        }
    }

    /// Base roots inside free text (diagnostic messages, hover text) become the worktree root.
    pub fn text_to_wt(&self, text: &str) -> String {
        let to = self.wt.to_string_lossy();
        self.base_forms
            .iter()
            .fold(text.to_string(), |t, from| replace_root(&t, from, &to))
    }

    /// Rewrites a bridge result in place: every `file` field via `to_agent`, every `message`
    /// and `text` field via `text_to_wt`.
    pub fn response(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    match (key.as_str(), v) {
                        ("file", Value::String(s)) => *s = self.to_agent(s),
                        ("message" | "text", Value::String(s)) => *s = self.text_to_wt(s),
                        (_, v) => self.response(v),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.response(v)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    const BASE: &str = "/r/.strata/app/.base";
    const WT: &str = "/r/.strata/app/fix-login";

    fn t() -> Translator {
        Translator::new(Path::new(BASE), Path::new(WT))
    }

    fn rel_path() -> impl Strategy<Value = String> {
        prop::collection::vec("[a-zA-Z0-9_.@-]{1,12}", 1..6).prop_map(|parts| parts.join("/"))
    }

    /// Path segments with anything a file name may hold: spaces, Unicode, dots inside names.
    fn wild_rel_path() -> impl Strategy<Value = String> {
        prop::collection::vec("[^/\\x00]{1,12}", 1..5)
            .prop_filter("no . or .. segments", |parts| {
                parts.iter().all(|p| p != "." && p != "..")
            })
            .prop_map(|parts| parts.join("/"))
    }

    /// Any JSON shape tsgo could answer with.
    fn json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<String>().prop_map(Value::from),
            wild_rel_path().prop_map(|r| Value::from(format!("{BASE}/{r}"))),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::from),
                prop::collection::hash_map(
                    prop_oneof![
                        Just("file".to_string()),
                        Just("message".to_string()),
                        Just("text".to_string()),
                        "[a-zA-Z]{1,8}"
                    ],
                    inner,
                    0..6
                )
                .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn wild_paths_round_trip(rel in wild_rel_path()) {
            let base = format!("{BASE}/{rel}");
            prop_assert_eq!(t().to_base(Path::new(&format!("{WT}/{rel}"))), PathBuf::from(&base));
            prop_assert_eq!(t().to_agent(&base), rel.clone());
        }

        /// Arbitrary text never panics, and text that does not contain the base root comes
        /// back unchanged.
        #[test]
        fn any_text_is_safe(text in any::<String>()) {
            let out = t().text_to_wt(&text);
            if !text.contains(BASE) {
                prop_assert_eq!(&out, &text);
            }
            let _ = (t().to_wt(&text), t().to_agent(&text));
        }

        /// No answer shape panics, no `file` field still names the base, and no `message` or
        /// `text` still mentions it (other fields carry no paths and are left alone).
        #[test]
        fn any_answer_is_translated(mut v in json()) {
            t().response(&mut v);
            fn walk(v: &Value) -> Result<(), String> {
                match v {
                    Value::Object(m) => m.iter().try_for_each(|(k, v)| match (k.as_str(), v) {
                        ("file", Value::String(s)) if s.starts_with(&format!("{BASE}/")) => Err(format!("file {s}")),
                        ("message" | "text", Value::String(s)) if s.contains(&format!("{BASE}/")) => Err(format!("{k} {s}")),
                        _ => walk(v),
                    }),
                    Value::Array(a) => a.iter().try_for_each(walk),
                    _ => Ok(()),
                }
            }
            prop_assert_eq!(walk(&v), Ok(()));
        }

        #[test]
        fn round_trips(rel in rel_path()) {
            let base = format!("{BASE}/{rel}");
            let wt = format!("{WT}/{rel}");
            prop_assert_eq!(t().to_base(Path::new(&wt)), PathBuf::from(&base));
            prop_assert_eq!(t().to_base(Path::new(&rel)), PathBuf::from(&base));
            prop_assert_eq!(t().to_wt(&base), wt.clone());
            prop_assert_eq!(t().to_agent(&base), rel.clone());
            prop_assert_eq!(t().to_base(Path::new(&t().to_wt(&base))), PathBuf::from(&base));
        }

        #[test]
        fn outside_and_lookalike_paths_are_untouched(rel in rel_path(), suffix in "[a-z0-9_-]{1,4}") {
            for other in [format!("/usr/lib/{rel}"), format!("{BASE}{suffix}/{rel}"), format!("/x{BASE}/{rel}")] {
                prop_assert_eq!(t().to_wt(&other), other.clone());
                prop_assert_eq!(t().to_agent(&other), other.clone());
                prop_assert_eq!(t().text_to_wt(&format!("see {other}")), format!("see {other}"));
            }
        }

        #[test]
        fn text_replaces_every_whole_root(rel in rel_path(), a in "[ a-z:'\"(]{0,8}", b in "[ a-z:'\")]{0,8}") {
            // A root is only replaced after a separator: `a/r/...` is some other relative path.
            let text = format!("{a} {BASE}/{rel}{b} and '{BASE}'");
            prop_assert_eq!(t().text_to_wt(&text), format!("{a} {WT}/{rel}{b} and '{WT}'"));
            let glued = format!("x{BASE}/{rel}");
            prop_assert_eq!(t().text_to_wt(&glued), glued.clone());
        }
    }

    #[test]
    fn real_path_spelling_of_the_base_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(real.join("base")).unwrap();
        std::os::unix::fs::symlink(real.join("base"), real.join("link")).unwrap();
        let tr = Translator::new(&real.join("link"), Path::new(WT));
        let reported = format!("{}/src/a.ts", real.join("base").display());
        assert_eq!(tr.to_wt(&reported), format!("{WT}/src/a.ts"));
    }

    #[test]
    fn identity_only_relativizes() {
        let tr = Translator::identity(Path::new(WT));
        assert_eq!(
            tr.to_base(Path::new("src/a.ts")),
            PathBuf::from(format!("{WT}/src/a.ts"))
        );
        assert_eq!(tr.to_agent(&format!("{WT}/src/a.ts")), "src/a.ts");
    }

    #[test]
    fn rewrites_bridge_results() {
        let mut v = json!({ "items": [
            { "file": format!("{BASE}/src/a.ts"), "line": 1, "message": format!("Cannot find module '{BASE}/src/x'.") },
            { "file": "/usr/lib/node/lib.d.ts", "message": "plain" },
            { "file": "", "message": "global" },
        ], "total": 3 });
        t().response(&mut v);
        assert_eq!(v["items"][0]["file"], "src/a.ts");
        assert_eq!(
            v["items"][0]["message"],
            format!("Cannot find module '{WT}/src/x'.")
        );
        assert_eq!(v["items"][1]["file"], "/usr/lib/node/lib.d.ts");
        assert_eq!(v["items"][2]["file"], "");
    }
}
