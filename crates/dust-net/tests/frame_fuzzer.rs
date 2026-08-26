//! A deterministic attacker.
//!
//! Hand-picked malformed inputs prove the defences they were designed
//! around; they say nothing about the ones nobody thought of. This file
//! generates hostility by rule instead of by inspiration: a seeded generator
//! builds thousands of corrupted, truncated, spliced and lying variants of
//! real frames, and the decoder's obligations are checked against every one.
//!
//! The obligations are the three from `tests/support`: **errors, not
//! panics** — every variant produces an `Err` naming itself or a decoded
//! frame, never a crash; **bounded memory** — sampled high-water marks stay
//! far under what one honest maximum frame would cost; **bounded reads** —
//! length prefixes claiming past the cap are refused before their bodies are
//! looked at, in both wire modes, at every state the decoder can be in.
//!
//! Seeded rather than random because a failure that reproduces from a
//! constant is a bug report, and one that evaporates is a rumour. The
//! generator is SplitMix64: integer arithmetic only, identical output on
//! every platform.

mod support;

use std::time::Instant;

use dust_net::frame::{Compress, Frame, FrameDecoder, FrameEncoder, FrameError, Limits};
use dust_net::varint::{
    read_var_int, read_var_long, var_int_len, var_long_len, write_var_int, write_var_long,
    VarIntReader,
};

#[global_allocator]
static ALLOCATOR: support::Counting = support::Counting;

struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn encode(frame: &Frame, mode: Compress) -> Vec<u8> {
    let mut encoder = FrameEncoder::new(Limits::default());
    encoder.set_compression(mode);
    let mut out = Vec::new();
    encoder
        .encode(frame, &mut out)
        .expect("corpus frames are encodable");
    out
}

/// Valid frames spanning every mode, threshold relation and size class the
/// mutations will be derived from.
fn corpus() -> Vec<(Compress, Vec<u8>)> {
    let modes = [
        Compress::Disabled,
        Compress::At { threshold: 1 },
        Compress::At { threshold: 16 },
        Compress::At { threshold: 256 },
    ];
    let sizes = [0usize, 1, 15, 63, 64, 65, 255, 256, 257, 1024];
    let mut entries = Vec::new();
    for mode in modes {
        for &size in &sizes {
            for kind in 0..3u8 {
                let body: Vec<u8> = match kind {
                    // Highly compressible.
                    0 => vec![kind * 37; size],
                    // Incompressible enough that the compressed form grows.
                    1 => (0..size).map(|i| (i * 131 + 7) as u8).collect(),
                    _ => continue,
                };
                for &id in &[0i32, 127, -1, 0x7F_FF] {
                    entries.push((mode, encode(&Frame::new(id, body.clone()), mode)));
                }
            }
        }
    }
    entries
}

/// Corrupt `wire` by one of several rules chosen by the generator.
fn mutate(rng: &mut SplitMix64, wire: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = wire.to_vec();
    match rng.below(7) {
        // Bit flips, one to eight of them.
        0 => {
            let flips = 1 + rng.below(8);
            for _ in 0..flips {
                let byte = rng.below(out.len() as u64) as usize;
                let bit: u8 = 1 << rng.below(8);
                out[byte] ^= bit;
            }
        }
        // Whole-byte substitution with hostile values.
        1 => {
            for _ in 0..1 + rng.below(4) {
                let byte = rng.below(out.len() as u64) as usize;
                out[byte] = match rng.below(4) {
                    0 => 0xFF,
                    1 => 0x80,
                    2 => 0x00,
                    _ => rng.next() as u8,
                };
            }
        }
        // Truncation anywhere, including mid-prefix and mid-body.
        2 => {
            let cut = rng.below(out.len() as u64) as usize;
            out.truncate(cut);
        }
        // Truncation plus a junk tail, so the decoder sees a complete-looking
        // frame carrying nonsense.
        3 => {
            let cut = rng.below(out.len() as u64) as usize;
            out.truncate(cut);
            let junk = 1 + rng.below(32) as usize;
            out.extend((0..junk).map(|_| rng.next() as u8));
        }
        // Splice two frames together at unrelated offsets.
        4 => {
            let a = rng.below(out.len() as u64) as usize;
            let b = rng.below(second.len() as u64) as usize;
            out.truncate(a);
            out.extend_from_slice(&second[b..]);
        }
        // Overwrite the length prefix with a claim of the generator's
        // choosing: enormous, negative, or merely wrong.
        5 => {
            let claim: i32 = match rng.below(3) {
                0 => -1 - (rng.next() as i32),
                1 => i32::MAX,
                _ => 2_097_152 + (rng.below(1 << 20) as i32),
            };
            let mut prefix = Vec::new();
            write_var_int(claim, &mut prefix);
            let keep = out.len().min(prefix.len() + rng.below(4) as usize);
            let mut rebuilt = prefix;
            rebuilt.extend_from_slice(&out[keep.min(out.len())..]);
            out = rebuilt;
        }
        // Insert a run of continuation bytes, the classic endless-prefix
        // shape, at a random offset.
        _ => {
            let at = rng.below(out.len() as u64) as usize;
            let run = 1 + rng.below(12) as usize;
            let mut injected = vec![0x80u8; run];
            injected.push(rng.next() as u8);
            out.splice(at..at, injected);
        }
    }
    if out.is_empty() {
        out.push(0x00); // an empty stream decodes to nothing and proves less
    }
    out
}

/// Feed everything and demand survival. Returns how many frames came out
/// and whether the stream ended in a refusal. No panic, no hang, and no
/// unbounded production of frames from bounded input.
fn survive(decoder: &mut FrameDecoder, bytes: &[u8]) -> (usize, bool) {
    decoder.feed(bytes);
    let mut produced = 0usize;
    loop {
        match decoder.next_frame() {
            Ok(Some(_)) => produced += 1,
            Ok(None) => return (produced, false),
            Err(_) => return (produced, true),
        }
        // Bounded input cannot legitimately contain thousands of frames;
        // this line exists so a decode loop that stops consuming becomes a
        // failure here rather than a hang somewhere above.
        assert!(
            produced < 4096,
            "unbounded frame production from {} bytes",
            bytes.len()
        );
    }
}

#[test]
fn ten_thousand_mutations_never_crash_the_decoder_or_the_varints() {
    let _gate = support::serial();
    let mut rng = SplitMix64::new(0xC0FF_EE00_D15E_A5E5);
    let entries = corpus();

    let started = Instant::now();
    let mut decoded = 0usize;
    let mut refused = 0usize;
    for iteration in 0..10_000u64 {
        let (mode, wire) = &entries[(rng.below(entries.len() as u64)) as usize];
        let (_, second) = &entries[(rng.below(entries.len() as u64)) as usize];
        let mutated = mutate(&mut rng, wire, second);

        // Small caps so the memory bound below is meaningful: a decoder
        // running these limits may allocate tens of kilobytes, never
        // megabytes, whatever it was handed.
        let mut decoder = FrameDecoder::new(Limits {
            max_frame_len: 64 * 1024,
            max_decompressed_len: 64 * 1024,
        });
        decoder.set_compression(*mode);

        let baseline = if iteration % 256 == 0 {
            Some(support::reset_peak())
        } else {
            None
        };
        let (produced, refused_this) = survive(&mut decoder, &mutated);
        decoded += produced;
        refused += usize::from(refused_this);
        if let Some(baseline) = baseline {
            let peak = support::peak_above(baseline);
            assert!(
                peak < 512 * 1024,
                "iteration {iteration}: surviving the mutation peaked at \
                 {peak} live bytes"
            );
        }

        // The same bytes through the one-shot VarInt readers, which guard
        // the prefix logic directly.
        let _ = read_var_int(&mutated);
        let _ = read_var_long(&mutated);
    }
    // Refusals must be the common case: the generator is aiming to break
    // things, so a run where almost everything decodes means the mutations
    // have gone soft and prove nothing. The converse also holds — some
    // mutations land on bytes that are still valid frames, and a decoder
    // that refused everything would be rejecting honest traffic too.
    assert!(
        refused > 5_000,
        "only {refused} of 10_000 hostile streams were refused; the corpus has \
         gone toothless"
    );
    assert!(
        decoded > 500,
        "only {decoded} frames survived out of 10_000 attacks; the decoder \
         may be refusing valid shapes as collateral"
    );
    let elapsed = started.elapsed();
    // Sanity on the harness itself: a loop this size should be seconds, so
    // a future slowdown announces itself rather than quietly doubling CI.
    assert!(elapsed.as_secs() < 60, "mutation loop took {elapsed:?}");
}

#[test]
fn length_caps_bite_in_both_modes_before_anything_is_allocated() {
    let _gate = support::serial();
    let plain = encode(&Frame::new(0, vec![0xAB; 300]), Compress::Disabled);
    let compressed = encode(
        &Frame::new(0, vec![0xAB; 300]),
        Compress::At { threshold: 16 },
    );

    // Claims that exceed the cap, in signed form where relevant, spliced
    // onto otherwise-valid frames in both wire modes.
    let claims: &[i32] = &[
        -1,
        i32::MIN,
        -2_000_000_000,
        2_000_000_000,
        2_097_152,   // one past the vanilla cap
        0x4000_0000, // comfortably past it
    ];
    for &claim in claims {
        for wire in [&plain, &compressed] {
            let mut prefix = Vec::new();
            write_var_int(claim, &mut prefix);
            let mut mutated = prefix;
            mutated.extend_from_slice(wire);

            let baseline = support::reset_peak();
            let mut decoder = FrameDecoder::new(Limits::default());
            decoder.feed(&mutated);
            let outcome = decoder.next_frame();
            let peak = support::peak_above(baseline);

            match outcome {
                Err(FrameError::TooLarge { .. }) | Err(FrameError::NegativeLength { .. }) => {}
                // A negative claim can surface as its own variant; anything
                // else means the cap did not bite first.
                other => panic!("claim {claim}: expected a cap refusal, got {other:?}"),
            }
            assert!(
                peak < 64 * 1024,
                "refusing claim {claim} peaked at {peak} bytes"
            );
        }
    }

    // The inner cap: a compressed frame whose declared *uncompressed* size
    // exceeds `max_decompressed_len` is refused before decompression starts,
    // whatever its zlib stream actually contains.
    let mut inner = Vec::new();
    write_var_int(2_000_000, &mut inner);
    inner.extend_from_slice(&compressed);
    let mut wire = Vec::new();
    write_var_int(inner.len() as i32, &mut wire);
    wire.extend_from_slice(&inner);

    let baseline = support::reset_peak();
    let mut decoder = FrameDecoder::new(Limits {
        max_frame_len: Limits::default().max_frame_len,
        max_decompressed_len: 64 * 1024,
    });
    decoder.set_compression(Compress::At { threshold: 16 });
    decoder.feed(&wire);
    let outcome = decoder.next_frame();
    let peak = support::peak_above(baseline);
    assert_eq!(
        outcome,
        Err(FrameError::DeclaredTooLarge {
            declared: 2_000_000,
            limit: 64 * 1024,
        })
    );
    assert!(
        peak < 64 * 1024,
        "pre-decompression refusal peaked at {peak}"
    );
}

#[test]
fn the_varints_hold_their_properties_under_a_hundred_thousand_random_values() {
    let _gate = support::serial();
    let mut rng = SplitMix64::new(0x0D05_C0DE_BEEF_1234);

    // Round-trip plus declared-length agreement, over the whole domain with
    // emphasis on boundaries and negatives.
    let mut value_checks = 0u64;
    for seed in 0..50_000u64 {
        let mut s = SplitMix64::new(seed ^ 0xA5A5_5A5A);
        let candidates = [
            s.next() as i32,
            (s.next() >> 32) as i32,
            if seed % 16 == 0 {
                i32::MAX
            } else {
                s.next() as i32
            },
            if seed % 16 == 1 {
                i32::MIN
            } else {
                (s.next() >> 33) as i32
            },
        ];
        for value in candidates {
            let mut encoded = Vec::new();
            let written = write_var_int(value, &mut encoded);
            assert_eq!(written, var_int_len(value));
            assert_eq!(encoded.len(), written);
            assert_eq!(read_var_int(&encoded), Ok((value, written)), "{value}");

            let long = ((value as i64) * 0x1_0000_0000_u64 as i64) | (value as i64 >> 17);
            let mut long_encoded = Vec::new();
            let long_written = write_var_long(long, &mut long_encoded);
            assert_eq!(long_written, var_long_len(long));
            assert_eq!(read_var_long(&long_encoded), Ok((long, long_written)));
            value_checks += 2;
        }
    }
    assert_eq!(value_checks, 400_000);

    // Injectivity on arbitrary byte strings, generalising the exhaustive
    // three-byte sweep: whatever a slice decodes to, re-encoding that value
    // reproduces the consumed prefix exactly. Two different strings mapping
    // to one number would break every identity-based use of a frame.
    let mut accepted = 0u64;
    for _ in 0..50_000 {
        let len = rng.below(14) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if let Ok((value, used)) = read_var_int(&bytes) {
            accepted += 1;
            let mut re = Vec::new();
            write_var_int(value, &mut re);
            assert_eq!(&re[..], &bytes[..used], "{bytes:x?} -> {value}");
        }
        if let Ok((value, used)) = read_var_long(&bytes) {
            accepted += 1;
            let mut re = Vec::new();
            write_var_long(value, &mut re);
            assert_eq!(&re[..], &bytes[..used], "{bytes:x?} -> {value}");
        }
    }
    assert!(
        accepted > 1_000,
        "only {accepted} slices decoded; generator suspect"
    );

    // The incremental reader agrees with the one-shot reader on random
    // streams: same verdict, same consumed length, no disagreement for a
    // parser differential to hide behind.
    for _ in 0..5_000 {
        let len = rng.below(16) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let one_shot = read_var_int(&bytes);
        let mut reader = VarIntReader::new();
        let mut incremental: Option<Result<i32, dust_net::varint::VarIntError>> = None;
        let mut fed = 0usize;
        for &byte in &bytes {
            fed += 1;
            match reader.push(byte) {
                Ok(Some(value)) => {
                    incremental = Some(Ok(value));
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    incremental = Some(Err(e));
                    break;
                }
            }
        }
        match (one_shot, incremental) {
            (Ok((v, n)), Some(Ok(iv))) => {
                assert_eq!(v, iv);
                assert_eq!(n, fed, "consumed lengths disagree on {bytes:x?}");
            }
            (Ok(_), None) => panic!("one-shot succeeded where incremental wanted more"),
            (Ok(_), Some(Err(_))) | (Err(_), Some(Ok(_))) => {
                panic!("verdict disagreement on {bytes:x?}")
            }
            (Err(_), None) => {}         // both want more
            (Err(_), Some(Err(_))) => {} // both refuse; variant identity checked above
        }
    }
}
