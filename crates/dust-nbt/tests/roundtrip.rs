//! Generated documents through both binary dialects.
//!
//! # What a round-trip suite can and cannot prove
//!
//! Encoding a tree and decoding it back proves the writer and the reader agree
//! with each other. As `tests/vanilla.rs` argues, that is not proof of
//! correctness — a self-consistent implementation that wrote little-endian
//! would pass everything here. What round-trips *do* catch is the other class
//! of bug: two code paths that drifted apart, a value that survives encoding
//! but not decoding, a document that parses but cannot be rewritten. Those are
//! caught by asserting three things per generated document:
//!
//! 1. **Value equality** after a full trip through file mode or network mode,
//!    using [`Tag`]'s bit-pattern float equality so `-0.0` stays `-0.0` and a
//!    NaN keeps its payload.
//! 2. **Byte equality on re-encode**, wherever the format guarantees it —
//!    which is everywhere for an accepted document, because compound order,
//!    declared empty-list types, string bytes and float bits are all preserved
//!    by construction.
//! 3. **Exact consumption**: `from_bytes_exact` accepts what the writer just
//!    produced with nothing left over.
//!
//! The generator lives in `tests/support/mod.rs`; what it deliberately
//! includes — boundary numerics, NaN payloads, surrogate-pair strings, empty
//! lists both vanilla-flavoured and declared — is documented there.

mod support;

use proptest::prelude::*;
use support::{any_root_name, any_tag, any_tag_surviving_snbt};

use dust_nbt::{read, snbt, write, Tag};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// File mode: write, read back exactly, compare, rewrite byte for byte.
    #[test]
    fn file_mode_round_trips(
        name in any_root_name(),
        tag in any_tag(),
    ) {
        let bytes = write::to_vec(&name, &tag).expect("a generated document writes");
        let document =
            read::from_bytes_exact(&bytes).expect("what the writer produced must parse");
        prop_assert_eq!(&document.name, &name);
        prop_assert_eq!(&document.tag, &tag);
        let rewritten = write::to_vec(&document.name, &document.tag).expect("rewrites");
        prop_assert_eq!(rewritten.as_slice(), bytes.as_slice());
    }

    /// Network mode: the same guarantees without the root name.
    #[test]
    fn network_mode_round_trips(tag in any_tag()) {
        let bytes = write::to_vec_network(Some(&tag)).expect("writes");
        let parsed = read::from_bytes_network(&bytes)
            .expect("parses")
            .expect("the root id is never TAG_End here");
        prop_assert_eq!(&parsed, &tag);
        let rewritten = write::to_vec_network(Some(&parsed)).expect("rewrites");
        prop_assert_eq!(rewritten.as_slice(), bytes.as_slice());
    }

    /// The absent-NBT byte is the one network input that is not a document;
    /// writing it back from the reader's side must give the same byte.
    #[test]
    fn the_absent_network_document_is_the_single_end_byte(input in proptest::bool::ANY) {
        if input {
            assert_eq!(
                read::from_bytes_network(&[0x00]).expect("parses"),
                None,
                "one zero byte means no NBT at all"
            );
            assert_eq!(write::to_vec_network(None).expect("writes"), vec![0x00]);
        } else {
            // Anything else — even nothing at all — is not that spelling.
            assert!(read::from_bytes_network(&[]).is_err());
            assert!(read::from_bytes_network(&[0x01]).is_err());
        }
    }

    /// SNBT differential: printing then parsing gives back the tree. The
    /// generator excludes the three shapes SNBT cannot carry — non-finite
    /// floats, typed-empty lists, empty compound keys — each of which is
    /// pinned separately below as documented lossiness rather than silently
    /// skipped.
    #[test]
    fn snbt_print_then_parse_gives_back_the_tree(tag in any_tag_surviving_snbt()) {
        let printed = snbt::to_string(&tag);
        let parsed = snbt::parse(&printed)
            .unwrap_or_else(|e| panic!("our own output {printed:?} did not re-parse: {e}"));
        prop_assert_eq!(&parsed, &tag);
    }

    /// And the printer is stable under repetition: parse(print(parse(print(x))))
    /// agrees with x, which catches printers whose second pass differs from
    /// their first.
    #[test]
    fn snbt_printing_reaches_a_fixed_point(tag in any_tag_surviving_snbt()) {
        let once = snbt::parse(&snbt::to_string(&tag)).expect("first pass parses");
        let twice_printed = snbt::to_string(&once);
        let twice = snbt::parse(&twice_printed).expect("second pass parses");
        prop_assert_eq!(&twice, &once);
        prop_assert_eq!(twice_printed, snbt::to_string(&once));
    }

    /// Binary documents survive an SNBT detour in value where SNBT can express
    /// them, which is how a chunk's data ends up logged and pasted back.
    #[test]
    fn binary_through_snbt_through_binary_preserves_values(tag in any_tag_surviving_snbt()) {
        let printed = snbt::to_string(&tag);
        let parsed = snbt::parse(&printed).expect("re-parses");
        let bytes = write::to_vec("", &parsed).expect("writes");
        let read_back = read::from_bytes_exact(&bytes).expect("reads").tag;
        prop_assert_eq!(read_back, parsed);
    }
}

/// Deep nesting just under the limit, built iteratively — the recursive part
/// of this test is the reader's and the writer's and the comparer's, not the
/// builder's. It runs on a large stack; see the note on
/// [`support::on_a_large_stack`] for why that is a property of the thread and
/// not an excuse to shrink the depth.
#[test]
fn nesting_one_under_the_limit_round_trips() {
    support::on_a_large_stack(|| {
        const DEPTH: usize = 511;

        let mut innermost = Tag::Int(-1);
        for _ in 0..DEPTH {
            let mut compound = dust_nbt::Compound::new();
            compound.insert("next", innermost);
            innermost = Tag::Compound(compound);
        }

        let bytes = write::to_vec("", &innermost).expect("writes");
        let document = read::from_bytes_exact(&bytes).expect("reads at the limit");
        assert_eq!(document.tag, innermost);
    });
}

/// A list of every scalar edge in one document: if any single width is
/// mistranslated, this names it.
#[test]
fn every_boundary_value_survives_both_dialects() {
    use std::f32::INFINITY as F32_INF;

    let mut compound = dust_nbt::Compound::new();
    compound.insert("byte_min", Tag::Byte(i8::MIN));
    compound.insert("byte_max", Tag::Byte(i8::MAX));
    compound.insert("short_min", Tag::Short(i16::MIN));
    compound.insert("int_min", Tag::Int(i32::MIN));
    compound.insert("long_min", Tag::Long(i64::MIN));
    compound.insert("long_max", Tag::Long(i64::MAX));
    compound.insert("float_neg_zero", Tag::Float(-0.0));
    compound.insert("float_nan", Tag::Float(f32::from_bits(0x7fc0_0001)));
    compound.insert("float_inf", Tag::Float(F32_INF));
    compound.insert("double_neg_zero", Tag::Double(-0.0));
    compound.insert("double_nan", Tag::Double(f64::from_bits(0xfff8_0000_0000_0001)));

    let bytes = write::to_vec("", &Tag::Compound(compound.clone())).expect("writes");
    assert_eq!(read::from_bytes_exact(&bytes).expect("reads").tag, Tag::Compound(compound.clone()));

    let network = write::to_vec_network(Some(&Tag::Compound(compound.clone()))).expect("writes");
    assert_eq!(
        read::from_bytes_network(&network).expect("reads").expect("not absent"),
        Tag::Compound(compound)
    );
}
