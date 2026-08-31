//! The route a light table takes from the operator's disk into the engine.
//!
//! Decision record 0008 chose this route over three others — a new `dust`
//! subcommand, the server running the oracle at boot, and a standalone jar per
//! release — and what it chose is a file in a directory the operator already
//! populates. These tests are about the *route* and not about the numbers: that
//! the file is looked for where the record says, that a table which is there is
//! used, that one which is absent is not an error, and that one which is there
//! and wrong stops the server instead of being skipped.
//!
//! Whether the numbers are Minecraft's is not checkable here and is not
//! checkable anywhere in this repository, because no Mojang value is committed
//! to it. `cargo xtask harness light` is what checks that, against a world
//! Minecraft lit itself.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use dust_registry::STATE_COUNT;
use dust_server::registries::constants;

/// A directory of its own per call, so two tests never share a file.
fn data_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "dust-constants-route-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&path).expect("create the data directory");
    path
}

/// A complete table where every state is a wall that emits nothing. The values
/// are irrelevant here; what matters is that it is the right *shape*, because
/// nothing shorter than a complete table is accepted.
fn table() -> String {
    let mut text = String::from("# state_id\topacity\temission\tocclude\tMOTION_BLOCKING\n");
    for state in 0..STATE_COUNT {
        text.push_str(&format!("{state}\t15\t0\t1\t1\n"));
    }
    text
}

#[test]
fn a_table_beside_the_data_is_read() {
    let dir = data_dir();
    std::fs::write(dir.join(constants::FILE), table()).expect("write the table");
    let loaded = constants::beside(&dir).expect("a well-formed table");
    let loaded = loaded.expect("the file is there, so there is a table");
    assert_eq!(loaded.len(), STATE_COUNT as usize);
}

#[test]
fn the_file_is_looked_for_under_the_data_path_itself_not_inside_minecraft() {
    // The layout the decision record draws: the table sits *beside*
    // `minecraft/`, not inside it. Everything under `minecraft/` is Minecraft's
    // own output in Minecraft's own layout, and a Dust file in there would look
    // like one more of them.
    let dir = data_dir();
    std::fs::create_dir_all(dir.join("minecraft")).expect("create the namespace");
    std::fs::write(dir.join("minecraft").join(constants::FILE), table()).expect("write it wrongly");
    assert!(
        constants::beside(&dir)
            .expect("no file is not an error")
            .is_none(),
        "a table inside minecraft/ is not the route"
    );
}

#[test]
fn no_table_is_not_an_error() {
    // A light table is not something an operator can be expected to have
    // before they have read about one, and a server that refused to start
    // without it would be a server that refuses to start.
    let dir = data_dir();
    assert!(constants::beside(&dir).expect("absence is fine").is_none());
}

#[test]
fn a_table_that_is_there_and_wrong_is_refused_rather_than_skipped() {
    // The alternative is a server that starts and lights worse than the
    // operator asked it to, silently, having read their file and put it down.
    let dir = data_dir();
    std::fs::write(
        dir.join(constants::FILE),
        "# state_id\topacity\temission\n0\t0\t0\n",
    )
    .expect("write a truncated table");
    let error = constants::beside(&dir).expect_err("a table with one row in it");
    let message = error.to_string();
    assert!(
        message.contains(constants::FILE),
        "the message has to name the file: {message}"
    );
    assert!(
        message.contains("Minecraft version") || message.contains("truncated"),
        "and say what is actually likely to be wrong with it: {message}"
    );
}

#[test]
fn the_name_says_who_wrote_it() {
    // Pinned because it is an interface with an operator: the name appears in
    // dust.toml.example, in the extractor's own output, and in whatever the
    // operator typed the day they set this up. Renaming it silently breaks a
    // working server on upgrade.
    assert_eq!(constants::FILE, "dust-constants.tsv");
}
