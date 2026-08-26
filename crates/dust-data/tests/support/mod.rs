//! Shared helpers for the integration tests.
//!
//! Mostly one thing: finding the extracted vanilla corpus, and being loud when
//! it is not there.

use std::io::Write as _;
use std::path::PathBuf;

/// The command that produces the corpus these tests read.
pub const REGENERATE: &str = "cargo xtask extract --version 1.21.1";

/// The extracted vanilla data tree, if it has been generated on this machine.
///
/// It lives in `.dust-extract/`, which is gitignored: **no Mojang file is
/// committed**, per the Code Provenance rule that the extractor and the
/// generated code are the repository's and Mojang's files stay on the machine
/// that downloaded them.
pub fn corpus_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.dust-extract/data-1.21.1")
        .canonicalize()
        .ok()?;
    root.join("data").is_dir().then_some(root)
}

/// Say, in a way the test harness cannot swallow, that a test did not run.
///
/// A test that quietly passes when its fixture is missing is worse than no test
/// at all: it reports a green that means nothing, and it does so most reliably
/// on the machine that has never had the fixture. `println!` will not do — the
/// harness captures it and shows it only for tests that fail, which is exactly
/// the wrong way round. Writing to the real stderr handle bypasses the capture,
/// so this line appears on a green run.
pub fn skipped(test: &str, reason: &str) {
    let _ = std::io::stderr().write_all(
        format!("\nSKIPPED {test}: {reason}\n         regenerate it with: {REGENERATE}\n\n")
            .as_bytes(),
    );
}

/// Print a measured number so it ends up in the run's output rather than only
/// in the head of whoever ran it. Same reasoning as [`skipped`].
pub fn report(lines: &[String]) {
    let mut out = String::new();
    for line in lines {
        out.push_str("         ");
        out.push_str(line);
        out.push('\n');
    }
    let _ = std::io::stderr().write_all(out.as_bytes());
}
