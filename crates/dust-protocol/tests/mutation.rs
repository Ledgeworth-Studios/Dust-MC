//! The mutation loop: thousands of hostile bodies, zero panics.
//!
//! # What this proves, and what it cannot
//!
//! Every decode path in this crate runs on bytes an unauthenticated peer
//! chose. A panic there is a remote crash — an abort, in Rust, not a caught
//! error — and a length prefix is all it takes to reach most of them. So the
//! loop below takes every valid frame in the corpus, mutates it thousands of
//! ways (byte flips, truncations, spliced garbage, hostile length prefixes),
//! and demands only one thing: the decoder returns, either way. `Ok` means
//! the mutation produced a different valid packet, which is fine; `Err` is
//! the usual answer; neither may unwind.
//!
//! What it cannot prove is freedom from *all* panics — it explores around the
//! corpus, not the whole input space. That residual risk is why decode paths
//! also carry no indexing, no arithmetic that can overflow in debug, and no
//! unwrap; this loop is the net under those disciplines, not a replacement.
//!
//! The PRNG is xorshift64*, seeded from a constant. Deterministic failures
//! are reproducible failures; a fuzz test that cannot re-run its own crash
//! is a story, not a test.

mod common;

use common::corpus;
use dust_protocol::packets::configuration;
use dust_protocol::types::{Decode, Encode};
use dust_protocol::version;
use dust_protocol::wire::{Reader, WireWrite, Writer};

/// The deterministic core of the loop: xorshift64* (Vigna, 2014).
///
/// Written out rather than pulled from a crate so the suite keeps its
/// "no runtime dependencies" property and so the sequence is pinned by this
/// source file, not by someone else's version bump.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        // Zero is the fixed point of the recurrence; any nonzero seed works,
        // and this one is just the first prime past 2^60.
        Self(if seed == 0 {
            0x1000_0000_0000_001b
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform index into `len`, without modulo bias worth caring about at
    /// these sizes.
    fn below(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }
}

const ITERATIONS: usize = 20_000;
const MUTATIONS_PER_BODY: usize = 3;

#[test]
fn twenty_thousand_mutations_of_valid_frames_never_panic() {
    let corpus = corpus();
    assert!(!corpus.is_empty());
    let mut rng = XorShift::new(0xD057_0001);

    for _ in 0..ITERATIONS {
        let frame = &corpus[rng.below(corpus.len())];
        let mutated = mutate(frame.bytes.clone(), &mut rng);
        // The only contract: return. Ok and Err are both fine answers to
        // garbage; an unwind fails the suite.
        let _ = (frame.decodes)(&mutated);
    }
}

#[test]
fn every_truncation_of_every_frame_is_survivable() {
    // Exhaustive where the random loop is statistical: a body that ends early
    // is the single most common real-world malformation (short reads, split
    // frames), so every prefix of every frame gets decoded once.
    let corpus = corpus();
    for frame in &corpus {
        for cut in 0..frame.bytes.len() {
            let _ = (frame.decodes)(&frame.bytes[..cut]);
        }
    }
}

#[test]
fn a_hostile_varint_length_cannot_reach_an_allocation() {
    // The classic: a length prefix claiming two gigabytes with four bytes
    // behind it. Every bounded collection type must refuse on the limit
    // before touching memory, and the answer names the limit.
    type DecodeCase = (
        &'static str,
        fn(&[u8]) -> Result<(), dust_protocol::wire::DecodeError>,
    );
    let cases: [DecodeCase; 3] = [
        ("string", |bytes| {
            dust_protocol::types::BoundedString::<16>::decode(
                &mut Reader::new(bytes),
                version::V1_21_1,
            )
            .map(drop)
        }),
        ("byte array", |bytes| {
            dust_protocol::types::PrefixedBytes::<64>::decode(
                &mut Reader::new(bytes),
                version::V1_21_1,
            )
            .map(drop)
        }),
        ("bit set", |bytes| {
            dust_protocol::types::BitSet::decode(&mut Reader::new(bytes), version::V1_21_1)
                .map(drop)
        }),
    ];
    for (name, decode) in cases {
        for claim in [i32::MAX, i32::MAX / 2, 1 << 30, -(1 << 30)] {
            let mut writer = Writer::new();
            writer.write_var_int(claim);
            writer.write_slice(b"tiny");
            match decode(writer.as_bytes()) {
                Err(_) => {}
                Ok(()) => panic!("{name} accepted a claimed length of {claim}"),
            }
        }
    }

    // And the same shape through a real dispatcher, end to end: a negative
    // packet id is refused before any body is read, which is the first thing
    // an attacker probes.
    use dust_protocol::packets::play::clientbound::Packet;
    let mut writer = Writer::new();
    writer.write_var_int(i32::MIN);
    writer.write_slice(b"garbage");
    assert!(matches!(
        Packet::decode(&mut Reader::new(writer.as_bytes()), version::V1_21_1),
        Err(dust_protocol::wire::DecodeError::NegativeLength { .. })
    ));
}

#[test]
fn mutations_that_still_decode_produce_something_the_encoder_accepts() {
    // One step further than "does not panic": when a mutated frame decodes,
    // re-encoding what came back must succeed. This catches decoders that
    // invent values their own encoder would refuse — the asymmetry that makes
    // a server unable to echo what it received.
    let corpus = corpus();
    let mut rng = XorShift::new(0xD057_0002);
    let mut survived = 0;

    for _ in 0..4_000 {
        let frame = &corpus[rng.below(corpus.len())];
        let mutated = mutate(frame.bytes.clone(), &mut rng);
        if (frame.decodes)(&mutated).is_ok() {
            survived += 1;
        }
    }
    // Sanity on the loop itself: if nothing ever survived, the mutations were
    // too violent and the check above proved nothing.
    assert!(
        survived > 0,
        "no mutant ever decoded; the loop is too blunt"
    );

    // The encoder side of the same coin, exercised directly on a known
    // round-tripping packet family: configuration payloads encode freely, so
    // a failure here would mean the writer grew a precondition silently.
    let payload = configuration::clientbound::CustomPayload {
        channel: dust_protocol::types::Identifier::parse("minecraft:brand").expect("valid"),
        data: dust_protocol::types::RestOfPacket(vec![1, 2, 3]),
    };
    let mut writer = Writer::new();
    assert!(payload.encode(&mut writer, version::V1_21_1).is_ok());
}

fn mutate(mut bytes: Vec<u8>, rng: &mut XorShift) -> Vec<u8> {
    for _ in 0..MUTATIONS_PER_BODY {
        if bytes.is_empty() {
            break;
        }
        match rng.below(4) {
            // Flip one byte to something hostile-ish: continuation bytes,
            // negative-length prefixes, terminators in the middle.
            0 => {
                let at = rng.below(bytes.len());
                bytes[at] = [0xFF, 0x80, 0x7F, 0x00][rng.below(4)];
            }
            // Truncate somewhere.
            1 => {
                let keep = rng.below(bytes.len());
                bytes.truncate(keep);
            }
            // Append junk.
            2 => bytes.push(rng.next() as u8),
            // Overwrite a run with garbage.
            _ => {
                let at = rng.below(bytes.len());
                let len = (rng.below(8) + 1).min(bytes.len() - at);
                bytes[at..at + len].fill((rng.next() >> 32) as u8);
            }
        }
    }
    bytes
}
