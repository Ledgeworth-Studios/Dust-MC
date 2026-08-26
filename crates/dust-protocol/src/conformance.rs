//! Known-answer vectors, and the runner the other crates check themselves with.
//!
//! # Why this is public API and not a test module
//!
//! Three crates were always going to implement pieces of this format.
//! `dust-nbt` still will own NBT. The VarInt question is settled: there is one
//! implementation, in [`crate::varint`], written by `dust-net` and adopted as
//! the project-wide rule when the crates merged — so the failure mode below
//! can no longer happen to it, and these tables now hold that decision in
//! place rather than referee it.
//!
//! Checking them against each other is the obvious thing and is not enough: it
//! says they agree, not that either is right, and when they disagree it does
//! not say which is wrong. So the tables below are a **third thing**. They were
//! computed by a separate implementation written from the format description
//! rather than by the code they test, and every implementation in this
//! workspace is checked against them. Two implementations that both satisfy
//! the same vector table cannot disagree with each other about anything the
//! table covers.
//!
//! That last clause is the honest limit. A vector table catches what it
//! covers. It says nothing about VarInts of a width nobody thought to list, or
//! an NBT shape nobody wrote down — which is why the live-server test in
//! `tests/` exists as well, and why these two checks are not substitutes for
//! each other.
//!
//! # How `dust-net` uses this
//!
//! ```ignore
//! use dust_protocol::conformance::{check_wire, WireImplementation};
//!
//! #[test]
//! fn our_var_int_agrees_with_the_protocol_vectors() {
//!     let failures = check_wire(&WireImplementation {
//!         read_var_int: |bytes| { /* dust-net's reader */ },
//!         read_var_long: |bytes| { /* ... */ },
//!         write_var_int: |value| { /* ... */ },
//!         write_var_long: |value| { /* ... */ },
//!     });
//!     assert!(failures.is_empty(), "{failures:#?}");
//! }
//! ```
//!
//! The runner takes plain function pointers rather than a generic over the
//! [`WireRead`] trait on purpose: `dust-net`'s reader
//! will have its own error type and its own lifetimes, and a conformance check
//! that made it implement this crate's traits first would be a check it could
//! not run until after the merge it exists to make safe.

use crate::nbt;
use crate::types::{read_string, write_string, Position};
use crate::wire::{Reader, WireRead, WireWrite, Writer};

/// A VarInt as `(value, bytes)`.
///
/// Computed from the format description — seven bits per byte, low group
/// first, continuation bit while more groups follow, the value taken as two's
/// complement of its width — by an implementation that is not the one under
/// test.
///
/// The negative cases are the ones that matter. A VarInt of a negative `i32`
/// is always five bytes, because the sign bits are real bits and there is no
/// zig-zag encoding here; an implementation that reached for one produces
/// three bytes for `-1` and disagrees with every Minecraft client ever
/// written.
pub static VAR_INT_VECTORS: &[(i32, &[u8])] = &[
    (0, &[0x00]),
    (1, &[0x01]),
    (2, &[0x02]),
    (10, &[0x0a]),
    (127, &[0x7f]),
    (128, &[0x80, 0x01]),
    (255, &[0xff, 0x01]),
    (300, &[0xac, 0x02]),
    (25565, &[0xdd, 0xc7, 0x01]),
    (2097151, &[0xff, 0xff, 0x7f]),
    (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
    (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
    (-2, &[0xfe, 0xff, 0xff, 0xff, 0x0f]),
    (-25565, &[0xa3, 0xb8, 0xfe, 0xff, 0x0f]),
    (-2147483648, &[0x80, 0x80, 0x80, 0x80, 0x08]),
];

/// A VarLong as `(value, bytes)`, from the same independent implementation.
pub static VAR_LONG_VECTORS: &[(i64, &[u8])] = &[
    (0, &[0x00]),
    (1, &[0x01]),
    (2, &[0x02]),
    (127, &[0x7f]),
    (128, &[0x80, 0x01]),
    (255, &[0xff, 0x01]),
    (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
    (
        9223372036854775807,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
    ),
    (
        -1,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
    ),
    (
        -2147483648,
        &[0x80, 0x80, 0x80, 0x80, 0xf8, 0xff, 0xff, 0xff, 0xff, 0x01],
    ),
    (
        -9223372036854775808,
        &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
    ),
];

/// Byte strings a conforming reader must refuse rather than accept.
///
/// A reader that only ever sees the accept table looks perfect and will happily
/// run off the end of a body on the first malformed input, which is the shape
/// an attacker sends.
///
/// The last three rows are the canonical-encoding rule, adopted project-wide
/// from `dust-net` when the crates merged, and they are deliberate reversals
/// of what this crate's stopgap reader used to accept. Vanilla discards junk:
/// it decodes the five-byte zero, the padded one and the wide final byte to
/// `0`, `1` and `-1`. Dust refuses all three, because an encoder that accepts
/// two byte strings for one value makes every replay guard, rate limiter or
/// deduplication keyed on frame bytes answer a question other than the one it
/// thinks it is answering. Refusing makes the map a bijection; see
/// [`crate::varint`] for the argument in full.
pub static VAR_INT_REJECTS: &[&[u8]] = &[
    // Six continuation bytes: past the width an i32 has.
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
    // Continues, and then the body ends.
    &[0x80],
    // Nothing at all.
    &[],
    // Five bytes for zero: four groups carry nothing.
    &[0x80, 0x80, 0x80, 0x80, 0x00],
    // Two bytes for one.
    &[0x81, 0x00],
    // The final byte sets bits 32..35, which an i32 does not have. Vanilla
    // shifts them off and calls this -1; so does 0xff 0xff 0xff 0xff 0x0f,
    // and only one of them may mean that.
    &[0xff, 0xff, 0xff, 0xff, 0x7f],
];

/// `(x, y, z, eight bytes big-endian)` for the packed block position.
///
/// The last row is the example published with the format itself, which makes
/// it the one row here that is independent of *both* implementations in this
/// workspace — a third party wrote the number and the bytes down together.
/// Every negative row is there because sign extension is the thing that gets
/// missed: an implementation that masks without extending gets all of the
/// positive rows right.
pub static POSITION_VECTORS: &[(i32, i32, i32, &[u8])] = &[
    (0, 0, 0, &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
    (1, 2, 3, &[0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x30, 0x02]),
    (
        -100,
        -60,
        100,
        &[0xff, 0xff, 0xe7, 0x00, 0x00, 0x06, 0x4f, 0xc4],
    ),
    (
        33554431,
        2047,
        33554431,
        &[0x7f, 0xff, 0xff, 0xdf, 0xff, 0xff, 0xf7, 0xff],
    ),
    (
        -33554432,
        -2048,
        -33554432,
        &[0x80, 0x00, 0x00, 0x20, 0x00, 0x00, 0x08, 0x00],
    ),
    (
        18357644,
        831,
        -20882616,
        &[0x46, 0x07, 0x63, 0x2c, 0x15, 0xb4, 0x83, 0x3f],
    ),
];

/// `(text, utf16 length, utf8 byte length, wire bytes)`.
///
/// The rows where the second and third numbers differ are the entire point.
/// `café` is four UTF-16 units and five bytes; `日本語` is three and nine; a
/// single emoji is two and four. A length check written in the wrong unit
/// agrees with this table on `Dust` and `Notch` and disagrees on everything
/// after them, which is exactly how the bug survives review.
pub static STRING_VECTORS: &[(&str, usize, usize, &[u8])] = &[
    ("", 0, 0, &[0x00]),
    ("a", 1, 1, &[0x01, 0x61]),
    ("Dust", 4, 4, &[0x04, 0x44, 0x75, 0x73, 0x74]),
    ("Notch", 5, 5, &[0x05, 0x4e, 0x6f, 0x74, 0x63, 0x68]),
    ("café", 4, 5, &[0x05, 0x63, 0x61, 0x66, 0xc3, 0xa9]),
    (
        "日本語",
        3,
        9,
        &[0x09, 0xe6, 0x97, 0xa5, 0xe6, 0x9c, 0xac, 0xe8, 0xaa, 0x9e],
    ),
    ("😀", 2, 4, &[0x04, 0xf0, 0x9f, 0x98, 0x80]),
    ("a😀b", 4, 6, &[0x06, 0x61, 0xf0, 0x9f, 0x98, 0x80, 0x62]),
];

/// `(length of the value, bytes that start with it)` for network NBT.
///
/// Some rows carry trailing bytes after the value, because "where does it end"
/// is the only question [`nbt::scan`] answers and a scanner that consumed the
/// whole slice would pass every row that had nothing after it.
pub static NBT_VECTORS: &[(usize, &[u8])] = &[
    (1, &[0x00]),
    (5, &[0x08, 0x00, 0x02, 0x68, 0x69]),
    (2, &[0x0a, 0x00]),
    (
        37,
        &[
            0x0a, 0x08, 0x00, 0x04, 0x74, 0x65, 0x78, 0x74, 0x00, 0x04, 0x44, 0x75, 0x73, 0x74,
            0x08, 0x00, 0x05, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x04, 0x67, 0x6f, 0x6c, 0x64,
            0x01, 0x00, 0x04, 0x62, 0x6f, 0x6c, 0x64, 0x01, 0x00,
        ],
    ),
    (
        14,
        &[
            0x09, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x07, 0xff, 0xff, 0xff, 0xf9,
        ],
    ),
    (6, &[0x09, 0x00, 0x00, 0x00, 0x00, 0x00]),
    (
        29,
        &[
            0x0c, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00,
        ],
    ),
    (9, &[0x07, 0x00, 0x00, 0x00, 0x04, 0x01, 0x02, 0x03, 0x04]),
    // Nested compounds. The row that caught a bug in the generator that
    // produced this table rather than in the code it tests: a compound used as
    // a *payload* must not repeat its own tag byte, because the parent already
    // wrote it as the entry's type, and only the root value is tagged. The
    // first version of this row was 22 bytes and double-tagged. Which of the
    // two implementations was wrong was settled by tracing the format, not by
    // preferring one of them — and that is the situation a differential exists
    // to produce.
    (
        20,
        &[
            0x0a, 0x0a, 0x00, 0x01, 0x61, 0x0a, 0x00, 0x01, 0x62, 0x03, 0x00, 0x01, 0x63, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ],
    ),
    (
        70,
        &[
            0x0a, 0x01, 0x00, 0x01, 0x62, 0x01, 0x02, 0x00, 0x01, 0x73, 0xff, 0xfe, 0x03, 0x00,
            0x01, 0x69, 0x00, 0x00, 0x00, 0x03, 0x04, 0x00, 0x01, 0x6c, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x04, 0x05, 0x00, 0x01, 0x66, 0x40, 0xa0, 0x00, 0x00, 0x06, 0x00,
            0x01, 0x64, 0x40, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x02, 0x69,
            0x61, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00,
        ],
    ),
    // A value with four bytes of junk after it: the answer is the value's
    // length, not the slice's.
    (
        15,
        &[
            0x0a, 0x08, 0x00, 0x04, 0x74, 0x65, 0x78, 0x74, 0x00, 0x04, 0x44, 0x75, 0x73, 0x74,
            0x00, 0xde, 0xad, 0xbe, 0xef,
        ],
    ),
];

/// Byte strings no NBT reader may accept.
pub static NBT_REJECTS: &[&[u8]] = &[
    // A compound that ends in the middle of an entry.
    &[0x0a, 0x08, 0x00, 0x04, 0x74, 0x65, 0x78, 0x74],
    // A tag type that does not exist.
    &[0x0d, 0x00],
    // A negative array length.
    &[0x0c, 0xff, 0xff, 0xff, 0xff],
    // A list of TAG_End that claims to have elements: there is no payload to
    // step over, so a reader that looped would loop forever.
    &[0x09, 0x00, 0x00, 0x00, 0x00, 0x01],
    // A string longer than what follows it.
    &[0x08, 0x00, 0x10, 0x68, 0x69],
    // An array whose length runs off the end of the body.
    &[0x0b, 0x77, 0x35, 0x94, 0x00, 0x00],
    // Nothing at all.
    &[],
];

/// One implementation of the contested wire primitives, as plain functions.
///
/// `read_*` returns the value and how many bytes it consumed, or `None` if the
/// implementation refuses the input. The byte count is checked as well as the
/// value: two readers can agree on what `0x80 0x01` means and disagree about
/// whether it was two bytes, and the second disagreement desynchronises a
/// stream while the first does not.
#[derive(Clone, Copy)]
pub struct WireImplementation {
    pub read_var_int: fn(&[u8]) -> Option<(i32, usize)>,
    pub read_var_long: fn(&[u8]) -> Option<(i64, usize)>,
    pub write_var_int: fn(i32) -> Vec<u8>,
    pub write_var_long: fn(i64) -> Vec<u8>,
}

impl std::fmt::Debug for WireImplementation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireImplementation")
    }
}

/// Check an implementation against every vector, and report **all** the ways it
/// disagreed rather than the first.
///
/// An empty result is a pass. A caller that only wants a boolean can ask
/// whether the vector is empty; a caller debugging a merge wants the list.
pub fn check_wire(subject: &WireImplementation) -> Vec<String> {
    let mut failures = Vec::new();

    for (value, bytes) in VAR_INT_VECTORS {
        let written = (subject.write_var_int)(*value);
        if written != *bytes {
            failures.push(format!(
                "VarInt {value} encodes to {written:02x?} and the vectors say {bytes:02x?}"
            ));
        }
        match (subject.read_var_int)(bytes) {
            Some((read, used)) if read == *value && used == bytes.len() => {}
            other => failures.push(format!(
                "VarInt {bytes:02x?} reads as {other:?} and the vectors say ({value}, {})",
                bytes.len()
            )),
        }
    }

    for (value, bytes) in VAR_LONG_VECTORS {
        let written = (subject.write_var_long)(*value);
        if written != *bytes {
            failures.push(format!(
                "VarLong {value} encodes to {written:02x?} and the vectors say {bytes:02x?}"
            ));
        }
        match (subject.read_var_long)(bytes) {
            Some((read, used)) if read == *value && used == bytes.len() => {}
            other => failures.push(format!(
                "VarLong {bytes:02x?} reads as {other:?} and the vectors say ({value}, {})",
                bytes.len()
            )),
        }
    }

    for bytes in VAR_INT_REJECTS {
        if let Some(accepted) = (subject.read_var_int)(bytes) {
            failures.push(format!(
                "VarInt {bytes:02x?} was accepted as {accepted:?} and must be refused"
            ));
        }
    }

    // A vector table is only as good as the fact that it is not empty, and a
    // table that got emptied by an edit would let every implementation pass.
    if VAR_INT_VECTORS.is_empty() || VAR_LONG_VECTORS.is_empty() || VAR_INT_REJECTS.is_empty() {
        failures.push("the vector tables are empty, so this check proved nothing".to_owned());
    }
    failures
}

/// Check an NBT reader's idea of where a value ends against the vectors.
///
/// `length_of` takes the bytes a value starts at and returns its length, or
/// `None` if it refuses them. That is the only question this crate asks of
/// `dust-nbt`, so it is the whole of the contract checked here.
pub fn check_nbt(length_of: fn(&[u8]) -> Option<usize>) -> Vec<String> {
    let mut failures = Vec::new();
    for (length, bytes) in NBT_VECTORS {
        match length_of(bytes) {
            Some(measured) if measured == *length => {}
            other => failures.push(format!(
                "NBT {bytes:02x?} measured {other:?} and the vectors say {length}"
            )),
        }
    }
    for bytes in NBT_REJECTS {
        if let Some(measured) = length_of(bytes) {
            failures.push(format!(
                "NBT {bytes:02x?} measured {measured} and must be refused"
            ));
        }
    }
    if NBT_VECTORS.is_empty() || NBT_REJECTS.is_empty() {
        failures.push("the NBT vector tables are empty, so this check proved nothing".to_owned());
    }
    failures
}

/// This crate's own wire implementation, as a [`WireImplementation`].
///
/// Exposed so that the crate's tests run the same runner `dust-net` will, and
/// so that a merge can compare the two side by side against the vectors rather
/// than against each other.
pub fn in_crate_wire() -> WireImplementation {
    WireImplementation {
        read_var_int: |bytes| {
            let mut reader = Reader::new(bytes);
            reader.read_var_int().ok().map(|v| (v, reader.position()))
        },
        read_var_long: |bytes| {
            let mut reader = Reader::new(bytes);
            reader.read_var_long().ok().map(|v| (v, reader.position()))
        },
        write_var_int: |value| {
            let mut writer = Writer::new();
            writer.write_var_int(value);
            writer.into_bytes()
        },
        write_var_long: |value| {
            let mut writer = Writer::new();
            writer.write_var_long(value);
            writer.into_bytes()
        },
    }
}

/// This crate's own NBT scanner, in the shape [`check_nbt`] takes.
pub fn in_crate_nbt() -> fn(&[u8]) -> Option<usize> {
    |bytes| nbt::scan(bytes).ok()
}

/// Check the field types this crate owns outright against their vectors.
///
/// `Position` and the string length rule are not going to be reimplemented
/// elsewhere, so there is no second implementation to differ from — but they
/// are the two field types whose bugs a round trip cannot see, so they get the
/// same treatment: a table computed elsewhere, and a check against it.
pub fn check_field_types(version: crate::ProtocolVersion) -> Vec<String> {
    use crate::types::{Decode, Encode};
    let mut failures = Vec::new();

    for (x, y, z, bytes) in POSITION_VECTORS {
        let position = Position::new(*x, *y, *z);
        let mut writer = Writer::new();
        if position.encode(&mut writer, version).is_err() {
            failures.push(format!("Position {x},{y},{z} would not encode"));
            continue;
        }
        if writer.as_bytes() != *bytes {
            failures.push(format!(
                "Position {x},{y},{z} encodes to {:02x?} and the vectors say {bytes:02x?}",
                writer.as_bytes()
            ));
        }
        match Position::decode(&mut Reader::new(bytes), version) {
            Ok(decoded) if decoded == position => {}
            other => failures.push(format!(
                "Position {bytes:02x?} decodes to {other:?} and the vectors say {x},{y},{z}"
            )),
        }
    }

    for (text, utf16, utf8, bytes) in STRING_VECTORS {
        if crate::types::utf16_len(text) != *utf16 {
            failures.push(format!(
                "`{text}` measures {} UTF-16 units and the vectors say {utf16}",
                crate::types::utf16_len(text)
            ));
        }
        if text.len() != *utf8 {
            failures.push(format!(
                "`{text}` is {} bytes and the vectors say {utf8}",
                text.len()
            ));
        }
        let mut writer = Writer::new();
        if write_string(&mut writer, text, *utf16).is_err() {
            failures.push(format!(
                "`{text}` would not encode at a limit of its own length, which is the limit it \
                 must exactly fit"
            ));
        } else if writer.as_bytes() != *bytes {
            failures.push(format!(
                "`{text}` encodes to {:02x?} and the vectors say {bytes:02x?}",
                writer.as_bytes()
            ));
        }
        match read_string(&mut Reader::new(bytes), *utf16) {
            Ok(decoded) if decoded == *text => {}
            other => failures.push(format!(
                "{bytes:02x?} decodes to {other:?} and the vectors say `{text}`"
            )),
        }
        // And the tight one: a limit one unit short must be refused. This is
        // the assertion a byte-length check fails, because for `café` the byte
        // length is five and a limit of three passes a five-byte check.
        if utf16 > &0 && read_string(&mut Reader::new(bytes), *utf16 - 1).is_ok() {
            failures.push(format!(
                "`{text}` was accepted at a limit of {} UTF-16 units, and it is {utf16}",
                *utf16 - 1
            ));
        }
    }

    if POSITION_VECTORS.is_empty() || STRING_VECTORS.is_empty() {
        failures.push("the field vector tables are empty, so this check proved nothing".to_owned());
    }
    failures
}
