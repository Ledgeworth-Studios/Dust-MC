//! Scratch directories the tests may write into.
//!
//! The workspace takes no `tempfile`-shaped dependency, and each test that
//! needs a directory needs one nobody else is using: tests in a binary run on
//! parallel threads, and two of them writing `world/region/r.0.0.mca` at once
//! would be a flake with no bug to fix. Process id plus an atomic counter is
//! enough uniqueness for one machine's test run; the names are distinctive so
//! anything left behind by a crashed run can be identified and deleted by
//! hand.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory under the system temporary area, created if needed.
pub(crate) fn scratch_dir(label: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "dust-harness-test-{}-{label}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("could not create the scratch directory");
    dir
}
