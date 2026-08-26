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
