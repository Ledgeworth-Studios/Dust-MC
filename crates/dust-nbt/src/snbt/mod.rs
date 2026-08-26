//! SNBT: NBT written as text.
//!
//! This is what a player types after `/data merge` and what `/give` takes in
//! brackets: `{Count:1b,id:"minecraft:stone",tag:{display:{Name:'{"text":"x"}'}}}`.
//! It is not JSON, and the differences are the whole difficulty — keys are
//! usually unquoted, both quote characters are available, numbers carry a
//! type suffix, and three of the thirteen tags have array syntax that a plain
//! list does not.
//!
//! # Where this grammar comes from
//!
//! Not from a wiki. Minecraft's parser is `net.minecraft.nbt.TagParser`, and in
//! the 1.21.1 server jar it is a class with seven `java.util.regex.Pattern`
//! constants and a dispatch that tries them in a fixed order. Both the patterns
//! and the order were read out of the class file. The parts that are not
//! regexes — which characters may appear in an unquoted string, how a quoted
//! string escapes — come from `com.mojang.brigadier.StringReader` in the
//! bundled Brigadier 1.3.10, read the same way.
//!
//! The seven rules, in the order `TagParser` tries them:
//!
//! | rule | pattern | flags |
//! |---|---|---|
//! | float | `[-+]?(?:[0-9]+[.]?\|[0-9]*[.][0-9]+)(?:e[-+]?[0-9]+)?f` | case-insensitive |
//! | byte | `[-+]?(?:0\|[1-9][0-9]*)b` | case-insensitive |
//! | long | `[-+]?(?:0\|[1-9][0-9]*)l` | case-insensitive |
//! | short | `[-+]?(?:0\|[1-9][0-9]*)s` | case-insensitive |
//! | int | `[-+]?(?:0\|[1-9][0-9]*)` | none |
//! | double, suffixed | `[-+]?(?:[0-9]+[.]?\|[0-9]*[.][0-9]+)(?:e[-+]?[0-9]+)?d` | case-insensitive |
//! | double, bare | `[-+]?(?:[0-9]+[.]\|[0-9]*[.][0-9]+)(?:e[-+]?[0-9]+)?` | case-insensitive |
//!
//! There is one asymmetry between them, and it is not a transcription error.
//! The suffixed float and double rules allow `[0-9]+[.]?` — a bare integer with
//! a suffix, so `1f` is a float — while the *bare* double rule allows only
//! `[0-9]+[.]`, requiring the point. That single `?` is what makes `1` an int
//! and `1.` a double.
//!
//! Anything matching none of the seven, and not `true` or `false`, is a string.
//!
//! Three things follow from that grammar that are worth knowing before they
//! surprise someone:
//!
//! * **An integer may not have a leading zero.** The pattern is
//!   `[-+]?(?:0|[1-9][0-9]*)`, so `0` is an int and `01` is not — it falls all
//!   the way through to being the *string* `"01"`. The same is true of `007b`.
//! * **A number that overflows its type becomes a string.** `TagParser` catches
//!   `NumberFormatException` around the whole dispatch and falls through to
//!   `StringTag`, so `300b` parses as the four-character string `"300b"`, not
//!   as a byte, and not as an error.
//! * **`true` and `false` are `TAG_Byte`.** Case-insensitively, and only as a
//!   whole unquoted word. There is no boolean tag; there never was.
//!
//! # What SNBT cannot represent
//!
//! There is no syntax for a NaN or an infinity. Vanilla prints them anyway —
//! `FloatTag.toString()` on a NaN produces `NaNf` — and its own parser reads
//! that back as the string `"NaNf"`. This crate reproduces vanilla's output
//! rather than inventing a syntax vanilla would reject, so the same lossiness
//! applies here, and `tests/snbt.rs` asserts it explicitly so that it is a
//! documented hole rather than a surprise. A tag tree containing a non-finite
//! float does not survive an SNBT round trip. It survives a *binary* round trip
//! exactly, bit patterns and all.
//!
//! # What this printer does not do
//!
//! It produces SNBT that Mojang's parser accepts and that re-parses to the tag
//! it came from. It does not reproduce vanilla's output byte for byte, because
//! the float and double formatting go through Rust's shortest-round-trip
//! formatter and vanilla's go through Java's, and the two disagree on
//! presentation — Java writes `1.0E10` where Rust writes `10000000000`, and
//! Java writes `1.0` where Rust writes `1`. Both re-parse to the same number.

mod parse;
mod print;

pub use parse::{parse, parse_compound, Expected, ParseError};
pub use print::{to_string, to_string_named};
