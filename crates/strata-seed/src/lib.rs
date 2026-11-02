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

/// What `fix_up` changed or found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FixUps {
    /// State files and `.bin` shims whose donor paths now point at the worktree.
    pub rewritten: Vec<PathBuf>,
    /// Native addons built by node-gyp whose build files name the donor's folder.
    pub rebuild: Vec<String>,
}

/// After seeding `folders` from `donor` into `target` (PLAN 4.4): points known state files
/// that hold absolute paths at the worktree (pnpm's `.pnpm-workspace-state-v1.json` and
/// `.modules.yaml`, old pnpm's `.bin` shims), resets pnpm's `lastValidatedTimestamp` to now, and
/// lists node-gyp addons whose build files name the donor. A rewritten file is a new file
/// renamed over the old one, so a hardlinked donor's copy is never touched.
pub fn fix_up(donor: &Path, target: &Path, folders: &[PathBuf]) -> io::Result<FixUps> {
    let (from, to) = (donor.to_string_lossy(), target.to_string_lossy());
    let mut out = FixUps::default();
    for rel in folders {
        let dir = target.join(rel);
        let mut files = vec![
            dir.join(".pnpm-workspace-state-v1.json"),
            dir.join(".modules.yaml"),
        ];
        files.extend(
            fs::read_dir(dir.join(".bin"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path()),
        );
        for f in files {
            if rewrite(&f, &from, &to)? {
                out.rewritten.push(f);
            }
        }
        out.rebuild.extend(addons(&dir, &from));
    }
    out.rebuild.sort();
    out.rebuild.dedup();
    Ok(out)
}

fn rewrite(file: &Path, from: &str, to: &str) -> io::Result<bool> {
    let Ok(meta) = fs::symlink_metadata(file) else {
        return Ok(false);
    };
    let Some(text) = meta
        .is_file()
        .then(|| fs::read(file).ok().and_then(|b| String::from_utf8(b).ok()))
        .flatten()
    else {
        return Ok(false);
    };
    let mut new = text.replace(from, to);
    if file.ends_with(".pnpm-workspace-state-v1.json") {
        new = reset_timestamp(&new);
    }
    if new == text {
        return Ok(false);
    }
    let tmp = file.with_extension("strata-tmp");
    fs::write(&tmp, new)?;
    fs::set_permissions(&tmp, meta.permissions())?;
    fs::rename(&tmp, file)?;
    Ok(true)
}

/// pnpm skips its "are dependencies up to date" check while manifests are older than this.
fn reset_timestamp(json: &str) -> String {
    let key = "\"lastValidatedTimestamp\":";
    let Some(at) = json.find(key) else {
        return json.to_string();
    };
    let start = at + key.len();
    let rest = &json[start..];
    let digits = rest.trim_start();
    let pad = rest.len() - digits.len();
    let len = digits.bytes().take_while(u8::is_ascii_digit).count();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{}{now}{}", &json[..start + pad], &digits[len..])
}

/// Packages in a `node_modules` folder (and pnpm's `.pnpm/*/node_modules`) with a node-gyp
/// `build/config.gypi` whose `build/` files name the donor.
fn addons(node_modules: &Path, donor: &str) -> Vec<String> {
    fn packages(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('@') {
                for s in fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    let sub = s.file_name().to_string_lossy().into_owned();
                    out.push((format!("{name}/{sub}"), s.path()));
                }
            } else if !name.starts_with('.') {
                out.push((name, e.path()));
            }
        }
    }
    let mut pkgs = vec![];
    packages(node_modules, &mut pkgs);
    for e in fs::read_dir(node_modules.join(".pnpm"))
        .into_iter()
        .flatten()
        .flatten()
    {
        packages(&e.path().join("node_modules"), &mut pkgs);
    }
    pkgs.into_iter()
        .filter(|(_, p)| p.join("build/config.gypi").is_file())
        .filter(|(_, p)| {
            fs::read_dir(p.join("build"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|f| {
                    let n = f.file_name().to_string_lossy().into_owned();
                    (n == "config.gypi" || n == "Makefile" || n.ends_with(".mk"))
                        && fs::read_to_string(f.path()).is_ok_and(|t| t.contains(donor))
                })
        })
        .map(|(name, _)| name)
        .collect()
}

/// Whether the checkout's `node_modules` keeps its packages in a store outside the checkout
/// (pnpm's global virtual store, PLAN 4.6): its packages are links into a store every
/// checkout shares, so a clone would not isolate anything (and pnpm's links are relative, so
/// a clone at another folder depth would break them). Read from `.modules.yaml`, which pnpm
/// writes as YAML or JSON.
pub fn shared_store(checkout: &Path) -> bool {
    let nm = checkout.join("node_modules");
    let Ok(text) = fs::read_to_string(nm.join(".modules.yaml")) else {
        return false;
    };
    let dir = text.lines().find_map(|l| {
        let rest = l
            .trim()
            .trim_start_matches('"')
            .strip_prefix("virtualStoreDir")?;
        let value = rest
            .trim_start_matches('"')
            .trim_start()
            .strip_prefix(':')?;
        Some(
            value
                .trim()
                .trim_end_matches(',')
                .trim_matches(|c| c == '"' || c == '\''),
        )
    });
    let (Some(dir), Ok(root)) = (dir, checkout.canonicalize()) else {
        return false;
    };
    nm.join(dir)
        .canonicalize()
        .is_ok_and(|store| !store.starts_with(root))
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
    fn fix_up_points_state_at_the_worktree_and_finds_stale_addons() {
        let t = tempfile::tempdir().unwrap();
        let (donor, target) = (t.path().join("donor"), t.path().join("target"));
        let nm = target.join("node_modules");
        for d in [
            ".bin",
            "napi/build",
            "@s/pure",
            ".pnpm/x@1/node_modules/@s/gyp/build",
        ] {
            fs::create_dir_all(nm.join(d)).unwrap();
        }
        let d = donor.display();
        let state = format!(
            "{{\n  \"lastValidatedTimestamp\": 1000,\n  \"projects\": {{ \"{d}\": {{}}, \"{d}/packages/a\": {{}} }}\n}}"
        );
        fs::write(nm.join(".pnpm-workspace-state-v1.json"), &state).unwrap();
        fs::write(
            nm.join(".bin/tool"),
            format!("NODE_PATH={d}/node_modules/.pnpm\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink("../tool/bin.js", nm.join(".bin/link")).unwrap();
        fs::write(nm.join("napi/build/config.gypi"), "{}").unwrap();
        fs::write(
            nm.join("napi/build/napi.target.mk"),
            format!("INCS := -I{d}/node_modules/nan\n"),
        )
        .unwrap();
        fs::write(
            nm.join(".pnpm/x@1/node_modules/@s/gyp/build/config.gypi"),
            format!("\"{d}\""),
        )
        .unwrap();
        // A hardlinked donor file must not change.
        fs::create_dir_all(&donor).unwrap();
        fs::hard_link(nm.join(".bin/tool"), donor.join("tool")).unwrap();

        let fixed = fix_up(&donor, &target, &["node_modules".into()]).unwrap();
        assert_eq!(fixed.rebuild, ["@s/gyp", "napi"]);
        assert_eq!(fixed.rewritten.len(), 2, "{fixed:?}");
        let state = fs::read_to_string(nm.join(".pnpm-workspace-state-v1.json")).unwrap();
        let t = target.display();
        assert!(
            state.contains(&format!("\"{t}\": {{}}, \"{t}/packages/a\"")),
            "{state}"
        );
        assert!(!state.contains("1000,"), "timestamp reset: {state}");
        assert!(
            fs::read_to_string(nm.join(".bin/tool"))
                .unwrap()
                .contains(&t.to_string())
        );
        assert!(
            fs::read_to_string(donor.join("tool"))
                .unwrap()
                .contains(&d.to_string())
        );
        assert_eq!(
            fix_up(&donor, &target, &["node_modules".into()])
                .unwrap()
                .rewritten
                .len(),
            1,
            "only the timestamp, again"
        );
    }

    #[test]
    fn a_virtual_store_outside_the_checkout_is_shared() {
        let t = tempfile::tempdir().unwrap();
        let store = t.path().join("store/links");
        fs::create_dir_all(&store).unwrap();
        for (name, yaml, shared) in [
            (
                "json-global",
                "{\n  \"virtualStoreDir\": \"../../store/links\",\n}",
                true,
            ),
            ("yaml-local", "virtualStoreDir: .pnpm\n", false),
            ("npm", "", false),
        ] {
            let nm = t.path().join(name).join("node_modules");
            fs::create_dir_all(nm.join(".pnpm")).unwrap();
            if !yaml.is_empty() {
                fs::write(nm.join(".modules.yaml"), yaml).unwrap();
            }
            assert_eq!(shared_store(&t.path().join(name)), shared, "{name}");
        }
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
