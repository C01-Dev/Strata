//! Dependency seeding (PLAN Phase 4): which of a checkout's ignored folders to seed, and
//! cloning them from a donor checkout that has the same `dep_key`.

use rayon::prelude::*;
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

/// How a folder gets from the donor into a worktree (PLAN's tiers, install aside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    Clone,
    Copy,
    Hardlink,
}

impl Way {
    pub fn name(self) -> &'static str {
        match self {
            Way::Clone => "clone",
            Way::Copy => "copy",
            Way::Hardlink => "hardlink",
        }
    }
}

/// Seeds each folder from `donor` into `target` the given way, where the donor has it as a real
/// folder (a symlink would share, not isolate) and `target` has its parent but not the folder.
/// Each one is made under a temporary name and renamed into place, so a failure leaves no
/// half-seeded folder. Returns the folders seeded.
pub fn seed_folders(
    donor: &Path,
    target: &Path,
    folders: &[PathBuf],
    way: Way,
) -> io::Result<Vec<PathBuf>> {
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
        let made = match way {
            Way::Clone => clone_tree(&src, &tmp),
            Way::Copy => walk(&src, &tmp, Op::Copy),
            Way::Hardlink => walk(&src, &tmp, Op::Hardlink),
        };
        if let Err(e) = made.and_then(|()| fs::rename(&tmp, &dst)) {
            let _ = remove(&tmp);
            return Err(e);
        }
        done.push(rel.clone());
    }
    Ok(done)
}

/// Bytes free to an unprivileged user on the volume holding `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid path and `st` is a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ by platform
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

/// Bytes in the files under `checkout`'s `folders` (apparent size, what a copy needs).
pub fn size(checkout: &Path, folders: &[PathBuf]) -> u64 {
    fn tree(p: &Path) -> u64 {
        fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => tree(&e.path()),
                Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
                _ => 0,
            })
            .sum()
    }
    folders.iter().map(|rel| tree(&checkout.join(rel))).sum()
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
pub fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
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

/// Linux: there is no tree clone, so the walker reflinks file by file (btrfs, XFS with
/// `reflink=1`, OpenZFS 2.2+). Elsewhere (ext4) the first file fails with EOPNOTSUPP or EXDEV.
#[cfg(not(target_os = "macos"))]
pub fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    walk(src, dst, Op::Reflink)
}

/// How `walk` makes each file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Shares the donor's blocks until either side writes (Linux `FICLONE`).
    Reflink,
    Copy,
    /// The same file as the donor's: a write in one shows in the other.
    Hardlink,
}

/// Recreates the tree at `src` as `dst`: folders with their modes, symlinks as symlinks (never
/// followed), files by `op` with their modes and modification times. Each folder's entries are
/// handled in parallel; cloning is metadata-bound, so threads help.
pub fn walk(src: &Path, dst: &Path, op: Op) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    fs::create_dir(dst)?;
    let entries = fs::read_dir(src)?.collect::<io::Result<Vec<_>>>()?;
    entries.par_iter().try_for_each(|e| {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let kind = e.file_type()?;
        if kind.is_dir() {
            walk(&from, &to, op)
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)
        } else {
            file(&from, &to, op)
        }
    })?;
    // Last, so a read-only folder still gets its entries.
    fs::set_permissions(dst, meta.permissions())
}

fn file(from: &Path, to: &Path, op: Op) -> io::Result<()> {
    if op == Op::Hardlink {
        return fs::hard_link(from, to);
    }
    let src = fs::File::open(from)?;
    let meta = src.metadata()?;
    let dst = fs::File::create_new(to)?;
    if op == Op::Reflink {
        reflink(&src, &dst)?;
    } else {
        io::copy(&mut &src, &mut &dst)?;
    }
    dst.set_permissions(meta.permissions())?;
    dst.set_modified(meta.modified()?)
}

#[cfg(target_os = "linux")]
fn reflink(src: &fs::File, dst: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: both descriptors are open for the duration of the call.
    if unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn reflink(_src: &fs::File, _dst: &fs::File) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
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

    #[test]
    fn the_walker_keeps_folders_links_modes_and_times() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir_all(src.join("pkg/bin")).unwrap();
        fs::write(src.join("pkg/index.js"), "x").unwrap();
        fs::write(src.join("pkg/bin/run"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(src.join("pkg/bin/run"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("../pkg/index.js", src.join("pkg/bin/link")).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(src.join("pkg/index.js"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        for op in [Op::Copy, Op::Hardlink] {
            let dst = t.path().join(format!("{op:?}"));
            walk(&src, &dst, op).unwrap();
            assert_eq!(fs::read_to_string(dst.join("pkg/index.js")).unwrap(), "x");
            let mode = fs::metadata(dst.join("pkg/bin/run"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "{op:?}");
            assert_eq!(
                fs::read_link(dst.join("pkg/bin/link")).unwrap(),
                Path::new("../pkg/index.js")
            );
            assert_eq!(
                fs::metadata(dst.join("pkg/index.js"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                old
            );
        }
        let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&fs::metadata(p).unwrap());
        assert_eq!(
            ino(&t.path().join("Hardlink/pkg/index.js")),
            ino(&src.join("pkg/index.js"))
        );
        assert_ne!(
            ino(&t.path().join("Copy/pkg/index.js")),
            ino(&src.join("pkg/index.js"))
        );
    }

    #[test]
    fn size_and_free_space() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("node_modules/a")).unwrap();
        fs::write(t.path().join("node_modules/a/x"), [0u8; 300]).unwrap();
        fs::write(t.path().join("node_modules/y"), [0u8; 20]).unwrap();
        assert_eq!(size(t.path(), &["node_modules".into()]), 320);
        assert!(free_space(t.path()).unwrap() > 0);
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
        let done = seed_folders(&donor, &target, &all, Way::Clone).unwrap();
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
            seed_folders(&donor, &target, &all, Way::Clone).unwrap(),
            Vec::<PathBuf>::new(),
            "already there"
        );
    }
}
