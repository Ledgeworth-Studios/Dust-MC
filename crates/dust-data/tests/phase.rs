//! A rough timing probe over the extracted vanilla tree.
//!
//! Not an assertion about correctness — the corpus tests next door own that —
//! but the numbers it prints are how a change to the loader gets judged as fast
//! or slow against five and a half thousand real files rather than against
//! synthetic fixtures that fit in cache.

mod support;

use std::time::Instant;

use dust_data::{DirectoryPack, PackSource};

#[test]
fn phases() {
    let Some(root) = support::corpus_root() else {
        support::skipped(
            "phases",
            "the extracted 1.21.1 data tree is not on this machine",
        );
        return;
    };
    let pack = DirectoryPack::builtin(&root, "vanilla", 48);
    let t = Instant::now();
    let list = pack.list().unwrap();
    let list_time = t.elapsed();
    let t = Instant::now();
    let mut total = 0usize;
    let mut blobs = Vec::with_capacity(list.len());
    for p in &list {
        if let Ok(Some(b)) = pack.read(p) {
            total += b.len();
            blobs.push(b);
        }
    }
    let read_time = t.elapsed();
    let t = Instant::now();
    let mut n = 0;
    for b in &blobs {
        if serde_json::from_slice::<serde_json::Value>(b).is_ok() {
            n += 1;
        }
    }
    let parse_time = t.elapsed();
    // Through `report`, not `println!`: the harness captures stdout, so the
    // numbers would otherwise be visible only on a failing run, and a timing
    // probe exists to be read on a passing one.
    support::report(&[
        format!("list   {list_time:?} for {} files", list.len()),
        format!("read   {read_time:?} for {total} bytes"),
        format!("parse  {parse_time:?} for {n}"),
    ]);
}
