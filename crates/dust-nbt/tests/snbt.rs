//! SNBT: the text form players type and commands take.
//!
//! # What the grammar is anchored to
//!
//! The parser's numeric rules are transcriptions of the seven regexes in
//! Minecraft's `TagParser`, so this file checks two different things. Against
//! **vanilla**: that the surprising rules hold — leading zeros make strings,
//! overflow falls through to a string, `5.` is a double while `5` is an int,
//! `[B;1]` is refused because widening is not vanilla's behaviour. Against
//! **itself**: the printer/parser differential, whose generator lives in
//! `tests/support/mod.rs`.
//!
//! Three shapes are *known* not to survive an SNBT round trip and are pinned
//! here as documented lossiness rather than left to be discovered: non-finite
//! floats print the way Java prints them and read back as strings; an empty
//! list loses its declared element type to the `[]` syntax; and an empty
//! compound key prints as `""` but vanilla's rule against empty keys refuses
//! to read it back.

mod support;

use dust_nbt::{snbt, Compound, List, Tag, TagType};

/// A compound with one field, for small grammar cases.
fn one_field(key: &str, value: Tag) -> Tag {
    let mut compound = Compound::new();
    compound.insert(key, value);
    Tag::Compound(compound)
}

// ---------------------------------------------------------------------------
// Numbers: the seven rules, in the order vanilla tries them
// ---------------------------------------------------------------------------

#[test]
fn integer_suffix_forms_parse_to_their_widths() {
    assert_eq!(snbt::parse("42b"), Ok(Tag::Byte(42)));
    assert_eq!(
        snbt::parse("42B"),
        Ok(Tag::Byte(42)),
        "suffixes are case-insensitive"
    );
    assert_eq!(snbt::parse("-129s"), Ok(Tag::Short(-129)));
    assert_eq!(snbt::parse("9223372036854775807L"), Ok(Tag::Long(i64::MAX)));
    assert_eq!(
        snbt::parse("-0"),
        Ok(Tag::Int(0)),
        "negative zero of an int is zero"
    );
    assert_eq!(
        snbt::parse("+7"),
        Ok(Tag::Int(7)),
        "a leading plus is part of the pattern"
    );
}

#[test]
fn float_and_double_forms_parse_to_their_widths() {
    assert_eq!(
        snbt::parse("1f"),
        Ok(Tag::Float(1.0)),
        "a bare integer takes the f suffix"
    );
    assert_eq!(snbt::parse("1F"), Ok(Tag::Float(1.0)));
    assert_eq!(snbt::parse(".5f"), Ok(Tag::Float(0.5)));
    assert_eq!(
        snbt::parse("1.d"),
        Ok(Tag::Double(1.0)),
        "digits-then-point needs no fraction"
    );
    assert_eq!(snbt::parse("-2.5e10d"), Ok(Tag::Double(-2.5e10)));
    assert_eq!(
        snbt::parse("5."),
        Ok(Tag::Double(5.0)),
        "the bare-double rule wants the point"
    );
    assert_eq!(
        snbt::parse("-.25"),
        Ok(Tag::Double(-0.25)),
        "the point may lead with nothing before it"
    );
}

#[test]
fn the_surprising_fall_throughs_match_vanilla() {
    // An integer may not have a leading zero; the whole word becomes a string,
    // suffix and all.
    assert_eq!(snbt::parse("01"), Ok(Tag::String("01".to_owned())));
    assert_eq!(snbt::parse("007b"), Ok(Tag::String("007b".to_owned())));

    // A number that overflows its type becomes a string of itself, not an
    // error and not a wider type.
    assert_eq!(snbt::parse("300b"), Ok(Tag::String("300b".to_owned())));
    assert_eq!(snbt::parse("32768s"), Ok(Tag::String("32768s".to_owned())));
    assert_eq!(
        snbt::parse("99999999999"),
        Ok(Tag::String("99999999999".to_owned())),
        "an int too big to be an int is nobody's number"
    );

    // The exponent belongs only to the float and double rules; a bare `1e10`
    // matches none of the seven and is a string.
    assert_eq!(snbt::parse("1e10"), Ok(Tag::String("1e10".to_owned())));

    // But the suffixed forms do take exponents.
    assert_eq!(snbt::parse("1e10d"), Ok(Tag::Double(1e10)));
}

#[test]
fn true_and_false_are_bytes_case_insensitively() {
    assert_eq!(snbt::parse("true"), Ok(Tag::Byte(1)));
    assert_eq!(snbt::parse("FALSE"), Ok(Tag::Byte(0)));
    assert_eq!(snbt::parse("tRuE"), Ok(Tag::Byte(1)));

    // Only as a whole unquoted word: quoted or decorated, they are strings.
    assert_eq!(snbt::parse("'true'"), Ok(Tag::String("true".to_owned())));
    assert_eq!(
        snbt::parse("truefalse"),
        Ok(Tag::String("truefalse".to_owned()))
    );

    // And printing goes back through the numeric path, which round-trips.
    assert_eq!(
        snbt::to_string(&Tag::Byte(1)),
        "1b",
        "there is no boolean tag to print"
    );
}

// ---------------------------------------------------------------------------
// Arrays and lists
// ---------------------------------------------------------------------------

#[test]
fn array_syntax_requires_the_exact_element_type() {
    use dust_nbt::snbt::Expected;

    assert_eq!(
        snbt::parse("[B;1b,-2b]"),
        Ok(Tag::ByteArray(vec![1, -2])),
        "elements must carry the matching suffix"
    );
    assert_eq!(snbt::parse("[I;1,-2,3]"), Ok(Tag::IntArray(vec![1, -2, 3])));
    assert_eq!(
        snbt::parse("[L;-9223372036854775808L]"),
        Ok(Tag::LongArray(vec![i64::MIN]))
    );

    // Vanilla's readArray compares parsed type to element type exactly:
    // widening a bare int would accept what the game rejects.
    let error = snbt::parse("[B;1]").expect_err("an unsuffixed int is not a byte");
    assert_eq!(error.offset, 3, "the error points at the element");
    assert_eq!(error.expected, Expected::ArrayElement(TagType::ByteArray));
    let error = snbt::parse("[I;1L]").expect_err("a long is not an int either");
    assert_eq!(error.offset, 3);
    assert_eq!(error.expected, Expected::ArrayElement(TagType::IntArray));
}

#[test]
fn empty_arrays_have_all_three_spellings() {
    assert_eq!(
        snbt::parse("[B;]"),
        Ok(Tag::ByteArray(Vec::new())),
        "the prefix alone is a complete empty array"
    );
    assert_eq!(snbt::parse("[I;]"), Ok(Tag::IntArray(Vec::new())));
    assert_eq!(snbt::parse("[L;]"), Ok(Tag::LongArray(Vec::new())));
    assert_eq!(snbt::to_string(&Tag::ByteArray(Vec::new())), "[B;]");
    assert_eq!(snbt::to_string(&Tag::IntArray(Vec::new())), "[I;]");
    assert_eq!(snbt::to_string(&Tag::LongArray(Vec::new())), "[L;]");
}

#[test]
fn lists_take_the_type_of_their_first_element() {
    let expected = List::from_elements(TagType::Float, vec![Tag::Float(1.0), Tag::Float(2.0)])
        .expect("homogeneous");
    assert_eq!(snbt::parse("[1f,2f]"), Ok(Tag::List(expected)));
    assert_eq!(snbt::parse("[]"), Ok(Tag::List(List::new(TagType::End))));
    assert_eq!(snbt::to_string(&Tag::List(List::new(TagType::End))), "[]");

    // Mixed types are refused at the offending element.
    let error = snbt::parse("[1,2s]").expect_err("a short cannot follow an int");
    assert_eq!(error.offset, 3);
}

#[test]
fn whitespace_is_tolerated_between_tokens() {
    let list =
        List::from_elements(TagType::Int, vec![Tag::Int(2), Tag::Int(3)]).expect("homogeneous");
    let mut compound = Compound::new();
    compound.insert("a", Tag::Int(1));
    compound.insert("b", Tag::List(list));

    assert_eq!(
        snbt::parse(" {  a : 1 , b : [ 2 , 3 ] } "),
        Ok(Tag::Compound(compound))
    );
}

// ---------------------------------------------------------------------------
// Strings, quoting, escaping
// ---------------------------------------------------------------------------

#[test]
fn quoted_strings_choose_and_escape_their_quote() {
    // The quote character follows the first quote-ish character in the
    // string; only the chosen quote and backslash ever escape.
    assert_eq!(
        snbt::parse(r#""plain""#),
        Ok(Tag::String("plain".to_owned()))
    );
    assert_eq!(
        snbt::parse("'he said \"hi\"'"),
        Ok(Tag::String("he said \"hi\"".to_owned())),
        "the other quote needs no escape inside"
    );
    assert_eq!(
        snbt::parse(r#""back\\slash""#),
        Ok(Tag::String("back\\slash".to_owned())),
        "backslash escapes in either flavour"
    );

    // And the printer picks by the same rule, so both flavours come back out.
    assert_eq!(
        snbt::to_string(&Tag::String("he said \"hi\"".to_owned())),
        "'he said \"hi\"'"
    );
    assert_eq!(
        snbt::to_string(&Tag::String("it's".to_owned())),
        "\"it's\"",
        "a lone apostrophe flips the wrapper to double quotes"
    );
    assert_eq!(
        snbt::to_string(&Tag::String("\"and 'both'".to_owned())),
        "'\"and \\'both\\''",
        "only the chosen quote is escaped, and only when met again"
    );

    // The full circle on the hardest case: both quotes in one string.
    let text = "\"and 'both'".to_owned();
    let printed = snbt::to_string(&Tag::String(text.clone()));
    assert_eq!(snbt::parse(&printed).expect("re-parses"), Tag::String(text));
}

#[test]
fn control_characters_pass_through_verbatim_in_both_directions() {
    // Brigadier has no escape for them, so the printer does not invent one;
    // a real newline inside quotes is accepted verbatim and comes back out.
    let text = "line\nbreak\tand\u{0001}bell".to_owned();
    let printed = snbt::to_string(&Tag::String(text.clone()));
    assert_eq!(
        snbt::parse(&printed).expect("re-parses"),
        Tag::String(text),
        "printed as {printed:?}"
    );
}

#[test]
fn unquoted_strings_stop_at_the_first_character_outside_brigadiers_alphabet() {
    // The alphabet is alphanumerics plus _ - . + — Brigadier's
    // StringReader.isAllowedInUnquotedString, byte for byte.
    assert_eq!(
        snbt::parse("hello_world-2.txt+1"),
        Ok(Tag::String("hello_world-2.txt+1".to_owned()))
    );

    // A colon ends the word, and what follows cannot continue any value,
    // which is why ids must be quoted as values even though they appear
    // unquoted inside item arguments everywhere else.
    let error = snbt::parse("minecraft:stone").expect_err("the colon ends the word");
    assert_eq!(error.offset, 9);
    assert!(matches!(error.found, Some(':')));

    assert_eq!(
        snbt::parse("'minecraft:stone'"),
        Ok(Tag::String("minecraft:stone".to_owned()))
    );
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

#[test]
fn keys_follow_the_same_quoting_rules_as_values() {
    assert_eq!(
        snbt::parse("{simple_key-1:0}"),
        Ok(one_field("simple_key-1", Tag::Int(0)))
    );
    assert_eq!(
        snbt::parse("{'minecraft:id':0}"),
        Ok(one_field("minecraft:id", Tag::Int(0))),
        "a colon forces the key into quotes"
    );
    // The printer quotes a key against `[A-Za-z0-9._+-]+`, so this parses
    // back to the same key.
    assert_eq!(
        snbt::parse(&snbt::to_string(&one_field("weird key:!", Tag::Byte(3))))
            .expect("round-trips"),
        one_field("weird key:!", Tag::Byte(3))
    );
}

#[test]
fn an_empty_compound_key_prints_but_does_not_re_parse() {
    use dust_nbt::snbt::{Expected, ParseError};

    // Vanilla's readKey refuses an empty key with argument.nbt.expected.key,
    // and so does this parser — whatever spelling produces one.
    assert_eq!(
        snbt::parse("{\"\":0}"),
        Err(ParseError {
            offset: 1,
            expected: Expected::Key,
            found: Some('"'),
        })
    );
    assert_eq!(
        snbt::parse("{,}").expect_err("no key here at all").expected,
        Expected::Value,
        "a stray comma never gets as far as the key check: the word reader refuses first"
    );

    // Which means a compound carrying an empty key — legal in binary NBT, and
    // produced by real files — prints as {"":...} and cannot come back. This
    // is the documented hole, asserted so it cannot become a surprise.
    let printed = snbt::to_string(&one_field("", Tag::Byte(0)));
    assert_eq!(printed, "{\"\":0b}");
    assert!(snbt::parse(&printed).is_err());
}

// ---------------------------------------------------------------------------
// Errors carry offsets
// ---------------------------------------------------------------------------

#[test]
fn errors_name_the_byte_and_the_expectation() {
    use dust_nbt::snbt::{Expected, ParseError};

    let error = snbt::parse("{a:1,").expect_err("ends mid-compound");
    assert_eq!(
        error,
        ParseError {
            offset: 5,
            expected: Expected::Value,
            found: None,
        },
        "after the comma there is nothing left to be a value"
    );

    let error = snbt::parse("{a 1}").expect_err("no colon");
    assert_eq!(error.offset, 3);
    assert_eq!(error.expected, Expected::Char(':'));
    assert_eq!(error.found, Some('1'));

    let error = snbt::parse("{} tail").expect_err("trailing text");
    assert_eq!(error.offset, 3);
    assert_eq!(error.expected, Expected::EndOfInput);

    let error = snbt::parse("").expect_err("nothing at all");
    assert_eq!(error.offset, 0);
    assert_eq!(error.expected, Expected::Value);

    let error = snbt::parse(r#""\q""#).expect_err("not an escape Brigadier knows");
    assert_eq!(error.offset, 2);
    assert_eq!(error.expected, Expected::ValidEscape);
    assert_eq!(error.found, Some('q'));

    let error = snbt::parse("'never closed").expect_err("unterminated");
    assert_eq!(error.offset, 13);
    assert_eq!(error.expected, Expected::ClosingQuote('\''));
    assert_eq!(error.found, None);
}

#[test]
fn parse_compound_demands_a_compound() {
    assert!(snbt::parse_compound("{a:0b}").is_ok());
    assert!(snbt::parse_compound("[1]").is_err());
    assert!(snbt::parse_compound("1").is_err());
}

#[test]
fn named_printing_labels_the_document_the_way_data_get_does() {
    let mut compound = Compound::new();
    compound.insert("Count", Tag::Byte(2));

    // A name is printed as a key in front of the value.
    assert_eq!(
        snbt::to_string_named("item", &Tag::Compound(compound.clone())),
        "item:{Count:2b}"
    );
    // An awkward name gets quoted like any key.
    assert_eq!(
        snbt::to_string_named("odd name", &Tag::Byte(1)),
        "\"odd name\":1b"
    );
    // The empty name prints nothing before the value — not even a colon —
    // which is what makes a round trip through to_string lossless for the
    // empty root names every vanilla file carries.
    assert_eq!(snbt::to_string_named("", &Tag::Byte(5)), "5b");
}

// ---------------------------------------------------------------------------
// The Java presentation profile
//
// `PrintProfile::JAVA` reproduces `Double.toString`/`Float.toString` *shapes*.
// Each golden below was worked out from the JDK's documented rules — decimal
// form exactly within [10^-3, 10^7), upper case `E`, no `+`, no padding, and
// always a fractional digit — and cross-checked against the values those
// rules are famous for. What the profile approximates rather than reproduces
// (the choice of shortest digits at the subnormal edge) is on
// `NumericStyle`'s doc comment, asserted further down.
// ---------------------------------------------------------------------------

/// Doubles whose Java spellings are pinned, including both threshold
/// neighbours: 9999999 stays decimal while 10^7 flips to scientific, 0.001
/// stays decimal while anything smaller flips.
#[test]
fn the_java_profile_shapes_doubles_like_double_tostring() {
    let cases: &[(f64, &str)] = &[
        (0.0, "0.0d"),
        (-0.0, "-0.0d"),
        (1.0, "1.0d"),
        (-1.5, "-1.5d"),
        (100.0, "100.0d"),
        (123.456, "123.456d"),
        (9999999.0, "9999999.0d"),
        (1.0e7, "1.0E7d"),
        (5.9999968e7, "5.9999968E7d"),
        (1.0e23, "1.0E23d"),
        (0.001, "0.001d"),
        (9.999e-4, "9.999E-4d"),
        (-2.5e-12, "-2.5E-12d"),
        (f64::MAX, "1.7976931348623157E308d"),
        (f64::MIN_POSITIVE, "2.2250738585072014E-308d"),
        (0.1 + 0.2, "0.30000000000000004d"),
    ];
    for (value, expected) in cases {
        assert_eq!(
            snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Double(*value)),
            *expected,
            "{value}e0 shaped wrong"
        );
    }
}

/// The float path keeps `f32` precision throughout: the digits are the
/// shortest that round-trip through an `f32`, never the promoted double's,
/// with the same 10^-3/10^7 window `Float.toString` applies.
#[test]
fn the_java_profile_shapes_floats_like_float_tostring() {
    let cases: &[(f32, &str)] = &[
        (1.0, "1.0f"),
        (0.5, "0.5f"),
        (16777216.0, "1.6777216E7f"),
        (3.4028235e38, "3.4028235E38f"),
        (1.1754944e-38, "1.1754944E-38f"),
        (-0.03043, "-0.03043f"),
        (9999999.0, "9999999.0f"),
    ];
    for (value, expected) in cases {
        assert_eq!(
            snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Float(*value)),
            *expected,
            "{value}e0 shaped wrong"
        );
    }
}

/// Only the floating shapes change. Integers stay bare, the byte/short/long
/// suffixes keep their letters and case, arrays are untouched, and the
/// default profile still answers for every tag exactly as it always did.
#[test]
fn the_java_profile_leaves_every_non_float_decision_alone() {
    let mut compound = Compound::new();
    compound.insert("b", Tag::Byte(-1));
    compound.insert("s", Tag::Short(2));
    compound.insert("i", Tag::Int(-3));
    compound.insert("l", Tag::Long(i64::MAX));
    compound.insert("a", Tag::IntArray(vec![1, -2]));
    compound.insert("text", Tag::String("he said \"hi\"".to_owned()));

    assert_eq!(
        snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Compound(compound)),
        "{b:-1b,s:2s,i:-3,l:9223372036854775807L,a:[I;1,-2],text:'he said \"hi\"'}"
    );

    // And the default profile is unchanged by the plumbing around it: same
    // output as before the profile existed, exponent-free and shortest.
    assert_eq!(snbt::to_string(&Tag::Double(5.9999968e7)), "59999968d");
    assert_eq!(snbt::to_string(&Tag::Float(0.5)), "0.5f");
    assert_eq!(
        snbt::to_string_with(snbt::PrintProfile::default(), &Tag::Int(-3)),
        "-3"
    );
}

mod java_roundtrip {
    //! The differential half of the profile: whatever the Java shapes print
    //! must read back as the bits that were printed. This is the property
    //! that makes the approximation honest — the spelling may differ from a
    //! JDK literal at the subnormal edge, but no value survives printing as
    //! something that parses to different bits.

    use crate::support::{any_finite_double, any_finite_float};
    use proptest::prelude::*;

    use dust_nbt::{snbt, Tag};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn java_shaped_doubles_re_parse_to_the_same_bits(value in any_finite_double()) {
            let printed = snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Double(value));
            let parsed = snbt::parse(&printed).expect("our own output parses");
            prop_assert_eq!(parsed, Tag::Double(value), "printed as {}", printed);
        }

        #[test]
        fn java_shaped_floats_re_parse_to_the_same_bits(value in any_finite_float()) {
            let printed = snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Float(value));
            let parsed = snbt::parse(&printed).expect("our own output parses");
            prop_assert_eq!(parsed, Tag::Float(value), "printed as {}", printed);
        }
    }
}

/// The documented subnormal divergence, pinned so it cannot drift silently in
/// either direction: Rust's shortest spelling has one significant digit where
/// the JDK prints two (`4.9E-324`). Both parse to the same bit pattern — that
/// is the property the suite above guards — and this test names the one place
/// a byte-diff against JDK output would notice.
#[test]
fn the_subnormal_edge_is_where_java_digits_are_approximated() {
    let min_subnormal = f64::from_bits(1);
    let printed = snbt::to_string_with(snbt::PrintProfile::JAVA, &Tag::Double(min_subnormal));
    assert_eq!(printed, "5.0E-324d", "Rust picks the single-digit spelling");
    assert_ne!(
        printed, "4.9E-324d",
        "the JDK's two-digit choice is the documented divergence"
    );
    assert_eq!(
        snbt::parse(&printed).expect("parses"),
        Tag::Double(min_subnormal),
        "the approximation never changes the bits"
    );
}
