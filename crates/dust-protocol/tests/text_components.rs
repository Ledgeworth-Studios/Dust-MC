//! The text component codec: round trips, the modified UTF-8 traps, and the
//! refusal policy for everything outside the subset.
//!
//! These tests exist because the two failure modes of this module are both
//! silent. A writer that emits plain UTF-8 where the format wants Java
//! modified UTF-8 produces bytes that look fine in ASCII and mangle the first
//! emoji a player types. A decoder that skipped keys it does not model renders
//! a different message than was sent and stays green forever. Both are pinned
//! here, by name.

mod common;

use common::v;
use dust_protocol::text::DEPTH_PER_LEVEL;
use dust_protocol::text::{Body, Color, Component, NamedColor, Style};
use dust_protocol::types::{Decode, Encode};
use dust_protocol::wire::{DecodeError, EncodeError, Reader, Writer};

fn round_trip(component: &Component) -> Vec<u8> {
    let mut writer = Writer::new();
    component.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();
    assert_eq!(
        &Component::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        component,
        "{component:?} changed on the way round"
    );
    bytes
}

#[test]
fn plain_text_travels_as_a_bare_string() {
    // The compact form: no compound, no style tags, one string.
    let bytes = round_trip(&Component::text("hello"));
    // TAG_STRING (0x08), u16 length 5, then the bytes.
    assert_eq!(bytes[0], 0x08);
    assert_eq!(&bytes[1..3], &[0x00, 0x05]);
    assert_eq!(&bytes[3..], b"hello");
}

#[test]
fn styling_or_children_promote_the_value_to_a_compound() {
    let bold = Component::text("hey").bold(true);
    let bytes = round_trip(&bold);
    assert_eq!(bytes[0], 0x0A, "a styled component is a compound");

    let parent = Component::text("").with_extra(vec![Component::text("child")]);
    let bytes = round_trip(&parent);
    assert_eq!(bytes[0], 0x0A, "an extra list is a compound too");
}

#[test]
fn translate_keys_carry_optional_fallbacks() {
    let keyed = Component::translate("chat.type.text", None);
    round_trip(&keyed);

    let with_fallback = Component::translate("dust:greeting", Some("Hello, {name}".to_owned()))
        .colored(Color::Named(NamedColor::Aqua))
        .italic(false);
    let decoded = {
        let mut writer = Writer::new();
        with_fallback.encode(&mut writer, v()).expect("encodes");
        let bytes = writer.into_bytes();
        Component::decode(&mut Reader::new(&bytes), v()).expect("decodes")
    };
    match decoded.body {
        Body::Translate { key, fallback } => {
            assert_eq!(key, "dust:greeting");
            assert_eq!(fallback.as_deref(), Some("Hello, {name}"));
        }
        other => panic!("expected translate, got {other:?}"),
    }
}

#[test]
fn extras_nest_and_inherit_nothing_at_this_layer() {
    // Inheritance is a *rendering* rule the client applies; on the wire each
    // node carries its own style or nothing. The round trip must not invent
    // inheritance by flattening.
    let message = Component {
        body: Body::Text("[Dust] ".to_owned()),
        style: Style {
            color: Some(Color::Named(NamedColor::Gold)),
            ..Style::default()
        },
        extra: vec![
            Component::text("patrick ").italic(true),
            Component::text("joined").colored(Color::Rgb(0x33_66_ff)),
        ],
    };

    let mut writer = Writer::new();
    message.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();
    assert_eq!(
        Component::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        message
    );
}

#[test]
fn colors_accept_the_sixteen_names_and_hex_but_nothing_else() {
    for name in ["red", "dark_purple", "white"] {
        assert!(Color::parse(name).is_some(), "{name}");
    }
    assert_eq!(
        Color::parse("#1a2b3c"),
        Some(Color::Rgb(0x1a_2b_3c)),
        "hex spells back"
    );
    assert_eq!(Color::Named(NamedColor::Red).to_string(), "red");
    assert_eq!(Color::Rgb(0x0a0b0c).to_string(), "#0a0b0c");

    // Case and spelling are exact: the client would render an unknown color
    // black, which is worse than refusing here.
    for bad in ["Red", "#12345", "#gggghi", "", "light purple"] {
        assert!(Color::parse(bad).is_none(), "`{bad}` parsed");
    }
    let unknown = Component::text("x").colored(Color::Named(NamedColor::Blue));
    let _ = unknown; // construction only; parsing is what rejects below
}

// ---------------------------------------------------------------------------
// Modified UTF-8, the part that eats emoji
// ---------------------------------------------------------------------------

#[test]
fn astral_characters_survive_as_modified_utf8_surrogate_pairs() {
    // One emoji is two UTF-16 units and six CESU-8 bytes here, where plain
    // UTF-8 would use four. The wire length prefix counts those six.
    let component = Component::text("hi 😀!");
    let mut writer = Writer::new();
    component.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();

    // "hi " (3) + surrogate pair (3 + 3) + "!" (1) = 10.
    assert_eq!(&bytes[1..3], &[0x00, 0x0a]);
    assert_eq!(
        Component::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        component
    );

    // And a NUL is C0 80, never 00 — which also means the encoded form can
    // never contain a zero byte, something binary parsers rely on.
    let nul = Component::text("a\u{0000}b");
    let mut writer = Writer::new();
    nul.encode(&mut writer, v()).expect("encodes");
    let bytes = writer.into_bytes();
    assert!(!bytes[3..].contains(&0x00), "{bytes:02x?}");
    assert_eq!(
        Component::decode(&mut Reader::new(&bytes), v()).expect("decodes"),
        nul
    );
}

#[test]
fn malformed_strings_are_refused_not_mangled() {
    // Four-byte UTF-8 lead: legal elsewhere, not modified UTF-8.
    let four_byte_utf8 = [0x08, 0x00, 0x04, 0xF0, 0x9F, 0x98, 0x80];
    assert_eq!(
        Component::decode(&mut Reader::new(&four_byte_utf8), v()),
        Err(DecodeError::NotUtf8),
        "the reader speaks modified UTF-8 only"
    );

    // A truncated sequence runs off the value's own length first: the
    // scanner that delimits the component refuses before any parsing starts.
    let truncated = [0x08, 0x00, 0x02, 0xE0];
    assert!(matches!(
        Component::decode(&mut Reader::new(&truncated), v()),
        Err(DecodeError::UnexpectedEnd { .. })
    ));
}

// ---------------------------------------------------------------------------
// Refusals: the subset is closed and says so
// ---------------------------------------------------------------------------

#[test]
fn unknown_keys_name_themselves_instead_of_vanishing() {
    // A click event, hand-assembled to reach the key check without a JSON
    // layer: compound containing "clickEvent" as a string entry. The decode
    // refuses naming the key, because dropping it would render different
    // text behaviour while every byte count stayed correct.
    let mut bytes = vec![0x0A];
    let entry_key = b"clickEvent";
    bytes.extend_from_slice(&[0x08]);
    bytes.extend_from_slice(&(entry_key.len() as u16).to_be_bytes());
    bytes.extend_from_slice(entry_key);
    let value = b"junk";
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value);
    bytes.push(0x00);

    match Component::decode(&mut Reader::new(&bytes), v()) {
        Err(DecodeError::UnknownField { container, key }) => {
            assert_eq!(container, "text component");
            assert_eq!(key, "clickEvent");
        }
        other => panic!("expected a named refusal, got {other:?}"),
    }
}

#[test]
fn a_fallback_without_translate_is_a_lie_about_structure() {
    // "fallback" only means something beside "translate". Accepting it alone
    // would silently drop the fallback text on render.
    let mut bytes = vec![0x0A];
    bytes.extend_from_slice(&[0x08, 0x00, 0x08]);
    bytes.extend_from_slice(b"fallback");
    bytes.extend_from_slice(&[0x00, 0x03]);
    bytes.extend_from_slice(b"abc");
    bytes.push(0x00);
    assert!(matches!(
        Component::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::Nbt { .. })
    ));
}

#[test]
fn nesting_is_bounded_before_it_is_walked() {
    // Each level wraps the last in an `extra` list. Past the limit both
    // directions give up by name rather than overflowing the stack — an abort
    // nobody can catch, reachable from any peer that sends components.
    let depth_limit = dust_protocol::text::MAX_DEPTH;

    let deep = build_nested(depth_limit + 5);
    let mut writer = Writer::new();
    assert!(matches!(
        deep.encode(&mut writer, v()),
        Err(EncodeError::Unsupported { .. })
    ));

    let bytes = encode_nested_raw(depth_limit);
    assert!(matches!(
        Component::decode(&mut Reader::new(&bytes), v()),
        Err(DecodeError::Nbt { .. })
    ));

    // And the positive control: just under what the scanner allows still
    // works both ways. A level costs two of the scanner's budget (compound
    // plus list), which the encoder respects, so the same tree passes or
    // fails in both directions together.
    let shallow = build_nested(depth_limit / DEPTH_PER_LEVEL - 1);
    let mut writer = Writer::new();
    shallow
        .encode(&mut writer, v())
        .expect("just under the limit encodes");
    Component::decode(&mut Reader::new(writer.as_bytes()), v()).expect("and decodes");
}

/// A chain of single-child extras, `levels` deep, built through the public API.
fn build_nested(levels: u32) -> Component {
    let mut node = Component::text("leaf");
    for _ in 0..levels {
        node = Component {
            body: Body::Text(String::new()),
            style: Style::default(),
            extra: vec![node],
        };
    }
    node
}

/// The same shape written straight to NBT bytes, so the decode-side limit is
/// tested independently of the encode-side one.
///
/// Each level is a compound whose only entry is an `extra` list of one child
/// compound. List elements carry no leading tag — the element type is stated
/// once in the list header — which is exactly why the naive version of this
/// builder double-tags every level and decodes into nonsense instead of
/// failing at the limit.
fn encode_nested_raw(levels: u32) -> Vec<u8> {
    fn string_entry(name: &[u8], value: &[u8]) -> Vec<u8> {
        let mut out = vec![0x08];
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value);
        out
    }

    // A compound *payload*: entries plus terminator, no leading tag.
    fn payload(levels_left: u32) -> Vec<u8> {
        let mut out = string_entry(b"text", b"node");
        if levels_left > 0 {
            out.push(0x09); // list
            out.extend_from_slice(&[0x00, 0x05]);
            out.extend_from_slice(b"extra");
            out.push(0x0A); // element type: compound
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // one element
            out.extend(payload(levels_left - 1));
        }
        out.push(0x00); // end of compound
        out
    }

    let mut root = vec![0x0A]; // the root's own tag
    root.extend(payload(levels));
    root
}
