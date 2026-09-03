//! The random source worldgen is seeded from, and the hash that positions it.
//!
//! Minecraft's modern worldgen uses xoroshiro128++ rather than `java.util.Random`,
//! and it never seeds a noise from a counter. Every noise is seeded from a
//! *name*: the world seed is upgraded to 128 bits, forked into a positional
//! factory, and each noise asks that factory for the stream belonging to its own
//! resource location. The consequence is the one that matters here — the octave
//! a noise gets does not depend on how many noises were built before it, so a
//! generator that builds five of vanilla's noises and none of the other fifty
//! five gets the same five streams vanilla would.
//!
//! That is why this is reproduced exactly rather than approximated. Every
//! constant below is a bit pattern; there is no room to be nearly right, and a
//! single wrong rotate turns the whole world into a different world that still
//! looks like a world.

/// The xoroshiro128++ generator, seed and all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xoroshiro {
    lo: u64,
    hi: u64,
}

/// `0x9E3779B97F4A7C15`.
const GOLDEN_RATIO_64: u64 = 0x9E37_79B9_7F4A_7C15;
/// `0x6A09E667F3BCC909`.
const SILVER_RATIO_64: u64 = 0x6A09_E667_F3BC_C909;

impl Xoroshiro {
    /// The generator for an explicit 128-bit seed.
    ///
    /// An all-zero seed is replaced, because xoroshiro's state transition fixes
    /// zero: a zero state returns zero forever. Minecraft substitutes exactly
    /// these two words and so does this.
    pub fn from_parts(lo: u64, hi: u64) -> Self {
        if lo | hi == 0 {
            Self {
                lo: GOLDEN_RATIO_64,
                hi: SILVER_RATIO_64,
            }
        } else {
            Self { lo, hi }
        }
    }

    /// The generator for a world seed, upgraded to 128 bits the way Minecraft
    /// upgrades it.
    pub fn from_seed(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed_to_128_bit(seed as u64);
        Self::from_parts(lo, hi)
    }

    pub fn next_u64(&mut self) -> u64 {
        let lo = self.lo;
        let mut hi = self.hi;
        let out = lo.wrapping_add(hi).rotate_left(17).wrapping_add(lo);
        hi ^= lo;
        self.lo = lo.rotate_left(49) ^ hi ^ (hi << 21);
        self.hi = hi.rotate_left(28);
        out
    }

    /// Java's `XoroshiroRandomSource.nextInt()`: the **low** 32 bits.
    ///
    /// Not `next(32)`, which is the high 32 and is what the legacy source
    /// returns. The two differ on every draw and both produce a plausible
    /// world, so nothing but a comparison against Minecraft's own can tell them
    /// apart — see the biome scores in decision record 0020.
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    /// Java's `nextInt(bound)` — Lemire's multiply-shift with the rejection
    /// loop that makes it unbiased.
    ///
    /// The loop is not decoration. It is entered for some draws and not others,
    /// and it consumes a draw when it is, so a version of this that skipped it
    /// would agree with Minecraft on most permutations and disagree on some —
    /// which is the worst of the two ways to be wrong, because the world would
    /// look right until it did not.
    pub fn next_i32_below(&mut self, bound: i32) -> i32 {
        debug_assert!(bound > 0, "bound must be positive");
        let bound = u64::from(bound as u32);
        let mut draw = u64::from(self.next_u32());
        let mut product = draw.wrapping_mul(bound);
        let mut low = product & 0xFFFF_FFFF;
        if low < bound {
            // `Integer.remainderUnsigned(-bound, bound)`: the largest low word
            // that has to be thrown away for the mapping to stay uniform.
            let threshold = (bound.wrapping_neg() & 0xFFFF_FFFF) % bound;
            while low < threshold {
                draw = u64::from(self.next_u32());
                product = draw.wrapping_mul(bound);
                low = product & 0xFFFF_FFFF;
            }
        }
        (product >> 32) as i32
    }

    /// Java's `nextDouble()`: 53 bits scaled into `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * 1.1102230246251565E-16
    }

    /// Split off the positional factory this stream seeds.
    ///
    /// Consumes two draws, which is why the order noises are *built* in still
    /// matters inside one `NormalNoise` even though the order they are named in
    /// does not.
    pub fn fork_positional(&mut self) -> Positional {
        Positional {
            lo: self.next_u64(),
            hi: self.next_u64(),
        }
    }
}

/// A factory that turns a name into a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Positional {
    lo: u64,
    hi: u64,
}

impl Positional {
    /// The stream belonging to `name` — an MD5 of the name, xored into this
    /// factory's own seed.
    pub fn from_hash_of(&self, name: &str) -> Xoroshiro {
        let (lo, hi) = seed_from_hash_of(name);
        Xoroshiro::from_parts(lo ^ self.lo, hi ^ self.hi)
    }
}

/// Minecraft's 64-to-128-bit seed upgrade.
fn upgrade_seed_to_128_bit(seed: u64) -> (u64, u64) {
    let lo = seed ^ SILVER_RATIO_64;
    let hi = lo.wrapping_add(GOLDEN_RATIO_64);
    (mix_stafford_13(lo), mix_stafford_13(hi))
}

/// Stafford variant 13 of the MurmurHash3 finaliser, as `RandomSupport` uses it.
fn mix_stafford_13(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// The 128-bit seed of a name: MD5 of its UTF-8 bytes, read as two big-endian
/// words.
fn seed_from_hash_of(name: &str) -> (u64, u64) {
    let digest = md5(name.as_bytes());
    let lo = u64::from_be_bytes(digest[..8].try_into().expect("eight bytes"));
    let hi = u64::from_be_bytes(digest[8..].try_into().expect("eight bytes"));
    (lo, hi)
}

// ---------------------------------------------------------------------------
// MD5
// ---------------------------------------------------------------------------

/// MD5, because the seed of every noise is one.
///
/// Not a security primitive and not offered as one — it is here because
/// Minecraft's noise names hash through it and the bytes have to match. It is
/// private to this module for that reason.
fn md5(message: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] = std::array::from_fn(|i| {
        // The integer part of |sin(i + 1)| scaled by 2^32, which is how the
        // constants are defined rather than a table somebody typed.
        ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32
    });

    let mut state: [u32; 4] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];

    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64) * 8).to_le_bytes());

    for block in padded.chunks_exact(64) {
        let words: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().expect("four bytes"))
        });
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (mixed, index) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let sum = a
                .wrapping_add(mixed)
                .wrapping_add(k[i])
                .wrapping_add(words[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(S[i]));
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut out = [0u8; 16];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: [u8; 16]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_agrees_with_the_published_vectors() {
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        // Longer than one block, so the second-block path is exercised too.
        assert_eq!(
            hex(md5(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn the_names_worldgen_actually_hashes_seed_the_words_minecraft_reads() {
        // Both halves, and both big-endian: reading the digest the other way
        // round is the most likely single mistake here and it is silent.
        assert_eq!(
            seed_from_hash_of("minecraft:temperature"),
            (
                u64::from_be_bytes(md5(b"minecraft:temperature")[..8].try_into().unwrap()),
                u64::from_be_bytes(md5(b"minecraft:temperature")[8..].try_into().unwrap()),
            )
        );
        assert_ne!(
            seed_from_hash_of("octave_-10"),
            seed_from_hash_of("octave_-9"),
            "adjacent octaves must not share a stream"
        );
    }

    #[test]
    fn a_zero_seed_is_replaced_rather_than_left_to_fix_itself() {
        let mut stuck = Xoroshiro::from_parts(0, 0);
        assert_ne!(stuck.next_u64(), 0);
    }

    #[test]
    fn the_generator_is_a_function_of_its_seed_and_nothing_else() {
        let mut first = Xoroshiro::from_seed(0);
        let mut second = Xoroshiro::from_seed(0);
        let a: Vec<u64> = (0..8).map(|_| first.next_u64()).collect();
        let b: Vec<u64> = (0..8).map(|_| second.next_u64()).collect();
        assert_eq!(a, b);
        let mut other = Xoroshiro::from_seed(1);
        assert_ne!(a[0], other.next_u64());
    }

    #[test]
    fn next_int_below_stays_inside_its_bound() {
        let mut random = Xoroshiro::from_seed(42);
        for bound in 1..=256 {
            for _ in 0..16 {
                let drawn = random.next_i32_below(bound);
                assert!((0..bound).contains(&drawn), "{drawn} outside 0..{bound}");
            }
        }
    }

    #[test]
    fn next_double_stays_in_the_unit_interval() {
        let mut random = Xoroshiro::from_seed(-1);
        for _ in 0..1024 {
            let drawn = random.next_f64();
            assert!((0.0..1.0).contains(&drawn), "{drawn} outside 0..1");
        }
    }
}
