//! Advancement validation over whole loads, through the public surface.
//!
//! The unit tests beside `src/advancement.rs` cover the graph rules; these
//! cover what an integrator actually calls — `validate_advancements` on a
//! `LoadedData` built from synthetic packs — and the one thing only a real
//! load can show: findings carrying the winning file's provenance after
//! another pack has overridden the file they are about.

mod support;

use dust_data::{load, LoadOptions, PackSource};
use support::PackBuilder;

#[test]
fn validation_reports_the_winner_of_an_overridden_parent() {
    // Two packs define the same parent differently; only the winner's file is
    // where a finding about that parent can point.
    let base = PackBuilder::new("base")
        .resource(
            "minecraft",
            "advancement",
            "shared_root",
            r#"{"criteria":{}}"#,
        )
        .resource(
            "minecraft",
            "advancement",
            "child",
            r#"{"parent":"minecraft:shared_root","criteria":{}}"#,
        );
    let over = PackBuilder::new("over").resource(
        "minecraft",
        "advancement",
        "shared_root",
        r#"{"criteria":{}}"#,
    );

    let data = load(
        &[
            &base.build() as &dyn PackSource,
            &over.build() as &dyn PackSource,
        ],
        &LoadOptions::default(),
    );
    let (report, findings) = dust_data::validate_advancements(&data);
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(report.roots.len(), 1);
    assert_eq!(report.deepest_chain, 2);
}

#[test]
fn findings_name_the_pack_and_file_that_won() {
    let pack = PackBuilder::new("culprit")
        .resource(
            "minecraft",
            "advancement",
            "orphan",
            r#"{"parent":"minecraft:nowhere","criteria":{}}"#,
        )
        .build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let (_, findings) = dust_data::validate_advancements(&data);

    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].pack, "culprit");
    assert!(
        findings[0]
            .file
            .ends_with("data/minecraft/advancement/orphan.json"),
        "{}",
        findings[0].file
    );
    assert_eq!(
        findings[0].subject.as_ref().map(|s| s.to_string()),
        Some("minecraft:orphan".to_owned())
    );
}
