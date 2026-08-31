//! The reader for the light oracle's table.
//!
//! These tests are self-consistency checks over a file format and they should
//! be read as ones: they say the parser refuses what it claims to refuse, and
//! they say nothing at all about whether the *numbers* Minecraft gives a block
//! are what this table holds. Nothing in a test suite can say that, because
//! nothing in this repository holds Mojang's numbers to compare against —
//! which is decision record 0008 and not an omission here. What checks the
//! values is `cargo xtask harness light`, against a world Minecraft lit itself.
//!
//! What they *can* check, and the reason the parser is strict, is the failure
//! that produces no error of its own: a table extracted from a different
//! version of Minecraft, where every row is well-formed and every state id
//! means a different block than it does in this build.

use dust_registry::constants::{BlockConstants, ConstantsError};
use dust_registry::{Registry, STATE_COUNT};

/// A complete table where state `n` has opacity `n % 16`, emits `(n * 7) % 16`
/// and occludes on even ids. Arbitrary, and the point is that it is *complete*:
/// the parser refuses anything else, so every test that is not about
/// incompleteness has to start from a whole one.
fn full() -> String {
    let mut text =
        String::from("# state_id\topacity\temission\tocclude\tMOTION_BLOCKING\tWORLD_SURFACE\n");
    for state in 0..STATE_COUNT {
        let opacity = state % 16;
        let emission = (state * 7) % 16;
        let occlude = u32::from(state % 2 == 0);
        let motion = u32::from(state % 3 != 1);
        let surface = u32::from(state != 0);
        text.push_str(&format!(
            "{state}\t{opacity}\t{emission}\t{occlude}\t{motion}\t{surface}\n"
        ));
    }
    text
}

/// The row [`full`] writes for state 0, so a test that corrupts "the first
/// row" corrupts the row it thinks it does. Written out rather than formatted
/// because a mistake in it is exactly the mistake it exists to prevent: a
/// pattern that matched some *other* row still produced a passing test, and the
/// line number in the failure message was the only thing that said so.
const ROW_ZERO: &str = "0\t0\t0\t1\t1\t0\n";

#[test]
fn the_first_row_of_the_fixture_is_the_one_the_tests_below_corrupt() {
    assert!(full().contains(ROW_ZERO));
    assert_eq!(full().lines().nth(1), Some(ROW_ZERO.trim_end()));
}

#[test]
fn a_complete_table_reads_back_every_state() {
    let table = BlockConstants::parse(&full()).expect("a complete table");
    assert_eq!(table.len(), STATE_COUNT as usize);
    assert!(!table.is_empty());
    for state in [0, 1, 15, 16, 1234, STATE_COUNT - 1] {
        assert_eq!(table.opacity(state), (state % 16) as u8, "opacity {state}");
        assert_eq!(
            table.emission(state),
            ((state * 7) % 16) as u8,
            "emission {state}"
        );
        assert_eq!(table.occludes(state), state % 2 == 0, "occlude {state}");
    }
}

#[test]
fn a_state_the_table_does_not_reach_is_a_wall_that_emits_nothing() {
    // The direction matters and is argued at `BlockConstants::opacity`: an unknown
    // id under-lights, which is where every other known gap already errs.
    let table = BlockConstants::parse(&full()).expect("a complete table");
    assert_eq!(table.opacity(STATE_COUNT), 15);
    assert_eq!(table.emission(STATE_COUNT), 0);
    assert!(table.occludes(STATE_COUNT));
}

#[test]
fn emitting_counts_the_states_that_give_off_anything() {
    let table = BlockConstants::parse(&full()).expect("a complete table");
    let expected = (0..STATE_COUNT).filter(|s| (s * 7) % 16 > 0).count();
    assert_eq!(table.emitting(), expected);
}

#[test]
fn comments_and_blank_lines_are_not_rows() {
    let mut text = full();
    text.push_str("\n# a trailing note\n\n");
    let table = BlockConstants::parse(&text).expect("a complete table with noise in it");
    assert_eq!(table.len(), STATE_COUNT as usize);
}

#[test]
fn a_table_written_before_the_occlude_column_reads_as_all_occluding() {
    let mut text = String::from("# state_id\topacity\temission\n");
    for state in 0..STATE_COUNT {
        text.push_str(&format!("{state}\t0\t0\n"));
    }
    let table = BlockConstants::parse(&text).expect("three columns is a table too");
    assert!(table.occludes(0));
    assert!(table.occludes(STATE_COUNT - 1));
    assert_eq!(table.flags().count(), 0);
}

#[test]
fn a_missing_state_is_refused_rather_than_defaulted() {
    // The version-skew case seen from the low end. A defaulted state is a
    // block that lights wrongly and says nothing.
    let text = full()
        .lines()
        .filter(|l| !l.starts_with("1234\t"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        BlockConstants::parse(&text),
        Err(ConstantsError::Incomplete {
            present: STATE_COUNT as usize - 1,
            expected: STATE_COUNT as usize,
        })
    );
}

#[test]
fn a_state_this_build_has_no_block_for_names_the_version_skew() {
    let mut text = full();
    text.push_str(&format!("{}\t0\t0\t0\t0\t0\n", STATE_COUNT));
    let error = BlockConstants::parse(&text).expect_err("a state beyond the table");
    assert!(
        matches!(
            error,
            ConstantsError::UnknownState { state, states, .. }
                if state == STATE_COUNT && states == STATE_COUNT
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("different Minecraft version"),
        "the message has to say what it actually means: {error}"
    );
}

#[test]
fn one_state_described_twice_is_two_answers_to_one_question() {
    let mut text = full();
    text.push_str("7\t0\t0\t0\t0\t0\n");
    assert!(matches!(
        BlockConstants::parse(&text),
        Err(ConstantsError::DuplicateState { state: 7, .. })
    ));
}

#[test]
fn a_level_that_does_not_fit_in_a_nibble_is_the_wrong_java_member() {
    for (column, row) in [(2, "0\t16\t0\t1\t1\t0\n"), (3, "0\t0\t99\t1\t1\t0\n")] {
        let text = full().replacen(ROW_ZERO, row, 1);
        let error = BlockConstants::parse(&text).expect_err("a level above fifteen");
        assert!(
            matches!(error, ConstantsError::OutOfRange { .. }),
            "column {column}: {error}"
        );
        assert!(
            error.to_string().contains("wrong member"),
            "the message has to point at the cause: {error}"
        );
    }
}

#[test]
fn a_row_that_is_not_numbers_names_the_field_and_the_line() {
    let text = full().replacen(ROW_ZERO, "0\tglass\t0\t1\t1\t0\n", 1);
    let error = BlockConstants::parse(&text).expect_err("a word where a number goes");
    let message = error.to_string();
    assert!(message.contains("opacity"), "{message}");
    assert!(message.contains("line 2"), "{message}");
}

#[test]
fn occlude_is_a_flag_and_not_a_number() {
    let text = full().replacen(ROW_ZERO, "0\t0\t0\t2\t1\t0\n", 1);
    let error = BlockConstants::parse(&text).expect_err("2 is not a boolean");
    assert!(error.to_string().contains("0 or 1"), "{error}");
}

#[test]
fn a_file_with_no_header_is_not_a_table_this_reader_can_read() {
    // Positional reading is what a header replaces, and a file with no header
    // is a file whose columns are whatever the last person assumed. Refused
    // rather than guessed at, which also covers the empty file.
    assert_eq!(BlockConstants::parse(""), Err(ConstantsError::NoHeader));
    assert_eq!(
        BlockConstants::parse("0\t0\t0\n"),
        Err(ConstantsError::NoHeader)
    );
}

#[test]
fn a_header_missing_a_column_the_reader_needs_names_it() {
    let text = full().replacen("opacity", "opactiy", 1);
    let error = BlockConstants::parse(&text).expect_err("a misspelled column");
    assert!(
        matches!(
            error,
            ConstantsError::MissingColumn {
                column: "opacity",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn columns_are_read_by_name_and_not_by_position() {
    // The whole argument for the header. A table with its columns in a
    // different order is the same table, and a reader that took the second
    // field as the opacity would read this one as emission.
    let mut text = String::from(
        "# emission	state_id	occlude	opacity	MOTION_BLOCKING
",
    );
    for state in 0..STATE_COUNT {
        let opacity = state % 16;
        let emission = (state * 7) % 16;
        text.push_str(&format!(
            "{emission}	{state}	1	{opacity}	0
"
        ));
    }
    let table = BlockConstants::parse(&text).expect("order is not meaning");
    assert_eq!(table.opacity(1234), (1234 % 16) as u8);
    assert_eq!(table.emission(1234), ((1234 * 7) % 16) as u8);
}

#[test]
fn a_flag_column_is_addressed_by_the_name_in_the_header() {
    let table = BlockConstants::parse(&full()).expect("a complete table");
    let motion = table
        .flag("MOTION_BLOCKING")
        .expect("the fixture carries that column");
    assert!(table.is_set(motion, 0));
    assert!(!table.is_set(motion, 1));
    assert!(table.is_set(motion, 2));
}

#[test]
fn a_flag_column_the_table_does_not_have_is_absent_rather_than_false() {
    // The difference matters: absent means "this table predates the column and
    // the caller should fall back", and false means "Minecraft says no". A
    // reader that collapsed the two would silently downgrade every server
    // whose operator extracted a table a version ago.
    let table = BlockConstants::parse(&full()).expect("a complete table");
    assert!(table.flag("OCEAN_FLOOR").is_none());
    assert_eq!(
        table.flags().collect::<Vec<_>>(),
        vec!["MOTION_BLOCKING", "WORLD_SURFACE"]
    );
}

#[test]
fn a_flag_that_is_not_zero_or_one_names_its_column() {
    let text = full().replacen(ROW_ZERO, "0\t0\t0\t1\t7\t0\n", 1);
    let error = BlockConstants::parse(&text).expect_err("7 is not a flag");
    let message = error.to_string();
    assert!(message.contains("MOTION_BLOCKING"), "{message}");
    assert!(message.contains("0 or 1"), "{message}");
}

#[test]
fn a_row_that_is_not_as_wide_as_the_header_is_refused() {
    // A row with a field missing would otherwise read every column after the
    // gap as the one beside it, which is the positional failure the header
    // exists to prevent arriving one row at a time.
    let text = full().replacen(ROW_ZERO, "0\t0\t0\t1\t1\n", 1);
    let error = BlockConstants::parse(&text).expect_err("a row one field short");
    assert!(
        matches!(error, ConstantsError::Malformed { line: 2, .. }),
        "{error}"
    );
}

/// A complete table that also carries the three sound columns.
///
/// The two sound names alternate so that a reader collapsing them onto one
/// answer fails, and they are real entries in `minecraft:sound_event` because
/// the parser resolves them there — a fixture with an invented name would be
/// testing the refusal rather than the reading.
fn full_with_sound() -> String {
    let mut text = String::from(
        "# state_id\topacity\temission\tocclude\tplace_sound\tsound_volume\tsound_pitch\tMOTION_BLOCKING\n",
    );
    for state in 0..STATE_COUNT {
        let (name, volume, pitch) = sound_of(state);
        text.push_str(&format!("{state}\t0\t0\t1\t{name}\t{volume}\t{pitch}\t1\n"));
    }
    text
}

/// The row [`full_with_sound`] writes for state 0, for the same reason
/// [`ROW_ZERO`] exists: a corruption that matched some other row would still
/// produce a passing test, and only the line number in the failure would say
/// so.
const SOUND_ROW_ZERO: &str = "0\t0\t0\t1\tminecraft:block.stone.place\t1\t1\t1\n";

/// What [`full_with_sound`] writes for a state, so the assertions and the
/// fixture cannot drift.
fn sound_of(state: u32) -> (&'static str, f32, f32) {
    if state % 2 == 0 {
        ("minecraft:block.stone.place", 1.0, 1.0)
    } else {
        ("minecraft:block.wool.place", 0.3, 1.5)
    }
}

#[test]
fn the_first_row_of_the_sound_fixture_is_the_one_the_tests_below_corrupt() {
    assert_eq!(
        full_with_sound().lines().nth(1),
        Some(SOUND_ROW_ZERO.trim_end())
    );
}

#[test]
fn a_table_with_the_sound_columns_reads_a_name_into_this_builds_registry() {
    let table = BlockConstants::parse(&full_with_sound()).expect("a complete table");
    assert!(table.has_place_sounds());
    let events = Registry::from_name("minecraft:sound_event").expect("the registry is generated");
    for state in [0, 1, 15, 1234, STATE_COUNT - 1] {
        let (name, volume, pitch) = sound_of(state);
        let sound = table.place_sound(state).expect("the column is there");
        assert_eq!(
            sound.sound,
            events.entry_id(name).expect("a real sound event"),
            "state {state} resolves to the id this build gives {name}"
        );
        assert_eq!(sound.volume, volume, "volume {state}");
        assert_eq!(sound.pitch, pitch, "pitch {state}");
    }
    // Two names in the fixture, so a reader that answered one thing for every
    // state — the shape a field resolved to the wrong member takes — is not
    // what just passed.
    assert_eq!(table.sound_groups(), 2);
}

#[test]
fn the_sound_columns_are_not_read_as_flags() {
    // The failure this catches is silent and specific: the flag columns are
    // "every column that is not one of the named ones", so a reader that did
    // not learn these three names would try to read `minecraft:block.stone.place`
    // as a 0 or a 1 — and, if it somehow did not, would offer a
    // `flag("place_sound")` that answers about a sound.
    let table = BlockConstants::parse(&full_with_sound()).expect("a complete table");
    assert_eq!(table.flags().collect::<Vec<_>>(), vec!["MOTION_BLOCKING"]);
    assert!(table.flag("place_sound").is_none());
    assert!(table.flag("sound_volume").is_none());
}

#[test]
fn a_table_written_before_the_sound_columns_is_silent_rather_than_wrong() {
    // The version this feature was added in is not the version an operator
    // last ran the extractor in. Absent is the state a server has to keep
    // running in, and it is distinguishable from "Minecraft says silence".
    let table = BlockConstants::parse(&full()).expect("a complete table");
    assert!(!table.has_place_sounds());
    assert_eq!(table.place_sound(0), None);
    assert_eq!(table.sound_groups(), 0);
}

#[test]
fn a_state_the_sound_table_does_not_reach_is_silent() {
    let table = BlockConstants::parse(&full_with_sound()).expect("a complete table");
    assert_eq!(table.place_sound(STATE_COUNT), None);
}

#[test]
fn two_of_the_three_sound_columns_is_refused_and_names_the_missing_one() {
    // A half-run oracle, or a hand-edited file. Reading what is there would
    // give every block a sound at whatever loudness the reader defaulted to,
    // which is a server that is audibly wrong and says nothing.
    for (drop, expected) in [
        ("place_sound", "place_sound"),
        ("sound_volume", "sound_volume"),
        ("sound_pitch", "sound_pitch"),
    ] {
        let text = full_with_sound().replacen(&format!("\t{drop}"), "\tsomething_else", 1);
        let error = BlockConstants::parse(&text).expect_err("two of the three");
        assert!(
            matches!(error, ConstantsError::MissingColumn { column, .. } if column == expected),
            "dropping {drop}: {error}"
        );
    }
}

#[test]
fn a_sound_this_build_has_no_entry_for_names_the_version_skew() {
    // The sound registry's own version-skew case, and the reason the column
    // holds a name. An id would have been a number in range and would have
    // played whatever this build's registry has at that position.
    let text = full_with_sound().replacen(
        SOUND_ROW_ZERO,
        "0\t0\t0\t1\tminecraft:block.dust.place\t1\t1\t1\n",
        1,
    );
    let error = BlockConstants::parse(&text).expect_err("a sound this build has never heard of");
    assert!(
        matches!(&error, ConstantsError::UnknownSound { name, .. } if name == "minecraft:block.dust.place"),
        "{error}"
    );
    assert!(
        error.to_string().contains("different Minecraft version"),
        "the message has to say what it actually means: {error}"
    );
}

#[test]
fn a_volume_that_is_not_a_loudness_is_refused() {
    for bad in ["NaN", "inf", "-1.0", "1e30", "loud"] {
        let text = full_with_sound().replacen(
            SOUND_ROW_ZERO,
            &format!("0\t0\t0\t1\tminecraft:block.stone.place\t{bad}\t1\t1\n"),
            1,
        );
        let error = BlockConstants::parse(&text).expect_err("a volume that is not one");
        let message = error.to_string();
        assert!(message.contains("sound_volume"), "{bad}: {message}");
        assert!(message.contains("line 2"), "{bad}: {message}");
    }
}
