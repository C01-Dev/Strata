//! Times a cold scan, a warm rescan and the layer of two checkouts.
//! `cargo run --release -p strata-overlay --example scan -- <base> <worktree>`

use std::time::Instant;
use strata_overlay::{Tree, layer};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut base, mut wt) = (Tree::new(args[0].as_ref(), &[]), Tree::new(args[1].as_ref(), &[]));
    for (name, tree) in [("base", &mut base), ("worktree", &mut wt)] {
        let t = Instant::now();
        let cold = tree.scan().unwrap();
        let cold_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let warm = tree.scan().unwrap();
        println!("{name}: {} files, cold {cold_ms:.0} ms, warm rescan {:.1} ms ({} rehashed)", cold.files, t.elapsed().as_secs_f64() * 1e3, warm.rehashed);
    }
    let t = Instant::now();
    let l = layer(&base, &wt);
    println!("layer: {} changed, {} removed, {:.1} ms", l.changed.len(), l.removed.len(), t.elapsed().as_secs_f64() * 1e3);
    println!("dep_key equal: {}", base.dep_key("x").unwrap() == wt.dep_key("x").unwrap());
}
