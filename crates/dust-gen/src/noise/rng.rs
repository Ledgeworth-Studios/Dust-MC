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
    /// apart — see the biome scores in decision record 0021.
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

    /// Java's `nextFloat()`: 24 bits, and **an `f32` before it is anything
    /// else**.
    ///
    /// The width is the whole point. `vertical_gradient` compares this against
    /// a chance computed in `f64`, and a 53-bit draw would agree with
    /// Minecraft about the bedrock roof almost everywhere and disagree at the
    /// edges — the shape of wrong this project keeps finding.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * 5.9604645E-8
    }

    /// Java's `nextBoolean()`: the low bit of a whole draw, not a comparison.
    pub fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 != 0
    }

    /// Java's `nextIntBetweenInclusive(min, max)`, both ends included.
    pub fn next_i32_between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        min + self.next_i32_below(max - min + 1)
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

    /// The stream belonging to a **position**, which is what a surface rule
    /// rolls its dice from.
    ///
    /// Only the low word is disturbed: the high word is the factory's own, so
    /// two factories forked from different names give different worlds at the
    /// same block.
    pub fn at(&self, x: i32, y: i32, z: i32) -> Xoroshiro {
        Xoroshiro::from_parts(position_seed(x, y, z) ^ self.lo, self.hi)
    }
}

/// `Mth.getSeed`: three coordinates folded into one word.
///
/// The shift is arithmetic and the multiply wraps, both deliberately — this is
/// a hash and not an arithmetic identity.
pub fn position_seed(x: i32, y: i32, z: i32) -> u64 {
    let mut seed = (i64::from(x.wrapping_mul(3129871))
        ^ (i64::from(z).wrapping_mul(116129781))
        ^ i64::from(y)) as i64;
    seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42317861)
        .wrapping_add(seed.wrapping_mul(11));
    (seed >> 16) as u64
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

/// Minecraft's "obfuscated" world seed: the first eight bytes of the SHA-256
/// of the seed, both read little-endian.
///
/// It is the seed the biome blur fiddles with, and it is *not* the world seed.
/// A blur run on the world seed itself would put biome edges in plausible but
/// different places, which is the kind of wrong that looks right.
pub fn obfuscate_seed(seed: i64) -> i64 {
    let digest = sha256(&seed.to_le_bytes());
    i64::from_le_bytes(digest[..8].try_into().expect("eight bytes"))
}

/// SHA-256, because the biome zoom seed is one.
///
/// Here for the same reason [`md5`] is: not a security primitive, offered as
/// nobody's, and private to this module because the only thing that may depend
/// on it is a number Minecraft computed the same way.
fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());

    let mut words = [0u32; 64];
    for block in padded.chunks_exact(64) {
        for (index, word) in words.iter_mut().enumerate().take(16) {
            *word = u32::from_be_bytes(
                block[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("four bytes"),
            );
        }
        for index in 16..64 {
            let a = words[index - 15];
            let b = words[index - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let one = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let two = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(one);
            d = c;
            c = b;
            b = a;
            a = one.wrapping_add(two);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
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
    fn sha256_agrees_with_the_published_vectors() {
        let hex =
            |bytes: [u8; 32]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Longer than one block, so the second-block path is exercised too.
        assert_eq!(
            hex(sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_position_stream_is_a_function_of_the_position() {
        let factory = Xoroshiro::from_seed(7).fork_positional();
        assert_eq!(
            factory.at(3, -5, 9).next_u64(),
            factory.at(3, -5, 9).next_u64()
        );
        assert_ne!(
            factory.at(3, -5, 9).next_u64(),
            factory.at(3, -5, 10).next_u64()
        );
        // The y is folded in without being multiplied, so two positions one
        // block apart in y are the pair most likely to collide.
        assert_ne!(
            factory.at(0, 0, 0).next_u64(),
            factory.at(0, 1, 0).next_u64()
        );
    }

    #[test]
    fn next_float_is_twenty_four_bits_wide() {
        let mut random = Xoroshiro::from_seed(3);
        for _ in 0..1024 {
            let drawn = random.next_f32();
            assert!((0.0..1.0).contains(&drawn), "{drawn} outside 0..1");
            // Every draw is a multiple of 2^-24. A 53-bit draw narrowed to an
            // f32 would not be.
            assert_eq!(drawn, (drawn * 16777216.0).round() / 16777216.0);
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
