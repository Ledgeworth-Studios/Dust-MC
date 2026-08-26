//! The borrowed reader: agreement with the owned one, and its own views.
//!
//! # Why agreement is the load-bearing property
//!
//! [`borrow`](dust_nbt::borrow) is a second parser over the same format. Two
//! parsers drift; that is not a risk, it is what two parsers do. The drift is
//! caught by making the owned reader the reference implementation and
//! asserting, over generated documents and over mutated hostile ones, that
//! this module answers every input identically: same document when accepted,
//! same error — variant and offset both — when refused. What remains to test
//! separately is only what the owned reader has no answer to: that views
//! actually view (no hidden copies), and that text resolves through the
//! document's region.

mod support;

use proptest::prelude::*;
use support::{any_root_name, any_tag};

use dust_nbt::{borrow, read, write, Limits, Tag};

/// Rebuild an owned tree from a borrowed one.
///
/// The comparison runs through the owned model because that is where the
/// crate keeps its document-equality rules: bit-pattern floats so `-0.0`
/// stays distinct, byte-exact strings, order-sensitive compounds. Converting
/// allocates everything back, which is exactly right for a test and exactly
/// wrong for production — production callers should never need this walk.
fn materialise(document: &borrow::Document<'_>, value: &borrow::Value<'_>) -> Tag {
    match value {
        borrow::Value::Byte(v) => Tag::Byte(*v),
        borrow::Value::Short(v) => Tag::Short(*v),
        borrow::Value::Int(v) => Tag::Int(*v),
        borrow::Value::Long(v) => Tag::Long(*v),
        borrow::Value::Float(v) => Tag::Float(*v),
        borrow::Value::Double(v) => Tag::Double(*v),
        borrow::Value::ByteArray(bytes) => Tag::ByteArray(bytes.iter().map(|b| b as i8).collect()),
        borrow::Value::String(text) => Tag::String(document.text(*text).to_owned()),
        borrow::Value::List(list) => {
            let elements = match list.values() {
                Some(values) => values
                    .iter()
                    .map(|value| materialise(document, value))
                    .collect(),
                None => (0..list.len())
                    .map(|index| {
                        list.get(index)
                            .map(|value| materialise(document, &value))
                            .expect("len bounds get")
                    })
                    .collect(),
            };
            Tag::List(
                dust_nbt::List::from_elements(list.element_type(), elements)
                    .expect("the borrowed list was homogeneous on parse"),
            )
        }
        borrow::Value::Compound(compound) => {
            let mut out = dust_nbt::Compound::new();
            for (name, value) in compound.iter() {
                // Append, not insert: file order, duplicates included. The
                // owned reader appended them too.
                out.append(
                    document.text(*name).to_owned(),
                    materialise(document, value),
                );
            }
            Tag::Compound(out)
        }
        borrow::Value::IntArray(ints) => Tag::IntArray(ints.iter().collect()),
        borrow::Value::LongArray(longs) => Tag::LongArray(longs.iter().collect()),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// File form: the borrowed view of a written document rebuilds the exact
    /// tree that went in, name and all.
    #[test]
    fn borrowed_file_parse_agrees_with_the_owned_tree(name in any_root_name(), tag in any_tag()) {
        let bytes = write::to_vec(&name, &tag).expect("writes");
        let document = borrow::from_bytes_exact(&bytes).expect("parses");
        prop_assert_eq!(document.root_name(), &name);
        prop_assert_eq!(materialise(&document, document.root()), tag);
    }

    /// Network form likewise, absent root name and all.
    #[test]
    fn borrowed_network_parse_agrees_with_the_owned_tree(tag in any_tag()) {
        let bytes = write::to_vec_network(Some(&tag)).expect("writes");
        let parsed = borrow::from_bytes_network(&bytes)
            .expect("parses")
            .expect("not the absent byte");
        prop_assert_eq!(materialise(&parsed, parsed.root()), tag);
    }
}

// ---------------------------------------------------------------------------
// Views: what makes this module worth existing
// ---------------------------------------------------------------------------

#[test]
fn scalar_lists_are_strides_not_vectors() {
    // Pos: [1.5, 64.0, -3.25] as doubles.
    let mut pos = dust_nbt::List::new(dust_nbt::TagType::Double);
    for coordinate in [1.5, 64.0, -3.25] {
        pos.push(Tag::Double(coordinate)).expect("homogeneous");
    }
    let mut compound = dust_nbt::Compound::new();
    compound.insert("Pos", Tag::List(pos));
    let bytes = write::to_vec("", &Tag::Compound(compound)).expect("writes");

    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    let pos = document
        .get("Pos")
        .and_then(borrow::Value::as_list)
        .expect("a list");
    assert_eq!(pos.element_type(), dust_nbt::TagType::Double);
    assert_eq!(pos.len(), 3);
    assert!(
        pos.values().is_none(),
        "a scalar-backed list has no materialised nodes to share"
    );

    assert_eq!(pos.get(0), Some(borrow::Value::Double(1.5)));
    assert_eq!(pos.get(2), Some(borrow::Value::Double(-3.25)));
    assert_eq!(pos.get(3), None, "past the end is none, not a panic");
}

#[test]
fn byte_arrays_lend_the_input_verbatim() {
    // A lighting-style payload: unsigned nibbles stored in signed bytes.
    let payload: Vec<i8> = [-1, 0, 127, -128, 42].to_vec();
    let bytes = write::to_vec("", &Tag::ByteArray(payload.clone())).expect("writes");

    // The array *is* the root here; nothing wraps it.
    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    let borrow::Value::ByteArray(array) = document.root() else {
        panic!("a byte array went in")
    };
    assert_eq!(array.len(), 5);
    assert_eq!(
        array.as_slice(),
        &[255u8, 0, 127, 128, 42],
        "the wire bytes are the signed values reinterpreted, untouched"
    );
    for (index, expected) in payload.iter().enumerate() {
        assert_eq!(
            array.as_i8(index),
            Some(*expected),
            "signed reading at {index}"
        );
    }
}

#[test]
fn int_array_views_decode_the_big_endian_words() {
    let words = vec![i32::MIN, -1, 0, 1, i32::MAX];
    let bytes = write::to_vec("", &Tag::IntArray(words.clone())).expect("writes");

    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    let borrow::Value::IntArray(ints) = document.root() else {
        panic!("an int array went in")
    };
    assert_eq!(ints.len(), 5);
    assert_eq!(ints.iter().collect::<Vec<_>>(), words);

    // The wire form is big-endian regardless of host endianness; the raw
    // slice is the whole payload, untouched.
    assert_eq!(
        ints.as_slice(),
        &[
            0x80, 0x00, 0x00, 0x00, // i32::MIN
            0xff, 0xff, 0xff, 0xff, // -1
            0x00, 0x00, 0x00, 0x00, // 0
            0x00, 0x00, 0x00, 0x01, // 1
            0x7f, 0xff, 0xff, 0xff, // i32::MAX
        ]
    );
}

#[test]
fn long_array_views_decode_the_big_endian_words() {
    let words = vec![i64::MIN, 3955, i64::MAX];
    let bytes = write::to_vec("", &Tag::LongArray(words.clone())).expect("writes");

    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    let borrow::Value::LongArray(longs) = document.root() else {
        panic!("a long array went in")
    };
    assert_eq!(longs.iter().collect::<Vec<_>>(), words);
    assert_eq!(longs.get(2), Some(i64::MAX));
}

#[test]
fn strings_resolve_through_the_documents_region() {
    let mut compound = dust_nbt::Compound::new();
    compound.insert("id", Tag::String("minecraft:stone".to_owned()));
    compound.insert("name", Tag::String("notch\u{0000}\u{1f600}".to_owned()));
    let bytes = write::to_vec("", &Tag::Compound(compound)).expect("writes");

    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    assert_eq!(document.root_name(), "");

    let id = document.get("id").expect("id field");
    let borrow::Value::String(handle) = id else {
        panic!("a string field")
    };
    assert_eq!(document.text(*handle), "minecraft:stone");

    // The NUL-and-surrogate-pair case survives decode into the region.
    let name = document.get("name").and_then(|value| match value {
        borrow::Value::String(handle) => Some(document.text(*handle)),
        _ => None,
    });
    assert_eq!(name, Some("notch\u{0000}\u{1f600}"));
}

#[test]
fn duplicate_keys_resolve_to_the_last_binding_here_too() {
    // {a:"first", a:"second"} built the way the reader builds it: appended,
    // both kept, last wins on lookup.
    let bytes: Vec<u8> = [
        &[0x0a, 0x00, 0x00][..],
        &[
            0x08, 0x00, 0x01, b'a', 0x00, 0x05, b'f', b'i', b'r', b's', b't',
        ],
        &[
            0x08, 0x00, 0x01, b'a', 0x00, 0x06, b's', b'e', b'c', b'o', b'n', b'd',
        ],
        &[0x00],
    ]
    .concat();

    let document = borrow::from_bytes_exact(&bytes).expect("reads");
    let compound = document.root().as_compound().expect("compound");
    assert_eq!(compound.len(), 2, "both bindings stay");
    let resolved = document.compound_get(compound, "a").expect("resolves");
    let borrow::Value::String(handle) = resolved else {
        panic!("string fields")
    };
    assert_eq!(document.text(*handle), "second");
    assert_eq!(document.get("a"), document.compound_get(compound, "a"));
}

#[test]
fn paths_walk_fields_and_indices_through_views() {
    let mut inner = dust_nbt::Compound::new();
    inner.insert("Y", Tag::Byte(-4));
    let mut sections = dust_nbt::List::new(dust_nbt::TagType::Compound);
    sections.push(Tag::Compound(inner)).expect("homogeneous");

    // A scalar list alongside it: index paths reach decoded scalars too.
    let mut pos = dust_nbt::List::new(dust_nbt::TagType::Double);
    for coordinate in [7.5, 64.0, 2.25] {
        pos.push(Tag::Double(coordinate)).expect("homogeneous");
    }

    let mut root = dust_nbt::Compound::new();
    root.insert("sections", Tag::List(sections));
    root.insert("Pos", Tag::List(pos));

    let bytes = write::to_vec("", &Tag::Compound(root)).expect("writes");
    let document = borrow::from_bytes_exact(&bytes).expect("reads");

    assert_eq!(
        document.get_path(&["sections", "0", "Y"]),
        Some(borrow::Value::Byte(-4))
    );
    assert_eq!(
        document.get_path(&["Pos", "1"]),
        Some(borrow::Value::Double(64.0))
    );
    assert_eq!(document.get_path(&["sections", "9"]), None);
    assert_eq!(document.get_path(&["nope"]), None);
}

#[test]
fn the_absent_network_document_is_none_here_too() {
    assert!(
        borrow::from_bytes_network(&[0x00])
            .expect("parses")
            .is_none(),
        "the single zero byte still means no NBT"
    );

    let bytes = write::to_vec_network(Some(&Tag::Byte(1))).expect("writes");
    let document = borrow::from_bytes_network(&bytes)
        .expect("parses")
        .expect("present");
    assert_eq!(document.root(), &borrow::Value::Byte(1));
    assert_eq!(document.root_name(), "", "network form carries no name");
}

#[test]
fn limits_are_enforced_with_the_same_errors() {
    use dust_nbt::Error;

    // Reading *at* the legal limit is where legitimate documents live, and
    // the refusal one past it must fire before the stack is spent; both ends
    // of that run where the hostile suite runs theirs. See
    // [`support::on_a_large_stack`].
    support::on_a_large_stack(|| {
        // One past the depth limit, built linearly: `[` wrappers around a byte.
        const DEPTH: usize = 513;
        let mut deep = Vec::with_capacity(6 + DEPTH * 5);
        deep.extend_from_slice(&[0x09, 0x00, 0x00]); // root list, empty name
        for _ in 0..DEPTH - 1 {
            deep.extend_from_slice(&[0x09, 0x00, 0x00, 0x00, 0x01]);
        }
        deep.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x01, 0x2a]);

        match borrow::from_bytes(&deep) {
            Err(Error::TooDeep { limit, .. }) => assert_eq!(limit, Limits::FILE.max_depth),
            other => panic!("expected TooDeep from the borrowed reader, got {other:?}"),
        }
    });

    // A lying array length is refused before any reservation, same numbers.
    let mut lying = vec![0x07, 0x00, 0x00];
    lying.extend_from_slice(&1_000_000i32.to_be_bytes());
    match borrow::from_bytes(&lying) {
        Err(Error::LengthExceedsInput {
            claimed, available, ..
        }) => {
            assert_eq!(claimed, 1_000_000);
            assert_eq!(available, 0);
        }
        other => panic!("expected LengthExceedsInput, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The differential against mutations
// ---------------------------------------------------------------------------

/// Every mutated corpus document gets the same answer from both readers:
/// the same document when accepted, or the same error — equal under
/// [`Error`]'s own equality, offsets included — when refused. Where the two
/// implementations could legally diverge is the heap budget, charged in each
/// crate's own sizes; file limits leave it unlimited, which keeps this test
/// inside the region where parity is a real requirement rather than an
/// accident of tuning.
#[test]
fn mutations_get_identical_answers_from_both_readers() {
    const ITERATIONS: usize = 4_000;
    const SEED: u64 = 0x5EED_0003;

    let originals = support::corpus_documents();
    let mut rng = support::SplitMix64::new(SEED);
    let mut agreed_accepted = 0usize;
    let mut agreed_refused = 0usize;

    for iteration in 0..ITERATIONS {
        let mut bytes = originals[rng.below(originals.len())].clone();
        for _ in 0..1 + rng.below(8) {
            support::mutate(&mut rng, &mut bytes);
        }

        let owned = read::from_bytes_with(&bytes, Limits::FILE);
        let borrowed = borrow::from_bytes_with(&bytes, Limits::FILE);

        match (&owned, &borrowed) {
            (Ok(owned_doc), Ok(document)) => {
                agreed_accepted += 1;
                assert_eq!(
                    materialise(document, document.root()),
                    owned_doc.tag,
                    "iteration {iteration}: documents disagree"
                );
                assert_eq!(
                    document.root_name(),
                    owned_doc.name,
                    "iteration {iteration}: names disagree"
                );
            }
            (Err(owned_error), Err(borrowed_error)) => {
                agreed_refused += 1;
                assert_eq!(
                    owned_error, borrowed_error,
                    "iteration {iteration}: refusals disagree"
                );
            }
            (owned_outcome, borrowed_outcome) => panic!(
                "iteration {iteration}: the readers disagree outright — \
                 owned {owned_outcome:?} against borrowed {borrowed_outcome:?}"
            ),
        }
    }

    assert!(
        agreed_accepted > 50 && agreed_refused > 50,
        "both outcomes should be common, got {agreed_accepted}/{agreed_refused}"
    );
}
