//! The SNBT printer.
//!
//! Compact, and shaped after `net.minecraft.nbt.StringTagVisitor` — the visitor
//! behind `CompoundTag.toString()` — rather than after
//! `SnbtPrinterTagVisitor`, the indented one `/data get` uses. The compact form
//! is what a document has to be in to be pasted back into a command, and the
//! indented form is a presentation of it.
//!
//! Every rule below was read out of `StringTagVisitor` in the 1.21.1 server
//! jar, because they are exactly the things that are easy to get plausibly
//! wrong:
//!
//! * The suffixes are `b`, `s`, nothing, `L`, `f`, `d` — lower case except the
//!   long, which is upper.
//! * A `TAG_Byte_Array` prints its elements with an **upper case** `B`:
//!   `[B;1B,2B]`. A `TAG_Long_Array` uses `L`, matching the scalar. A
//!   `TAG_Int_Array` uses no suffix at all.
//! * A key is left unquoted when it matches `[A-Za-z0-9._+-]+` — the pattern is
//!   a string constant in the class — and double-quoted otherwise.
//! * A string value picks its quote character by the rule in
//!   `StringTag.quoteAndEscape`, reproduced in [`quote_and_escape`].
//!
//! What it does **not** reproduce is Java's float formatting; see the module
//! documentation for `snbt`.

use std::fmt::Write as _;

use crate::tag::{Compound, List, Tag};

/// Print a tag as compact SNBT.
///
/// ```
/// use dust_nbt::{snbt, Compound, Tag};
///
/// let mut compound = Compound::new();
/// compound.insert("Count", Tag::Byte(1));
/// compound.insert("id", Tag::String("minecraft:stone".to_owned()));
/// assert_eq!(
///     snbt::to_string(&Tag::Compound(compound)),
///     "{Count:1b,id:\"minecraft:stone\"}"
/// );
/// ```
pub fn to_string(tag: &Tag) -> String {
    let mut out = String::new();
    write_tag(&mut out, tag);
    out
}

/// Print a tag with a name in front of it, the way `/data get` labels a result.
///
/// Included because the file form of the binary encoding has a root name and
/// SNBT has nowhere to put one, so a caller converting between the two needs
/// somewhere for it to go that is not silently the bin.
pub fn to_string_named(name: &str, tag: &Tag) -> String {
    let mut out = String::new();
    if !name.is_empty() {
        write_key(&mut out, name);
        out.push(':');
    }
    write_tag(&mut out, tag);
    out
}

fn write_tag(out: &mut String, tag: &Tag) {
    match tag {
        Tag::Byte(v) => {
            let _ = write!(out, "{v}b");
        }
        Tag::Short(v) => {
            let _ = write!(out, "{v}s");
        }
        Tag::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Tag::Long(v) => {
            let _ = write!(out, "{v}L");
        }
        Tag::Float(v) => {
            if !write_non_finite(out, f64::from(*v)) {
                // Formatted as an `f32`, not promoted: `0.1f32` promoted to
                // `f64` is 0.10000000149011612, and printing that is both
                // hideous and unlike vanilla. Rust's `{}` for an `f32` gives
                // the shortest decimal that reads back as the same `f32`.
                let _ = write!(out, "{v}");
            }
            out.push('f');
        }
        Tag::Double(v) => {
            if !write_non_finite(out, *v) {
                let _ = write!(out, "{v}");
            }
            out.push('d');
        }
        Tag::ByteArray(values) => {
            out.push_str("[B;");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                // Upper case, which is what `StringTagVisitor` appends and
                // which the byte pattern accepts because it is compiled
                // case-insensitively.
                let _ = write!(out, "{value}B");
            }
            out.push(']');
        }
        Tag::String(text) => quote_and_escape(out, text),
        Tag::List(list) => write_list(out, list),
        Tag::Compound(compound) => write_compound(out, compound),
        Tag::IntArray(values) => {
            out.push_str("[I;");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{value}");
            }
            out.push(']');
        }
        Tag::LongArray(values) => {
            out.push_str("[L;");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{value}L");
            }
            out.push(']');
        }
    }
}

/// The three values that have no SNBT syntax, printed the way Java prints
/// them; `true` if this was one of them.
///
/// Rust's `{}` gives the shortest decimal that round-trips, which is what is
/// wanted, and it is allowed to give an integer-looking one — `1` for
/// `1.0f32`. That is fine on its own, because the suffixed float and double
/// patterns both accept a bare integer: `1f` parses as a float.
///
/// `inf` is not fine. Nothing in the grammar accepts it, and `inff` would read
/// back as a *string*. Java has exactly the same hole and prints `Infinity`,
/// which its own parser also reads back as a string, so this prints what Java
/// prints rather than inventing a spelling vanilla would reject either way.
/// See the `snbt` module documentation, and `tests/snbt.rs` for the test that
/// pins the lossiness in place so it cannot be discovered by accident.
///
/// Unlike Java this never uses exponent notation, so a very large double prints
/// as a great many digits. Both forms read back as the same number.
fn write_non_finite(out: &mut String, value: f64) -> bool {
    if value.is_nan() {
        out.push_str("NaN");
    } else if value == f64::INFINITY {
        out.push_str("Infinity");
    } else if value == f64::NEG_INFINITY {
        out.push_str("-Infinity");
    } else {
        return false;
    }
    true
}

fn write_list(out: &mut String, list: &List) {
    out.push('[');
    for (index, element) in list.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_tag(out, element);
    }
    out.push(']');
}

fn write_compound(out: &mut String, compound: &Compound) {
    out.push('{');
    for (index, (name, value)) in compound.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_key(out, name);
        out.push(':');
        write_tag(out, value);
    }
    out.push('}');
}

/// A key: bare when it can be, double-quoted when it cannot.
///
/// `StringTagVisitor` tests the key against `[A-Za-z0-9._+-]+` — a `Pattern`
/// constant in the class — and quotes it if it does not match entirely. Note
/// that the pattern excludes `:`, so a key like `minecraft:custom` is quoted,
/// and that an *empty* key does not match `+` and is therefore quoted as `""`,
/// which is what keeps a compound with an empty key printable.
fn write_key(out: &mut String, key: &str) {
    if !key.is_empty() && key.chars().all(is_simple_key_char) {
        out.push_str(key);
    } else {
        quote_and_escape(out, key);
    }
}

fn is_simple_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '+' || c == '-'
}

/// `StringTag.quoteAndEscape`, reproduced.
///
/// The rule reads oddly until it is written out. The quote character is chosen
/// by the **first** quote-ish character in the string: if that is a `"`, the
/// string is wrapped in `'`; if it is a `'`, the string is wrapped in `"`; if
/// there is none, `"`. Only the chosen quote is then escaped, and `\` is always
/// escaped. So `he said "hi"` prints as `'he said "hi"'` with nothing escaped
/// at all, and only a string containing both quote characters ever needs an
/// escape for one of them.
///
/// Nothing else is escaped — not a newline, not a tab, not a control
/// character. Brigadier's reader has no escape for them either, so escaping
/// them would produce something its own parser rejects. A tag containing a
/// newline prints with a literal newline in the middle of it, and round-trips.
fn quote_and_escape(out: &mut String, text: &str) {
    let mut quote = None;
    for c in text.chars() {
        if c == '"' || c == '\'' {
            quote = Some(if c == '"' { '\'' } else { '"' });
            break;
        }
    }
    let quote = quote.unwrap_or('"');
    out.push(quote);
    for c in text.chars() {
        if c == '\\' || c == quote {
            out.push('\\');
        }
        out.push(c);
    }
    out.push(quote);
}
