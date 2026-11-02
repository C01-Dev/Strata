//! Deterministic sample files and positions for the parity suite and `strata bench`.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use strata_overlay::{Tree, layer};

const SKIP_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "examples",
    "scripts",
    "playwright",
];

/// 1-based line and UTF-16 column of a byte offset.
pub fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        before.matches('\n').count() as u32 + 1,
        before[line_start..].encode_utf16().count() as u32 + 1,
    )
}

/// Up to `n` identifier positions (4+ characters) after the first 30% of the file.
pub fn identifiers(text: &str, n: usize) -> Vec<(u32, u32)> {
    let mut out = vec![];
    let mut i = text.len() * 3 / 10;
    while !text.is_char_boundary(i) {
        i += 1;
    }
    let bytes = text.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while i < bytes.len() && out.len() < n {
        if (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') && (i == 0 || !word(bytes[i - 1])) {
            let end = (i..bytes.len())
                .find(|&j| !word(bytes[j]))
                .unwrap_or(bytes.len());
            if end - i >= 4 {
                out.push(line_col(text, i));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

pub fn is_source(rel: &str) -> bool {
    (rel.ends_with(".ts") || rel.ends_with(".tsx")) && !rel.ends_with(".d.ts")
}

/// Source files the worktree changed or added relative to base.
pub fn changed_sources(base: &Path, wt: &Path) -> io::Result<Vec<String>> {
    let (mut b, mut w) = (Tree::new(base, &[]), Tree::new(wt, &[]));
    b.scan()?;
    w.scan()?;
    Ok(layer(&b, &w)
        .changed
        .into_iter()
        .filter(|f| is_source(f))
        .collect())
}

/// `n` source files of `root` spread evenly (sorted, fixed stride), inside one of `scopes`
/// (folders relative to root; "" = everywhere), not in `exclude`, and not in test-like folders.
pub fn spread(
    root: &Path,
    scopes: &[String],
    exclude: &HashSet<String>,
    n: usize,
) -> io::Result<Vec<String>> {
    let mut tree = Tree::new(root, &[]);
    tree.scan()?;
    let in_scope = |f: &str| {
        scopes
            .iter()
            .any(|s| s.is_empty() || f.starts_with(&format!("{s}/")))
    };
    let mut files: Vec<&str> = tree
        .files()
        .filter(|f| {
            is_source(f)
                && in_scope(f)
                && !exclude.contains(*f)
                && !f.split('/').any(|p| SKIP_DIRS.contains(&p))
        })
        .collect();
    files.sort();
    let step = (files.len() / n.max(1)).max(1);
    Ok(files
        .into_iter()
        .step_by(step)
        .take(n)
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_one_based_utf16() {
        let text = "const é = 1;\nconst 😀value = alphaBeta;\n";
        assert_eq!(line_col(text, text.find("value").unwrap()), (2, 9));
        assert_eq!(identifiers(text, 2), [(2, 1), (2, 9)]); // `const`, then `value` after a 2-unit emoji
    }
}
