//! The text component this server actually builds, and the subset of NBT it
//! needs to put one on the wire.
//!
//! # Why a second component type, next to the opaque one
//!
//! [`crate::nbt::TextComponent`] carries a component as the bytes it arrived
//! as, which is exactly right for a field this crate only has to delimit. It
//! is useless for a message this crate has to *write*: a server that cannot
//! say "this text is red" without hand-assembling binary tags is a server that
//! will get the tags wrong. So there are two types with two jobs —
//! `TextComponent` for fields Dust merely forwards, and this [`Component`]
//! for fields Dust authors (system messages, disconnect reasons, chat
//! formatting). Both travel as network NBT; they differ in who reads them.
//!
//! # The subset, stated plainly
//!
//! A component here is a body — plain text or a translation key with an
//! optional fallback — plus a style of color and bold/italic, plus an `extra`
//! array of further components. That is what chat and system messages need,
//! and it is all this type does. Click events, hover events, keybinds,
//! scores, selectors, selectors-in-translation-arguments and the rest arrive
//! with the datapack work, and nothing about the wire encoding below changes
//! when they do: they are more keys inside the same compound.
//!
//! The decode side refuses rather than approximates. An unknown key means the
//! sender used something outside the subset, and silently dropping it would
//! render a different message than was sent while every test stayed green —
//! the failure mode this whole crate exists to prevent. The refusal names the
//! key, which makes the day the subset grows a one-line diff.
//!
//! # The encoding trap this module owns
//!
//! NBT strings are **Java modified UTF-8**, not UTF-8. They differ in two
//! places: a NUL byte is written as `C0 80` rather than `00`, and characters
//! outside the Basic Multilingual Plane — an emoji, in practice — are written
//! as a surrogate pair encoded CESU-8 style, six bytes where UTF-8 uses four.
//! A writer that emits plain UTF-8 passes every ASCII test and then sends a
//! chat message the client's reader mangles the moment somebody posts an
//! emoji, which is not a corner case in this genre. The two functions at the
//! bottom of this file are the whole of the fix, and they exist here because
//! `dust-nbt`, which will own them properly, does not exist yet.
//!
//! Like everything else that predates `dust-nbt`, this is marked for deletion:
//! the parser below should become a thin wrapper over `dust-nbt`'s reader, and
//! the conformance vectors in `crate::conformance` are what will prove the
//! swap changed nothing.

use std::fmt;

use crate::nbt;
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::types::{Decode, Encode};
use crate::ProtocolVersion;

const TAG_END: u8 = 0;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_BYTE: u8 = 1;

/// How deep a component may nest before both directions give up.
///
/// The same bound [`nbt::scan`] applies, and for the same reason: `decode`
/// recurses on attacker-reachable bytes, a stack overflow in Rust is an abort
/// no caller can catch, and the limit is the entire defence.
const MAX_DEPTH: u32 = nbt::MAX_DEPTH;

/// One node of a formatted message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    pub body: Body,
    pub style: Style,
    pub extra: Vec<Component>,
}

impl Component {
    /// Plain text with no styling.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            body: Body::Text(content.into()),
            ..Self::default()
        }
    }

    /// A translation key, looked up in the client's language file.
    ///
    /// The fallback is what the client shows when it has no entry for the key.
    /// Omitting it is correct only for keys vanilla ships.
    pub fn translate(key: impl Into<String>, fallback: Option<String>) -> Self {
        Self {
            body: Body::Translate {
                key: key.into(),
                fallback,
            },
            ..Self::default()
        }
    }

    /// Set the color, replacing whatever was there.
    pub fn colored(mut self, color: Color) -> Self {
        self.style.color = Some(color);
        self
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.style.bold = Some(bold);
        self
    }

    pub fn italic(mut self, italic: bool) -> Self {
        self.style.italic = Some(italic);
        self
    }

    /// Attach children, which inherit this node's style when rendered.
    pub fn with_extra(mut self, extra: Vec<Component>) -> Self {
        self.extra = extra;
        self
    }
}

/// What a component says, before styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Literal text, shown as written.
    Text(String),
    /// A key into the client's translation files.
    Translate {
        key: String,
        /// Shown when the client knows no such key.
        fallback: Option<String>,
    },
}

impl Default for Body {
    // The default body is empty text rather than something cleverer: a
    // component whose body nobody set is a styling container for its `extra`
    // list, and "no text" is precisely what it means.
    fn default() -> Self {
        Self::Text(String::new())
    }
}

/// Color, weight and slant, each independently absent.
///
/// Absent means *inherit*, not *off*: a child with no opinion about boldness
/// takes its parent's. Encoding `false` when nothing was said would break that
/// chain, which is why these are `Option`s and not `bool`s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Style {
    pub color: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

/// A text color: one of the sixteen vanilla palette entries or an RGB triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Named(NamedColor),
    /// Written as `#rrggbb`, which is how 1.16+ spells it.
    Rgb(u32),
}

impl Color {
    const NAMES: [(&str, NamedColor); 16] = [
        ("black", NamedColor::Black),
        ("dark_blue", NamedColor::DarkBlue),
        ("dark_green", NamedColor::DarkGreen),
        ("dark_aqua", NamedColor::DarkAqua),
        ("dark_red", NamedColor::DarkRed),
        ("dark_purple", NamedColor::DarkPurple),
        ("gold", NamedColor::Gold),
        ("gray", NamedColor::Gray),
        ("dark_gray", NamedColor::DarkGray),
        ("blue", NamedColor::Blue),
        ("green", NamedColor::Green),
        ("aqua", NamedColor::Aqua),
        ("red", NamedColor::Red),
        ("light_purple", NamedColor::LightPurple),
        ("yellow", NamedColor::Yellow),
        ("white", NamedColor::White),
    ];

    /// Parse the spelling a component carries on the wire.
    ///
    /// Exact lowercase, as the format requires: `Red` is not a color, and
    /// accepting it would write a value the client renders black.
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(hex) = text.strip_prefix('#') {
            let value = u32::from_str_radix(hex, 16).ok()?;
            return (hex.len() == 6).then_some(Self::Rgb(value));
        }
        Self::NAMES
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, color)| Self::Named(*color))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(color) => f.write_str(color.spelling()),
            Self::Rgb(value) => write!(f, "#{:06x}", value),
        }
    }
}

/// The sixteen colors the client has had words for since before components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

impl NamedColor {
    pub fn spelling(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::DarkBlue => "dark_blue",
            Self::DarkGreen => "dark_green",
            Self::DarkAqua => "dark_aqua",
            Self::DarkRed => "dark_red",
            Self::DarkPurple => "dark_purple",
            Self::Gold => "gold",
            Self::Gray => "gray",
            Self::DarkGray => "dark_gray",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Aqua => "aqua",
            Self::Red => "red",
            Self::LightPurple => "light_purple",
            Self::Yellow => "yellow",
            Self::White => "white",
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// A bare string is the whole component when nothing else is set.
///
/// The compact form exists on the wire and vanilla uses it everywhere it can;
/// a writer that always emitted compounds would be correct and four times the
/// size for every plain sentence this server sends. The rule is exact: bare
/// string iff the body is text, the style is empty, and there are no extras.
fn is_bare_string(component: &Component) -> bool {
    matches!(component.body, Body::Text(_))
        && component.style == Style::default()
        && component.extra.is_empty()
}

impl Encode for Component {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        encode_at_depth(self, out, 0)
    }
}

fn encode_at_depth<W: WireWrite + ?Sized>(
    component: &Component,
    out: &mut W,
    depth: u32,
) -> Result<(), EncodeError> {
    if depth > MAX_DEPTH {
        return Err(EncodeError::Unsupported {
            field: "text component",
            why: "the tree nests deeper than the decoder will read, so this value could never \
                  round-trip",
        });
    }
    if is_bare_string(component) {
        let Body::Text(text) = &component.body else {
            unreachable!("checked by `is_bare_string`");
        };
        write_tag(out, TAG_STRING);
        write_nbt_string(out, text);
        return Ok(());
    }

    write_tag(out, TAG_COMPOUND);
    // An NBT compound entry is the **type byte first**, then the name, then
    // the payload — the opposite order to how one reads a map, and the kind
    // of thing the round trips exist to catch. Every branch below follows it.
    match &component.body {
        Body::Text(text) => {
            if !text.is_empty() {
                write_tag(out, TAG_STRING);
                write_key(out, "text");
                write_nbt_string(out, text);
            }
        }
        Body::Translate { key, fallback } => {
            write_tag(out, TAG_STRING);
            write_key(out, "translate");
            write_nbt_string(out, key);
            if let Some(fallback) = fallback {
                write_tag(out, TAG_STRING);
                write_key(out, "fallback");
                write_nbt_string(out, fallback);
            }
        }
    }
    if let Some(color) = component.style.color {
        write_tag(out, TAG_STRING);
        write_key(out, "color");
        write_nbt_string(out, &color.to_string());
    }
    for (key, flag) in [("bold", component.style.bold), ("italic", component.style.italic)] {
        if let Some(value) = flag {
            write_tag(out, TAG_BYTE);
            write_key(out, key);
            out.write_u8(u8::from(value));
        }
    }
    if !component.extra.is_empty() {
        let count = i32::try_from(component.extra.len()).map_err(|_| EncodeError::TooManyElements {
            count: component.extra.len(),
        })?;
        write_key(out, "extra");
        write_tag(out, TAG_LIST);
        write_tag(out, TAG_COMPOUND);
        out.write_var_int(count);
        for child in &component.extra {
            encode_at_depth(child, out, depth + 1)?;
        }
    }
    write_tag(out, TAG_END);
    Ok(())
}

fn write_tag<W: WireWrite + ?Sized>(out: &mut W, tag: u8) {
    out.write_u8(tag);
}

fn write_key<W: WireWrite + ?Sized>(out: &mut W, key: &str) {
    write_nbt_string(out, key);
}

fn write_nbt_string<W: WireWrite + ?Sized>(out: &mut W, text: &str) {
    let encoded = to_modified_utf8(text);
    out.write_u16(encoded.len() as u16);
    out.write_slice(&encoded);
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

impl Decode for Component {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        // Delimit first, via the scanner this crate already trusts, then parse
        // within the slice. Parsing from the live cursor directly would work
        // until a malformed value left the cursor somewhere undefined; taking
        // the length first means a refused value consumes nothing at all.
        let len = nbt::scan(input.peek())?;
        let bytes = input.read_vec(len)?;
        let mut value = ReaderAtEnd { bytes: &bytes };
        let component = read_component(&mut value, 0)?;
        if !value.bytes.is_empty() {
            return Err(DecodeError::Nbt {
                why: "a component ended before its bytes did",
            });
        }
        Ok(component)
    }
}

struct ReaderAtEnd<'a> {
    bytes: &'a [u8],
}

impl ReaderAtEnd<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], DecodeError> {
        if len > self.bytes.len() {
            return Err(DecodeError::Nbt {
                why: "a string ran past the end of the value",
            });
        }
        let (head, tail) = self.bytes.split_at(len);
        self.bytes = tail;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
}

fn read_component(reader: &mut ReaderAtEnd<'_>, depth: u32) -> Result<Component, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError::Nbt {
            why: "a component nests deeper than the limit",
        });
    }
    let tag = reader.byte()?;
    match tag {
        TAG_END => Err(DecodeError::Nbt {
            why: "a bare TAG_End is not a component",
        }),
        TAG_STRING => Ok(Component::text(read_nbt_string(reader)?)),
        TAG_COMPOUND => read_compound(reader, depth),
        _ => Err(DecodeError::Unsupported {
            field: "text component",
            why: "only a bare string or a compound is accepted; this tag is neither",
        }),
    }
}

fn read_compound(
    reader: &mut ReaderAtEnd<'_>,
    depth: u32,
) -> Result<Component, DecodeError> {
    let mut body = Body::default();
    let mut style = Style::default();
    let mut extra: Vec<Component> = Vec::new();
    loop {
        let entry = reader.byte()?;
        if entry == TAG_END {
            break;
        }
        let key = read_nbt_string(reader)?;
        match (key.as_str(), entry) {
            ("text", TAG_STRING) => body = Body::Text(read_nbt_string(reader)?),
            ("translate", TAG_STRING) => body = Body::Translate {
                key: read_nbt_string(reader)?,
                fallback: None,
            },
            ("fallback", TAG_STRING) => {
                let fallback = read_nbt_string(reader)?;
                match &mut body {
                    Body::Translate { fallback: slot, .. } => *slot = Some(fallback),
                    _ => {
                        return Err(DecodeError::Nbt {
                            why: "`fallback` without `translate` says nothing",
                        })
                    }
                }
            }
            ("color", TAG_STRING) => {
                let spelling = read_nbt_string(reader)?;
                style.color = Some(Color::parse(&spelling).ok_or_else(|| DecodeError::Nbt {
                    why: "the color name is not one the client knows",
                })?);
            }
            ("bold", TAG_BYTE) | ("italic", TAG_BYTE) => {
                // A boolean flag in NBT is a byte, and vanilla writes 0 or 1.
                // Anything non-zero meaning true is the reader Java itself
                // gets for free, so matching it is not laxness.
                let value = reader.byte()?;
                let flag = value != 0;
                if key == "bold" {
                    style.bold = Some(flag);
                } else {
                    style.italic = Some(flag);
                }
            }
            ("extra", TAG_LIST) => {
                let element = reader.byte()?;
                if element != TAG_COMPOUND {
                    return Err(DecodeError::Unsupported {
                        field: "text component extra",
                        why: "children must be compounds; this list holds something else",
                    });
                }
                let count = read_var_int_len(reader)?;
                extra.reserve(count.min(reader.bytes.len()));
                for _ in 0..count {
                    extra.push(read_component(reader, depth + 1)?);
                }
            }
            (key, _) => {
                return Err(DecodeError::UnknownField {
                    container: "text component",
                    key: key.to_owned(),
                });
            }
        }
    }
    Ok(Component { body, style, extra })
}

fn read_var_int_len(reader: &mut ReaderAtEnd<'_>) -> Result<usize, DecodeError> {
    let mut value: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = reader.byte()?;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return usize::try_from(value).map_err(|_| DecodeError::Nbt {
                why: "a list length does not fit the address space",
            });
        }
        shift += 7;
        if shift >= 32 {
            return Err(DecodeError::VarIntTooLong { bits: 32 });
        }
    }
}

fn read_nbt_string(reader: &mut ReaderAtEnd<'_>) -> Result<String, DecodeError> {
    let len = usize::from(u16::from_be_bytes([reader.byte()?, reader.byte()?]));
    let bytes = reader.take(len)?;
    from_modified_utf8(bytes).ok_or(DecodeError::NotUtf8)
}

// ---------------------------------------------------------------------------
// Modified UTF-8, the part dust-nbt deletes
// ---------------------------------------------------------------------------

/// Encode as Java modified UTF-8: NUL becomes `C0 80`, astral code points go
/// as a surrogate pair in CESU-8 form.
fn to_modified_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for unit in text.encode_utf16() {
        if unit == 0 {
            out.extend_from_slice(&[0xC0, 0x80]);
        } else if unit < 0x80 {
            out.push(unit as u8);
        } else if unit < 0x800 {
            out.push(0xC0 | (unit >> 6) as u8);
            out.push(0x80 | (unit & 0x3F) as u8);
        } else {
            out.push(0xE0 | (unit >> 12) as u8);
            out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
            out.push(0x80 | (unit & 0x3F) as u8);
        }
    }
    out
}

/// The inverse. `None` on any sequence a JVM reader would reject: bad leads,
/// bad continuations, unpaired surrogates, encodings of zero other than
/// `C0 80`.
fn from_modified_utf8(bytes: &[u8]) -> Option<String> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let first = bytes[at];
        let unit = match first {
            0x01..=0x7F => {
                at += 1;
                u16::from(first)
            }
            0xC0..=0xDF => {
                let second = *bytes.get(at + 1)?;
                if second & 0xC0 != 0x80 {
                    return None;
                }
                at += 2;
                (u16::from(first & 0x1F) << 6) | u16::from(second & 0x3F)
            }
            0xE0..=0xEF => {
                let second = *bytes.get(at + 1)?;
                let third = *bytes.get(at + 2)?;
                if second & 0xC0 != 0x80 || third & 0xC0 != 0x80 {
                    return None;
                }
                at += 3;
                (u16::from(first & 0x0F) << 12)
                    | (u16::from(second & 0x3F) << 6)
                    | u16::from(third & 0x3F)
            }
            // Single-byte NUL, continuation bytes as leads, four-byte UTF-8:
            // none of these are modified UTF-8.
            _ => return None,
        };
        units.push(unit);
    }

    // Reject the surrogates that were never paired before rebuilding the
    // string; `String::from_utf16` would reject them anyway, but the paired
    // case is what makes the emoji path worth having walked through by hand.
    String::from_utf16(&units).ok()
}
