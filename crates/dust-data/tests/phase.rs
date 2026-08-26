use std::time::Instant;
use dust_data::{DirectoryPack, PackSource};
#[test]
fn phases() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dust-extract/data-1.21.1");
    let pack = DirectoryPack::builtin(&root, "vanilla", 48);
    let t = Instant::now();
    let list = pack.list().unwrap();
    println!("list {:?} for {}", t.elapsed(), list.len());
    let t = Instant::now();
    let mut total = 0usize;
    let mut blobs = Vec::with_capacity(list.len());
    for p in &list { if let Ok(Some(b)) = pack.read(p) { total += b.len(); blobs.push(b); } }
    println!("read {:?} for {} bytes", t.elapsed(), total);
    let t = Instant::now();
    let mut n = 0;
    for b in &blobs { if serde_json::from_slice::<serde_json::Value>(b).is_ok() { n += 1; } }
    println!("parse {:?} for {n}", t.elapsed());
}
