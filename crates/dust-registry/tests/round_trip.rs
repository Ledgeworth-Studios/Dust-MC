//! Phase 0.5's exit criterion: the generated registry compiles and round-trips
//! every vanilla block state.
//!
//! This is the test that says the extraction was right. The extractor verifies
//! its own reading of the report as it goes, but it verifies it against the
//! report — this checks the code that came out, which is a different thing and
//! the one that runs on every pull request forever.

use dust_registry::{Block, BlockState, DATA_VERSION, STATE_COUNT, STATE_SAMPLES};

#[test]
fn every_state_round_trips_through_its_properties() {
    // Decode each id to a block and a set of property values, then rebuild the
    // id from nothing but those values. 26,684 of them, and the loop takes
    // milliseconds, so there is no reason to sample.
    for id in 0..STATE_COUNT {
        let state = BlockState::from_id(id).unwrap_or_else(|| panic!("id {id} has no state"));
        assert_eq!(state.id(), id);

        let block = state.block();
        let mut rebuilt = block.default_state();
        for (property, value) in state.properties() {
            rebuilt = rebuilt.with(property, value).unwrap_or_else(|| {
                panic!(
                    "{}: {property} = {value} was rejected by the block it came from",
                    block.name()
                )
            });
        }
        assert_eq!(
            rebuilt.id(),
            id,
            "{} state {id} rebuilt to {} from {:?}",
            block.name(),
            rebuilt.id(),
            state.properties()
        );
    }
}

#[test]
fn every_state_belongs_to_the_block_that_claims_it() {
    // The lookup from id to block is a partition point over a table that is
    // only correct if the blocks tile the id space with no gap. If they do not,
    // this attributes states to whichever block happens to sit below the hole.
    let mut seen = 0u32;
    for block in Block::all() {
        for state in block.states() {
            assert_eq!(
                state.block(),
                block,
                "state {} escaped {}",
                state.id(),
                block.name()
            );
            seen += 1;
        }
    }
    assert_eq!(
        seen, STATE_COUNT,
        "the blocks do not account for every state"
    );
}

#[test]
fn no_id_beyond_the_table_decodes() {
    assert!(BlockState::from_id(STATE_COUNT).is_none());
    assert!(BlockState::from_id(u32::MAX).is_none());
}

#[test]
fn every_block_is_findable_by_name_and_names_itself() {
    for block in Block::all() {
        assert_eq!(
            Block::from_name(block.name()),
            Some(block),
            "{}",
            block.name()
        );
    }
    assert_eq!(Block::from_name("minecraft:not_a_block"), None);
    // A bare name is deliberately not accepted; see `Block::from_name`.
    assert_eq!(Block::from_name("stone"), None);
}

#[test]
fn a_default_state_belongs_to_its_own_block() {
    for block in Block::all() {
        assert_eq!(block.default_state().block(), block, "{}", block.name());
    }
}

#[test]
fn changing_a_property_stays_within_the_block() {
    for block in Block::all() {
        let state = block.default_state();
        for property in block.properties() {
            for value in property.values {
                let moved = state.with(property.name, value).unwrap_or_else(|| {
                    panic!("{}.{} rejected {value}", block.name(), property.name)
                });
                assert_eq!(moved.block(), block);
                assert_eq!(moved.property(property.name), Some(*value));
            }
            // A value the property does not take is refused, rather than
            // arithmetically producing some other block's state.
            assert_eq!(state.with(property.name, "definitely-not-a-value"), None);
        }
        assert_eq!(state.with("definitely-not-a-property", "x"), None);
    }
}

#[test]
fn the_table_agrees_with_mojang_and_not_merely_with_itself() {
    // This is the test the round-trip above cannot be. A round-trip decodes a
    // state through the table and re-encodes it through the same table, which
    // agrees with itself under *any* internally consistent property order —
    // including one where two properties are swapped and every chest faces the
    // wrong way. It proves the encoder and the decoder agree with each other.
    // Whether they agree with Minecraft is a different question and needs
    // something that did not come from the table.
    //
    // STATE_SAMPLES is that something: it is taken from Mojang's report at
    // extraction time, with property names sorted alphabetically so the rows
    // encode nothing about the radix order the table uses.
    assert!(
        !STATE_SAMPLES.is_empty(),
        "the generated table carries no samples"
    );

    for &(id, name, expected) in STATE_SAMPLES {
        let state = BlockState::from_id(id).unwrap_or_else(|| panic!("{name}: id {id} is absent"));
        assert_eq!(state.block().name(), name, "id {id}");

        let mut pairs: Vec<String> = state
            .properties()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        pairs.sort();
        assert_eq!(
            pairs.join(","),
            expected,
            "{name} state {id} decodes to something other than what Mojang's report says"
        );
    }
}

#[test]
fn every_block_with_properties_is_covered_by_the_samples() {
    // A sample that quietly stopped covering a block would leave that block's
    // property order unchecked while the suite stayed green — the failure mode
    // where a guard degrades instead of breaking.
    let mut sampled: Vec<&str> = STATE_SAMPLES.iter().map(|(_, name, _)| *name).collect();
    sampled.sort_unstable();
    sampled.dedup();

    let missing: Vec<&str> = Block::all()
        .filter(|b| !b.properties().is_empty())
        .map(Block::name)
        .filter(|name| sampled.binary_search(name).is_err())
        .collect();
    assert!(
        missing.is_empty(),
        "blocks with properties and no sample: {missing:?}"
    );
}

#[test]
fn the_table_says_which_version_it_came_from() {
    // A generated table with no version in it is a table nobody can tell is
    // stale, and D3 commits this project to more than one protocol version.
    assert_eq!(DATA_VERSION, "1.21.1");
}
