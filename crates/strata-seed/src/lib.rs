//! Dependency seeding (PLAN Phase 4): which of a checkout's ignored folders to seed, and
//! cloning them from a donor checkout that has the same `dep_key`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Whether `path` (relative, `/`-separated) matches `pattern`: `*` is one folder name, `**`
/// any number of folders, none included.
pub fn matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[&str], s: &[&str]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(&"**"), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(&pc), Some(&sc)) => (pc == "*" || pc == sc) && go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    go(&p, &s)
}

/// The entries of `git ls-files --others --ignored --directory` (folders end in `/`; git lists
/// only the outermost ignored folder) that match `patterns`, as relative paths.
pub fn folders(ignored: &[String], patterns: &[String]) -> Vec<PathBuf> {
    ignored
        .iter()
        .map(|p| p.strip_suffix('/').unwrap_or(p))
        .filter(|p| patterns.iter().any(|pat| matches(pat, p)))
        .map(PathBuf::from)
        .collect()
}

/// Clones each folder from `donor` into `target`, where the donor has it as a real folder (a
/// symlink would share, not isolate) and `target` has its parent but not the folder. Each one
/// is cloned under a temporary name and renamed into place, so a failure leaves no half-seeded
/// folder. Returns the folders cloned.
pub fn clone_folders(donor: &Path, target: &Path, folders: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut done = vec![];
    for rel in folders {
        let (src, dst) = (donor.join(rel), target.join(rel));
        if !src.symlink_metadata().is_ok_and(|m| m.is_dir())
            || dst.symlink_metadata().is_ok()
            || !dst.parent().is_some_and(Path::is_dir)
        {
            continue;
        }
        let name = dst.file_name().unwrap_or_default().to_string_lossy();
        let tmp = dst.with_file_name(format!(".strata-seed-{name}"));
        remove(&tmp)?; // left by a crash
        if let Err(e) = clone_tree(&src, &tmp).and_then(|()| fs::rename(&tmp, &dst)) {
            let _ = remove(&tmp);
            return Err(e);
        }
        done.push(rel.clone());
    }
    Ok(done)
}

/// Removes a file, symlink or folder; missing is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    let result = match path.symlink_metadata() {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// macOS: one `clonefile(2)` clones the whole tree (APFS; 272k entries in about 2 s).
#[cfg(target_os = "macos")]
fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let src = CString::new(src.as_os_str().as_bytes())?;
    let dst = CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    if unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_tree(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cloning is not implemented on this platform yet",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        for (pat, path, want) in [
            ("node_modules", "node_modules", true),
            ("node_modules", "apps/web/node_modules", false),
            ("apps/*/node_modules", "apps/web/node_modules", true),
            ("apps/*/node_modules", "apps/web/x/node_modules", false),
            ("**/node_modules", "node_modules", true),
            (
                "**/node_modules",
                "packages/platform/atoms/node_modules",
                true,
            ),
            ("**/node_modules", "packages/node_modules_x", false),
            ("packages/**/dist", "packages/platform/types/dist", true),
            ("packages/**/dist", "apps/dist", false),
        ] {
            assert_eq!(matches(pat, path), want, "{pat} vs {path}");
        }
    }

    #[test]
    fn folders_take_matching_ignored_entries() {
        let ignored = [
            "node_modules/",
            "apps/web/node_modules/",
            "apps/web/.next/",
            ".env",
        ]
        .map(String::from);
        assert_eq!(
            folders(&ignored, &["**/node_modules".into()]),
            [
                PathBuf::from("node_modules"),
                "apps/web/node_modules".into()
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clones_real_folders_and_skips_links_and_existing() {
        let t = tempfile::tempdir().unwrap();
        let (donor, target) = (t.path().join("donor"), t.path().join("target"));
        for d in ["node_modules/pkg", "apps/web/node_modules", "apps/api"] {
            fs::create_dir_all(donor.join(d)).unwrap();
        }
        fs::write(donor.join("node_modules/pkg/index.js"), "x").unwrap();
        std::os::unix::fs::symlink("/elsewhere", donor.join("apps/api/node_modules")).unwrap();
        for d in ["apps/web", "apps/api"] {
            fs::create_dir_all(target.join(d)).unwrap();
        }
        let all: Vec<PathBuf> = [
            "node_modules",
            "apps/web/node_modules",
            "apps/api/node_modules",
            "gone/node_modules",
        ]
        .map(PathBuf::from)
        .to_vec();
        let done = clone_folders(&donor, &target, &all).unwrap();
        assert_eq!(done, all[..2]);
        assert_eq!(
            fs::read_to_string(target.join("node_modules/pkg/index.js")).unwrap(),
            "x"
        );
        assert!(
            !target.join("apps/api/node_modules").exists(),
            "symlinked source skipped"
        );
        assert_eq!(
            clone_folders(&donor, &target, &all).unwrap(),
            Vec::<PathBuf>::new(),
            "already there"
        );
    }
}
