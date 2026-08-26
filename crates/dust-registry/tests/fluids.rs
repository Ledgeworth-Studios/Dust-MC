//! Fluid relationships: the joins, checked from both ends.
//!
//! A relationship table has no round-trip either. What it has is three other
//! tables it must agree with — a fluid's block has to exist in the block table,
//! its bucket in the item registry — and [`FLUID_SAMPLES`], the same three
//! reports copied as plain text by an extraction pass that shares no reading
//! with the join. The test reads each sample row and asks the compiled table
//! the same question; if the two passes disagree about where water goes, one
//! of them is wrong and this file says which.

use dust_registry::generated::fluids::{FLUID_DEFS, FLUID_SAMPLES};
use dust_registry::{Block, Fluid, Item, DATA_VERSION};

#[test]
fn every_fluid_has_a_row_and_every_row_lands_in_the_table() {
    assert_eq!(
        FLUID_DEFS.len(),
        Fluid::all().count(),
        "the relationship rows and the fluid registry disagree about how many fluids exist"
    );
    for fluid in Fluid::all() {
        let def = fluid.def();
        // Both sides resolve through their own registries, so a typo in the
        // generated text cannot hide behind a valid-looking Option.
        assert_eq!(
            def.block.and_then(Block::from_name).map(|b| b.name()),
            def.block,
            "{}: the block named here is not a block",
            fluid.name()
        );
        assert_eq!(
            def.bucket.and_then(Item::from_name).map(|i| i.name()),
            def.bucket,
            "{}: the bucket named here is not an item",
            fluid.name()
        );
    }
}

#[test]
fn the_table_says_what_the_copied_rows_say() {
    // The external check. Each row was written from the reports before the
    // join ran; these assertions re-derive the same answers from the table.
    assert_eq!(FLUID_SAMPLES.len(), FLUID_DEFS.len());
    for &(name, block, bucket, flowing_of) in FLUID_SAMPLES {
        let fluid = Fluid::from_name(name).unwrap_or_else(|| panic!("{name} sampled and absent"));
        let def = fluid.def();
        assert_eq!(def.block.unwrap_or_default(), block, "{name}: block");
        assert_eq!(def.bucket.unwrap_or_default(), bucket, "{name}: bucket");
        assert_eq!(
            def.flowing_of.unwrap_or_default(),
            flowing_of,
            "{name}: still partner"
        );
    }
}

#[test]
fn only_still_fluids_have_buckets() {
    // Nobody picks up flowing water. If the join ever inverted and paired by
    // item-name suffix without regard to stillness, this catches it, because
    // the flowing half would grow buckets the item registry does not contain.
    for fluid in Fluid::all() {
        let has_bucket = fluid.def().bucket.is_some();
        assert_eq!(
            has_bucket,
            fluid.flowing_of().is_none()
                && fluid != Fluid::from_name("minecraft:empty").expect("empty"),
            "{}",
            fluid.name()
        );
    }
}

#[test]
fn the_named_facts_about_water_and_lava_are_still_true() {
    // Written down rather than derived: if the extractor silently started
    // reading a different field, everything above could agree with itself all
    // the way to a wrong answer.
    let water = Fluid::from_name("minecraft:water").expect("water");
    assert_eq!(water.block().map(Block::name), Some("minecraft:water"));
    assert_eq!(
        water.bucket().map(Item::name),
        Some("minecraft:water_bucket")
    );

    let lava = Fluid::from_name("minecraft:lava").expect("lava");
    assert_eq!(lava.block().map(Block::name), Some("minecraft:lava"));
    assert_eq!(lava.bucket().map(Item::name), Some("minecraft:lava_bucket"));

    let empty = Fluid::from_name("minecraft:empty").expect("empty");
    assert_eq!(empty.block(), None);
    assert_eq!(empty.bucket(), None);
    assert_eq!(
        empty,
        Fluid::registry()
            .default_entry()
            .and_then(Fluid::from_name)
            .expect("the default"),
        "the registry's default is the empty fluid"
    );
}

#[test]
fn every_flowing_fluid_names_a_still_one_that_exists() {
    let count = Fluid::all().filter(|f| f.flowing_of().is_some()).count();
    assert_eq!(count, 2, "water and lava are the paired pair on 1.21.1");
    for fluid in Fluid::all() {
        let Some(still) = fluid.flowing_of() else {
            continue;
        };
        assert_ne!(still, fluid);
        assert_eq!(
            still.def().flowing_of,
            None,
            "{} flows from {} which itself flows",
            fluid.name(),
            still.name()
        );
    }
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(dust_registry::generated::fluids::DATA_VERSION, DATA_VERSION);
}
