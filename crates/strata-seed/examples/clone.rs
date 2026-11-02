//! Times one way of seeding a tree: `cargo run --release -p strata-seed --example clone --
//! <src> <dst> <clone|copy|hardlink>`. `clone` is `clonefile` on macOS and the reflink walker on
//! Linux. `dst` must not exist.

use std::time::Instant;
use strata_seed::{Op, clone_tree, walk};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (src, dst) = (args[0].as_ref(), args[1].as_ref());
    let t = Instant::now();
    match args[2].as_str() {
        "clone" => clone_tree(src, dst),
        "copy" => walk(src, dst, Op::Copy),
        "hardlink" => walk(src, dst, Op::Hardlink),
        other => panic!("unknown way {other:?}"),
    }
    .unwrap();
    println!("{}: {:.2} s", args[2], t.elapsed().as_secs_f64());
}
