//! P1-BENCH-02's pairing driver: duscope's own walk of a tree, timed in
//! alternating pairs with the clone attributes off and on, so the drift of
//! a busy machine cancels within each pair. It prints the `walker.clones`
//! object of `bench/results/clone-attributes-macos.json`.
//!
//! ```sh
//! cargo run --release -p duscope-walk --example clone_pairs -- bench/.trees/T1 40
//! ```

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use duscope_walk::{WalkOptions, walk};

/// Unmeasured pairs first, so the tree is warm in the cache.
const WARMUP: usize = 3;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(tree) = args.next() else {
        eprintln!("usage: clone_pairs TREE [PAIRS]");
        std::process::exit(2);
    };
    let pairs: usize = args.next().map_or(40, |n| n.parse().unwrap_or(40));
    let time = |clones: bool| {
        let opts = WalkOptions {
            clones,
            ..WalkOptions::default()
        };
        let start = Instant::now();
        if let Err(e) = walk(Path::new(&tree), &opts, |_| {}) {
            eprintln!("clone_pairs: {e}");
            std::process::exit(1);
        }
        start.elapsed().as_secs_f64()
    };
    let mut runs = Vec::with_capacity(pairs);
    for i in 0..WARMUP + pairs {
        // Alternate within each pair and swap the order every other pair.
        let (off, on) = if i % 2 == 0 {
            let off = time(false);
            (off, time(true))
        } else {
            let on = time(true);
            (time(false), on)
        };
        if i >= WARMUP {
            runs.push((off, on));
        }
    }
    // The value at `percent` of the way through the sorted values.
    let quantile = |mut values: Vec<f64>, percent: usize| {
        values.sort_by(f64::total_cmp);
        values[((values.len() - 1) * percent + 50) / 100]
    };
    let ratios: Vec<f64> = runs.iter().map(|&(off, on)| on / off).collect();
    let list = runs
        .iter()
        .map(|(off, on)| format!("[{off:.4}, {on:.4}]"))
        .collect::<Vec<_>>()
        .join(", ");
    // The result goes to stdout through a writer: the workspace keeps
    // println! to the binary's output module.
    let written = writeln!(
        std::io::stdout().lock(),
        "{{\"compares\": \"--no-clones (off) against --clones (on)\", \"pairs\": {pairs}, \
         \"order\": \"alternating within each pair, swapped every other pair, after {WARMUP} \
         unmeasured pairs\", \"off_median_s\": {:.4}, \"on_median_s\": {:.4}, \
         \"ratio_median\": {:.4}, \"ratio_p25\": {:.4}, \"ratio_p75\": {:.4}, \"runs_s\": [{list}]}}",
        quantile(runs.iter().map(|r| r.0).collect(), 50),
        quantile(runs.iter().map(|r| r.1).collect(), 50),
        quantile(ratios.clone(), 50),
        quantile(ratios.clone(), 25),
        quantile(ratios, 75),
    );
    if written.is_err() {
        std::process::exit(1);
    }
}
