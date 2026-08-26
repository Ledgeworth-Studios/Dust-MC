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
//! What it does **not** reproduce by default is Java's float formatting; see
//! the module documentation for `snbt`. [`PrintProfile::JAVA`] switches the
//! float and double shapes over to Java's, under the terms documented on
//! [`NumericStyle`].

use std::fmt::Write as _;

use crate::tag::{Compound, List, Tag};

/// How a finite float or double is turned into digits.
///
/// The two styles produce different *text* for the same bits and never
/// different bits: both are shortest-round-trip spellings, so either parses
/// back to the value it printed. The choice is purely about which reader is
/// staring at the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumericStyle {
    /// Rust's `{}` formatting: the shortest decimal that reads back as the
    /// same number, spelled out in full — `59999968` for 5.9999968E7, however
    /// many digits that takes. Never exponent notation.
    ///
    /// This is what this printer has always done, and it stays the default:
    /// the output is stable, it re-parses through every SNBT grammar including
    /// ours, and nothing downstream of Dust expects Java's spelling.
    #[default]
    Shortest,
    /// The shapes of Java's `Double.toString` and `Float.toString`, which is
    /// what `StringTagVisitor` prints through: decimal form exactly when the
    /// magnitude sits in [10⁻³, 10⁷), scientific form as `M.MM…E±exp`
    /// otherwise — with an upper case `E`, no `+`, no zero padding on the
    /// exponent — and at least one digit after the point in either shape,
    /// so `1.0E7` rather than `1E7`.
    ///
    /// # Reproduced
    ///
    /// * The decimal/scientific threshold itself: `0.001` stays decimal,
    ///   `9999999.0` stays decimal, `1.0E7` goes scientific.
    /// * The exponent spelling: `5.9999968E7`, not `5.9999968e+07`.
    /// * The forced fraction: `100.0`, `1.0E23` — Java never prints a bare
    ///   integer for a floating-point tag.
    /// * `-0.0` keeps its sign, `NaN`/`Infinity` print as words (unchanged
    ///   from [`NumericStyle::Shortest`]; see `write_non_finite`).
    ///
    /// # Approximated
    ///
    /// The *digits* are Rust's shortest-round-tripping decimals, not the
    /// output of the JDK's Schubfach implementation. Throughout the normal
    /// range the two agree digit for digit — both produce the fewest digits
    /// that round-trip, broken towards the candidate closest to the value —
    /// and the goldens in `tests/snbt.rs` pin the agreement on the values
    /// where presentations historically diverge. At the subnormal edge they
    /// can disagree: the JDK prints `4.9E-324` where the shortest spelling is
    /// `5.0E-324`, because it prefers more digits that sit nearer the value's
    /// neighbourhood than the single digit that round-trips. Both parse to
    /// the identical bit pattern, so the differential property — everything
    /// printed re-reads as what was printed — holds either way; only a byte
    /// comparison against a JDK-produced literal can tell the difference, and
    /// no consumer here makes one.
    Java,
}

/// Presentation choices for the printer.
///
/// Everything the compact printer does except number shaping — quoting,
/// suffixes, array prefixes — is fixed by the format and carries no options;
/// the profile exists so that the one genuinely contested choice has a name
/// and a place to live rather than growing into a boolean parameter list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintProfile {
    /// How finite floats and doubles become text.
    pub numeric: NumericStyle,
}

impl PrintProfile {
    /// Java's presentation, for text aimed at tools that diff or display it
    /// next to vanilla's own output.
    pub const JAVA: Self = Self {
        numeric: NumericStyle::Java,
    };
}

impl Default for PrintProfile {
    /// The printer as it has always behaved. Deliberate: existing output does
    /// not move because a new option appeared next to it.
    fn default() -> Self {
        Self {
            numeric: NumericStyle::Shortest,
        }
    }
}

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
    to_string_with(PrintProfile::default(), tag)
}

/// [`to_string`] under `profile`.
///
/// ```
/// use dust_nbt::{snbt, PrintProfile, Tag};
///
/// let printed = snbt::to_string_with(PrintProfile::JAVA, &Tag::Double(5.9999968e7));
/// assert_eq!(printed, "5.9999968E7d");
/// ```
pub fn to_string_with(profile: PrintProfile, tag: &Tag) -> String {
    let mut out = String::new();
    write_tag(&mut out, profile, tag);
    out
}

/// Print a tag with a name in front of it, the way `/data get` labels a result.
///
/// Included because the file form of the binary encoding has a root name and
/// SNBT has nowhere to put one, so a caller converting between the two needs
/// somewhere for it to go that is not silently the bin.
pub fn to_string_named(name: &str, tag: &Tag) -> String {
    to_string_named_with(PrintProfile::default(), name, tag)
}

/// [`to_string_named`] under `profile`.
pub fn to_string_named_with(profile: PrintProfile, name: &str, tag: &Tag) -> String {
    let mut out = String::new();
    if !name.is_empty() {
        write_key(&mut out, name);
        out.push(':');
    }
    write_tag(&mut out, profile, tag);
    out
}

fn write_tag(out: &mut String, profile: PrintProfile, tag: &Tag) {
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
                match profile.numeric {
                    NumericStyle::Shortest => {
                        // Formatted as an `f32`, not promoted: `0.1f32` promoted to
                        // `f64` is 0.10000000149011612, and printing that is both
                        // hideous and unlike vanilla. Rust's `{}` for an `f32` gives
                        // the shortest decimal that reads back as the same `f32`.
                        let _ = write!(out, "{v}");
                    }
                    NumericStyle::Java => {
                        // The same digits `Float.toString` would have chosen —
                        // both are the shortest spellings that round-trip through
                        // `f32` — shaped into Java's decimal/scientific split.
                        write_java_shaped(out, format!("{v:e}").as_str());
                    }
                }
            }
            out.push('f');
        }
        Tag::Double(v) => {
            if !write_non_finite(out, *v) {
                match profile.numeric {
                    NumericStyle::Shortest => {
                        let _ = write!(out, "{v}");
                    }
                    // See the `NumericStyle::Java` note: same digits as
                    // `Double.toString`, Java's shapes.
                    NumericStyle::Java => write_java_shaped(out, format!("{v:e}").as_str()),
                }
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
        Tag::List(list) => write_list(out, profile, list),
        Tag::Compound(compound) => write_compound(out, profile, compound),
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
/// Unlike [`NumericStyle::Shortest`] this never happens without exponent
/// notation under [`NumericStyle::Java`], whose very large doubles print as a
/// handful of digits instead. Both forms read back as the same number.
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

/// Shape Rust's shortest decimal into Java's `toString` spelling, given the
/// lower-exp form `{}` produces for a finite value: `[-]d[.ddd]e[-]dd`, one
/// non-zero digit before the (optional) point.
///
/// The digits themselves are kept exactly as formatted — they are already the
/// shortest ones that round-trip, which is what the JDK's own formatter aims
/// for too. What changes is only where the point goes and whether the number
/// is written out or exponentiated:
///
/// * Java keeps **decimal** form precisely when |v| lies in [10⁻³, 10⁷), that
///   is, when the exponent sits in `-3..=6`. Outside the window: scientific.
/// * Scientific is an upper case `E`, the exponent a plain signed integer —
///   no `+`, no padding — and the mantissa always keeps at least one
///   fractional digit: `1.0E7`, never `1E7` and never `1.0E+07`.
/// * Decimal form always shows a point with something after it: `100.0`,
///   `0.001`, `-0.0`.
fn write_java_shaped(out: &mut String, lower_exp: &str) {
    let bytes = lower_exp.as_bytes();
    let mut at = 0;
    if bytes[0] == b'-' {
        out.push('-');
        at += 1;
    }
    // The mantissa digits without the point. Rust guarantees the first is
    // significant (non-zero) except for zero itself, which formats as `0e0`
    // and falls through both branches below correctly.
    let mut digits = String::with_capacity(bytes.len());
    while bytes[at].is_ascii_digit() {
        digits.push(bytes[at] as char);
        at += 1;
    }
    if bytes[at] == b'.' {
        at += 1;
        while bytes[at].is_ascii_digit() {
            digits.push(bytes[at] as char);
            at += 1;
        }
    }
    debug_assert_eq!(bytes[at], b'e');
    at += 1;
    let mut exponent = 0i32;
    let negative_exponent = bytes[at] == b'-';
    if negative_exponent {
        at += 1;
    }
    while at < bytes.len() {
        exponent = exponent * 10 + i32::from(bytes[at] - b'0');
        at += 1;
    }
    if negative_exponent {
        exponent = -exponent;
    }

    if (-3..=6).contains(&exponent) {
        if exponent >= 0 {
            let point = exponent as usize + 1;
            if digits.len() > point {
                // Digits to spare: the point lands inside them.
                out.push_str(&digits[..point]);
                out.push('.');
                out.push_str(&digits[point..]);
            } else {
                // A whole number: pad out to the point, then Java's mandatory
                // fraction of zero.
                out.push_str(&digits);
                for _ in digits.len()..point {
                    out.push('0');
                }
                out.push_str(".0");
            }
        } else {
            // Below one: `0.00…digits`, with the point that many places left
            // of the first significant digit.
            out.push_str("0.");
            for _ in 0..(-exponent - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        }
    } else {
        out.push(digits.as_bytes()[0] as char);
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('E');
        let _ = write!(out, "{exponent}");
    }
}

fn write_list(out: &mut String, profile: PrintProfile, list: &List) {
    out.push('[');
    for (index, element) in list.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_tag(out, profile, element);
    }
    out.push(']');
}

fn write_compound(out: &mut String, profile: PrintProfile, compound: &Compound) {
    out.push('{');
    for (index, (name, value)) in compound.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_key(out, name);
        out.push(':');
        write_tag(out, profile, value);
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
