//! The tag model itself: what the containers promise before any bytes exist.
//!
//! # Why this file is not redundant with the byte-level suites
//!
//! `tests/binary.rs` pins encodings and `tests/roundtrip.rs` pins agreement
//! between reader and writer. Neither says anything about the *container*
//! contracts a downstream crate builds on: that `insert` replaces visibly,
//! that duplicate keys resolve to the last binding everywhere and not just in
//! one method, that `List::push` adopts rather than widens. A caller who hits
//! one of those promises being broken will not be reading bytes at the time —
//! they will be building or editing a document — so the promises are tested
//! exactly there.

use dust_nbt::{Compound, List, ListError, Tag, TagType};

#[test]
fn insert_replaces_in_place_and_returns_the_old_value() {
    let mut compound = Compound::new();
    compound.insert("a", Tag::Byte(1));
    assert_eq!(compound.insert("a", Tag::Byte(2)), Some(Tag::Byte(1)));
    assert_eq!(compound.get("a"), Some(&Tag::Byte(2)));
    assert_eq!(compound.len(), 1, "replacement does not grow the compound");
}

/// The rule that makes duplicate keys safe to carry: every read resolves to
/// the last binding, and so does every write. If `insert` replaced the first
/// binding instead, `{a=1, a=2}` plus `insert("a", 3)` would leave `get`
/// answering `2` — a write nobody can observe.
#[test]
fn insert_on_a_duplicated_key_replaces_the_binding_that_get_resolves() {
    let mut compound = Compound::new();
    compound.append("a".to_owned(), Tag::Byte(1));
    compound.append("a".to_owned(), Tag::Byte(2));

    assert_eq!(compound.insert("a", Tag::Byte(3)), Some(Tag::Byte(2)));

    let fields: Vec<i8> = compound
        .iter()
        .filter_map(|(_, value)| value.as_i64().map(|v| v as i8))
        .collect();
    assert_eq!(
        fields,
        vec![1, 3],
        "order is kept; the last binding changed"
    );
    assert_eq!(compound.get("a"), Some(&Tag::Byte(3)));
}

#[test]
fn append_allows_duplicates_and_remove_takes_the_last() {
    let mut compound = Compound::new();
    compound.append("id".to_owned(), Tag::String("first".to_owned()));
    compound.append("keep".to_owned(), Tag::Int(7));
    compound.append("id".to_owned(), Tag::String("second".to_owned()));

    assert_eq!(
        compound.remove("id"),
        Some(Tag::String("second".to_owned()))
    );
    let order: Vec<&str> = compound.keys().collect();
    assert_eq!(
        order,
        vec!["id", "keep"],
        "removal preserves the rest's order"
    );
    assert_eq!(compound.get("id").and_then(Tag::as_str), Some("first"));
}

#[test]
fn get_mut_sees_the_last_binding_too() {
    let mut compound = Compound::new();
    compound.append("x".to_owned(), Tag::Int(1));
    compound.append("x".to_owned(), Tag::Int(2));
    if let Some(Tag::Int(value)) = compound.get_mut("x") {
        *value = 9;
    }
    assert_eq!(compound.get("x"), Some(&Tag::Int(9)));
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[test]
fn push_adopts_the_type_of_an_empty_end_list() {
    let mut list = List::new(TagType::End);
    list.push(Tag::Short(-3))
        .expect("an empty list takes anything");
    assert_eq!(list.element_type(), TagType::Short);
    assert_eq!(list.len(), 1);
}

#[test]
fn push_refuses_mismatches_and_names_the_index() {
    let mut list = List::new(TagType::End);
    list.push(Tag::Int(1)).expect("first element sets the type");
    let error = list.push(Tag::Long(2)).expect_err("a long is not an int");
    assert_eq!(
        error,
        ListError {
            index: 1,
            expected: TagType::Int,
            found: TagType::Long,
        }
    );
    assert_eq!(
        error.to_string(),
        "list element 1 is TAG_Long but the list holds TAG_Int"
    );
    assert_eq!(list.len(), 1, "a refused element is not appended");
}

#[test]
fn from_elements_names_the_first_offender() {
    let error = List::from_elements(
        TagType::Float,
        vec![
            Tag::Float(1.0),
            Tag::Double(2.0),
            Tag::String("nope".to_owned()),
        ],
    )
    .expect_err("two wrong elements");
    assert_eq!(error.index, 1, "the first offender, not the last");
    assert_eq!(error.expected, TagType::Float);
    assert_eq!(error.found, TagType::Double);
}

#[test]
fn empty_lists_keep_their_declared_type_through_construction() {
    // Built empty with a declared type: it stays.
    let declared = List::new(TagType::IntArray);
    assert_eq!(declared.element_type(), TagType::IntArray);
    assert!(declared.is_empty());

    // Built empty without one: vanilla's spelling.
    assert_eq!(List::new(TagType::End).element_type(), TagType::End);

    // Built from nothing via from_elements: also fine, type kept.
    let built = List::from_elements(TagType::LongArray, Vec::new()).expect("empty is homogeneous");
    assert_eq!(built.element_type(), TagType::LongArray);
}

#[test]
fn consuming_iteration_yields_ownership_in_order() {
    let list = List::from_elements(
        TagType::Byte,
        vec![Tag::Byte(1), Tag::Byte(2), Tag::Byte(3)],
    )
    .expect("homogeneous");
    let values: Vec<i8> = list
        .into_iter()
        .map(|tag| match tag {
            Tag::Byte(value) => value,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(values, vec![1, 2, 3]);

    let mut compound = Compound::new();
    compound.insert("a", Tag::Int(1));
    compound.insert("b", Tag::Int(2));
    let names: Vec<&str> = compound.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
    let owned: Vec<(String, Tag)> = compound.into_iter().collect();
    assert_eq!(owned.len(), 2);
    assert_eq!(owned[0].0, "a");
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

#[test]
fn paths_walk_fields_then_indices_then_stop_at_scalars() {
    let mut inner = Compound::new();
    inner.insert("Y", Tag::Byte(-4));
    let mut sections = List::new(TagType::Compound);
    sections.push(Tag::Compound(inner)).expect("homogeneous");

    let mut root = Compound::new();
    root.insert("sections", Tag::List(sections));
    let root = Tag::Compound(root);

    assert_eq!(
        root.get_path(&["sections"]),
        root.as_compound().and_then(|c| c.get("sections"))
    );
    assert_eq!(root.get_path(&["sections", "0", "Y"]), Some(&Tag::Byte(-4)));

    // Failures are None, each for its own reason: no such field, not an
    // index-shaped segment, out of range, and walking into a scalar.
    assert_eq!(root.get_path(&["nope"]), None);
    assert_eq!(root.get_path(&["sections", "zero"]), None);
    assert_eq!(root.get_path(&["sections", "5"]), None);
    assert_eq!(root.get_path(&["sections", "0", "Y", "deeper"]), None);

    // The empty path is the tag itself.
    assert_eq!(root.get_path(&[]), Some(&root));
}

// ---------------------------------------------------------------------------
// Equality is about documents
// ---------------------------------------------------------------------------

#[test]
fn float_equality_is_bit_pattern_equality() {
    // NaN equals itself: the question Tag equality answers is "same document",
    // and a NaN read back from a file is the same tag it was written from.
    let nan = f32::from_bits(0x7fc0_0001);
    assert_eq!(Tag::Float(nan), Tag::Float(nan));

    // Different payloads are different tags — do not ask numeric questions of
    // document equality.
    assert_ne!(
        Tag::Float(f32::NAN),
        Tag::Float(f32::from_bits(0x7fc0_0002))
    );

    // And negative zero differs from zero, because the bytes differ.
    assert_ne!(Tag::Double(0.0), Tag::Double(-0.0));
}
