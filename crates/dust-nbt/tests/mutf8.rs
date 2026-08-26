//! Modified UTF-8, against vectors recorded from the JDK.
//!
//! # Where these numbers came from
//!
//! Not from a specification and not from this crate. Every `ENC` vector below
//! is what `java.io.DataOutputStream.writeUTF` produced for that string, and
//! every `DEC` expectation is what `java.io.DataInputStream.readUTF` did with
//! those bytes, on OpenJDK 22 (`java version "22" 2024-03-19`). The programme
//! that produced them is twenty lines and is reproduced here so that anyone can
//! re-derive the table rather than trust it:
//!
//! ```text
//! import java.io.*;
//! public class Mutf8Probe {
//!     static void enc(String label, String s) throws IOException {
//!         ByteArrayOutputStream bo = new ByteArrayOutputStream();
//!         new DataOutputStream(bo).writeUTF(s);
//!         StringBuilder sb = new StringBuilder();
//!         for (byte x : bo.toByteArray()) sb.append(String.format("%02x", x));
//!         System.out.println("ENC " + label + " -> " + sb);
//!     }
//!     static void dec(String label, int... bytes) {
//!         byte[] p = new byte[bytes.length];
//!         for (int i = 0; i < bytes.length; i++) p[i] = (byte) bytes[i];
//!         byte[] full = new byte[p.length + 2];
//!         full[0] = (byte) (p.length >> 8); full[1] = (byte) p.length;
//!         System.arraycopy(p, 0, full, 2, p.length);
//!         try {
//!             String s = new DataInputStream(new ByteArrayInputStream(full)).readUTF();
//!             for (int i = 0; i < s.length(); i++)
//!                 System.out.printf("DEC %s -> U+%04X%n", label, (int) s.charAt(i));
//!         } catch (Exception e) { System.out.println("DEC " + label + " -> " + e); }
//!     }
//!     public static void main(String[] a) throws Exception { /* the cases below */ }
//! }
//! ```
//!
//! The two-byte length prefix `writeUTF` writes is stripped from the `ENC`
//! vectors here, because [`dust_nbt::mutf8::encode`] produces the payload and
//! the reader writes the prefix.

use dust_nbt::mutf8::{self, Mutf8Error};

/// Encoding, against what the JDK produced.
///
/// Each row is (what the string is, the string, the payload `writeUTF` wrote).
/// The rows with a six-byte encoding are the ones that separate this from
/// UTF-8: standard UTF-8 would use four bytes and `readUTF` would refuse them.
#[test]
fn encoding_matches_the_jdk() {
    let cases: &[(&str, &str, &[u8])] = &[
        ("empty", "", &[]),
        ("ascii", "hello", b"hello"),
        // `writeUTF("\0")` is `00 02 c0 80`: two payload bytes for one NUL.
        ("U+0000 alone", "\u{0000}", &[0xc0, 0x80]),
        ("a NUL b", "a\u{0000}b", &[0x61, 0xc0, 0x80, 0x62]),
        ("U+007F", "\u{007f}", &[0x7f]),
        ("U+0080", "\u{0080}", &[0xc2, 0x80]),
        ("U+07FF", "\u{07ff}", &[0xdf, 0xbf]),
        ("U+0800", "\u{0800}", &[0xe0, 0xa0, 0x80]),
        ("U+FFFF", "\u{ffff}", &[0xef, 0xbf, 0xbf]),
        ("section sign", "\u{00a7}", &[0xc2, 0xa7]),
        // Above the BMP: a surrogate pair, six bytes, not the four-byte form.
        ("U+1F600 emoji", "\u{1f600}", &[0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80]),
        ("U+10000", "\u{10000}", &[0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80]),
        ("U+10FFFF", "\u{10ffff}", &[0xed, 0xaf, 0xbf, 0xed, 0xbf, 0xbf]),
        (
            "emoji then NUL",
            "\u{1f600}\u{0000}",
            &[0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80, 0xc0, 0x80],
        ),
    ];

    for (label, text, expected) in cases {
        assert_eq!(
            mutf8::encode(text),
            *expected,
            "encoding {label} did not match what the JDK wrote"
        );
        assert_eq!(
            mutf8::encoded_len(text),
            expected.len(),
            "encoded_len disagreed with encode for {label}; the writer measures with one \
             and appends with the other, so they have to agree or a length prefix lies"
        );
        assert_eq!(
            mutf8::decode(expected).expect("decodes").as_ref(),
            *text,
            "decoding the JDK's bytes for {label} did not give the string back"
        );
    }
}

/// The four-byte form standard UTF-8 uses for an emoji is *invalid* here.
///
/// This is the case a `str::from_utf8` reader gets exactly backwards: it
/// accepts these bytes, which no Java writer produces, and rejects the six-byte
/// form, which every Java writer produces.
#[test]
fn standard_utf8_supplementary_form_is_rejected() {
    let four_byte = [0xf0, 0x9f, 0x98, 0x80];
    assert!(
        std::str::from_utf8(&four_byte).is_ok(),
        "these bytes are valid UTF-8, which is the point"
    );
    assert_eq!(
        mutf8::decode(&four_byte),
        Err(Mutf8Error::InvalidStart {
            offset: 0,
            byte: 0xf0
        }),
        "readUTF rejects this with 'malformed input around byte 0' and so must we"
    );
}

/// A raw NUL and the two overlong forms the JDK accepts, refused here.
///
/// Recorded JDK behaviour: `00` decodes to U+0000, `c1 bf` to U+007F, and
/// `e0 80 80` to U+0000. All three are refused here, and the module note on
/// `mutf8` sets out why — accepting them would mean a rewrite changed the
/// bytes, and they are the standard way to slip a character past a filter that
/// looked at the encoded form.
#[test]
fn overlong_forms_the_jdk_accepts_are_refused_here() {
    assert_eq!(
        mutf8::decode(&[0x00]),
        Err(Mutf8Error::InvalidStart {
            offset: 0,
            byte: 0x00
        })
    );
    assert_eq!(
        mutf8::decode(&[0xc1, 0xbf]),
        Err(Mutf8Error::Overlong {
            offset: 0,
            value: 0x7f
        })
    );
    assert_eq!(
        mutf8::decode(&[0xe0, 0x80, 0x80]),
        Err(Mutf8Error::Overlong {
            offset: 0,
            value: 0
        })
    );
    // And the one two-byte form that is *not* overlong, because this encoding
    // requires it.
    assert_eq!(mutf8::decode(&[0xc0, 0x80]).unwrap().as_ref(), "\u{0000}");
}

/// A lone surrogate: legal for Java, impossible for a Rust `String`.
///
/// Recorded JDK behaviour: `ed a0 80` decodes to the single char U+D800, and
/// `ed a0 bd 41` decodes to U+D83D followed by `A`. Rust cannot hold either
/// result, so both are errors here, naming the surrogate.
#[test]
fn lone_surrogates_are_an_error_that_names_the_surrogate() {
    assert_eq!(
        mutf8::decode(&[0xed, 0xa0, 0x80]),
        Err(Mutf8Error::UnpairedSurrogate {
            offset: 0,
            value: 0xd800
        })
    );
    assert_eq!(
        mutf8::decode(&[0xed, 0xb0, 0x80]),
        Err(Mutf8Error::UnpairedSurrogate {
            offset: 0,
            value: 0xdc00
        })
    );
    // A high surrogate followed by something that is not a low surrogate. The
    // error blames the high surrogate, at its own offset, rather than the `A`
    // that followed it — the `A` is fine, it is the surrogate that is not a
    // character.
    assert_eq!(
        mutf8::decode(&[0xed, 0xa0, 0xbd, 0x41]),
        Err(Mutf8Error::UnpairedSurrogate {
            offset: 0,
            value: 0xd83d
        })
    );
    // A high surrogate at the very end of the payload. The string carries its
    // own length, so nothing is truncated: the payload is complete and holds a
    // surrogate with no partner, which is what the error names and where it
    // names it.
    assert_eq!(
        mutf8::decode(&[0x41, 0xed, 0xa0, 0xbd]),
        Err(Mutf8Error::UnpairedSurrogate {
            offset: 1,
            value: 0xd83d
        })
    );
}

/// Truncated and malformed sequences, matching what `readUTF` refuses.
#[test]
fn malformed_sequences_are_refused_with_an_offset() {
    // Recorded: `c0` alone is "partial character at end".
    assert_eq!(
        mutf8::decode(&[0xc0]),
        Err(Mutf8Error::Truncated {
            offset: 0,
            needed: 2,
            available: 1
        })
    );
    assert_eq!(
        mutf8::decode(&[0xe0, 0x80]),
        Err(Mutf8Error::Truncated {
            offset: 0,
            needed: 3,
            available: 2
        })
    );
    // Recorded: `ff` and a bare continuation byte are both "malformed input
    // around byte 0".
    assert_eq!(
        mutf8::decode(&[0xff]),
        Err(Mutf8Error::InvalidStart {
            offset: 0,
            byte: 0xff
        })
    );
    assert_eq!(
        mutf8::decode(&[0x80]),
        Err(Mutf8Error::InvalidStart {
            offset: 0,
            byte: 0x80
        })
    );
    // A continuation byte that is not one, in the middle of a sequence, with
    // the offset of the *bad byte* rather than of the sequence.
    assert_eq!(
        mutf8::decode(&[0x41, 0xc2, 0x41]),
        Err(Mutf8Error::InvalidContinuation {
            offset: 2,
            byte: 0x41
        })
    );
}

/// The fast path and the slow path have to agree.
///
/// `decode` borrows the input when it contains none of `00`, `c0` or `ed`, and
/// decodes character by character otherwise. Two code paths for one function is
/// how a decoder ends up correct in the case nobody tests, so this drives a
/// string through both and compares.
#[test]
fn the_borrowing_path_and_the_decoding_path_agree() {
    let awkward = [
        "",
        "plain ascii",
        "\u{00a7}6gold\u{00a7}r",
        "\u{4e2d}\u{6587}",
        "\u{d000}", // A three-byte sequence with lead 0xed that is not a surrogate.
        "a\u{0000}b",
        "\u{1f600}",
        "mixed \u{00a7}c\u{1f600}\u{0000} tail",
    ];
    for text in awkward {
        let encoded = mutf8::encode(text);
        let decoded = mutf8::decode(&encoded).expect("round-trips");
        assert_eq!(decoded.as_ref(), text, "for {text:?}");
        // Whether it borrowed is an implementation detail, but which path ran
        // is not: a string with none of the three marker bytes must borrow, or
        // the fast path is not being taken and the performance note is wrong.
        let expect_borrow = !encoded.iter().any(|&b| b == 0 || b == 0xc0 || b == 0xed);
        assert_eq!(
            matches!(decoded, std::borrow::Cow::Borrowed(_)),
            expect_borrow,
            "for {text:?}: the fast path should have been taken iff the payload has none \
             of 00, c0 or ed"
        );
    }
}

/// A string too long for the `u16` prefix is refused by name.
///
/// The limit is on the *encoded* length, which is the trap: 40,000 characters
/// is well within 65,535 and 40,000 emoji is 240,000 bytes.
#[test]
fn a_string_too_long_to_write_is_refused_and_says_why() {
    use dust_nbt::{write, Tag};

    // 32,768 emoji: 32,768 chars, and 196,608 bytes once encoded.
    let long: String = std::iter::repeat_n('\u{1f600}', 32_768).collect();
    assert!(long.chars().count() < mutf8::MAX_ENCODED_LEN);
    assert_eq!(mutf8::encoded_len(&long), 32_768 * 6);

    let error = write::to_vec("", &Tag::String(long.clone())).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("196608") && message.contains("65535"),
        "the error should name both the length it had and the length allowed: {message}"
    );

    // And the boundary itself: exactly 65,535 bytes is writable, one more is
    // not. Built from ASCII so the encoded length is the character count.
    let at_limit: String = std::iter::repeat_n('x', mutf8::MAX_ENCODED_LEN).collect();
    assert!(write::to_vec("", &Tag::String(at_limit.clone())).is_ok());
    let over_limit = format!("{at_limit}x");
    assert!(write::to_vec("", &Tag::String(over_limit)).is_err());
}

/// The same limit applies to a *name*, which is the more likely way to hit it.
#[test]
fn a_key_too_long_to_write_is_refused_too() {
    use dust_nbt::{write, Compound, Tag};

    let mut compound = Compound::new();
    let long: String = std::iter::repeat_n('\u{0000}', 40_000).collect();
    // 40,000 NULs is 40,000 characters and 80,000 bytes.
    assert_eq!(mutf8::encoded_len(&long), 80_000);
    compound.insert(long, Tag::Byte(1));
    assert!(write::to_vec("", &Tag::Compound(compound)).is_err());
}
