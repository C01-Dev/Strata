//! Toy TypeScript projects for checker tests. TypeScript comes from packages/tsbridge's own
//! pinned dev dependency (`npm ci --prefix packages/tsbridge`).

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Links `<dir>/node_modules/typescript` to the bridge's pinned TypeScript.
pub fn link_typescript(dir: &Path) {
    let ts = workspace().join("packages/tsbridge/node_modules/typescript");
    assert!(ts.exists(), "run `npm ci --prefix packages/tsbridge` first");
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(ts, dir.join("node_modules/typescript")).unwrap();
}

pub fn write(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = dir.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
}

pub const TSCONFIG: &str = r#"{ "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler" }, "include": ["src"] }"#;
pub const A: &str =
    "/** Adds one. */\nexport function addOne(n: number): number { return n + 1; }\n";
pub const B: &str = "import { addOne } from \"./a\";\nexport const total = addOne(2);\n";

/// A small project in `<tmp>/<name>`, with TypeScript linked.
pub fn project(tmp: &Path, name: &str) -> PathBuf {
    let dir = tmp.join(name);
    write(
        &dir,
        &[
            ("tsconfig.json", TSCONFIG),
            ("src/a.ts", A),
            ("src/b.ts", B),
        ],
    );
    link_typescript(&dir);
    dir.canonicalize().unwrap()
}
