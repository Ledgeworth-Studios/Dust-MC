//! SHA-1, for verifying a downloaded server jar against the digest Mojang
//! publishes for it.
//!
//! Hand-written rather than taken from a crate. Sixty lines of a fully
//! specified algorithm, with the published test vectors beside it, is cheaper
//! than six transitive dependencies in a tree that is audited on every build —
//! and a verifier whose own correctness is asserted here is a better thing to
//! be trusting than one that is not.
//!
//! SHA-1 is broken for signatures. It is not being used for one: this checks
//! that a download over an untrusted network arrived intact and complete,
//! against a digest fetched separately over TLS. Mojang publishes SHA-1 and
//! that is what there is to compare against.

/// Streaming SHA-1, so a fifty-megabyte jar is not held in memory to hash it.
#[derive(Debug, Clone)]
pub struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    buffered: usize,
    length_bits: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            buffer: [0; 64],
            buffered: 0,
            length_bits: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length_bits = self.length_bits.wrapping_add(data.len() as u64 * 8);
        while !data.is_empty() {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    /// The digest, lowercase hex, as Mojang publishes it.
    pub fn finish(mut self) -> String {
        let length_bits = self.length_bits;
        self.update(&[0x80]);
        // The length field occupies the last eight bytes of the final block.
        while self.buffered != 56 {
            self.update(&[0]);
        }
        self.length_bits = length_bits;
        let block = {
            let mut block = self.buffer;
            block[56..].copy_from_slice(&length_bits.to_be_bytes());
            block
        };
        self.compress(&block);

        let mut hex = String::with_capacity(40);
        for word in self.state {
            hex.push_str(&format!("{word:08x}"));
        }
        hex
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

/// The SHA-1 of a byte slice, lowercase hex.
pub fn hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_test_vectors() {
        // FIPS 180-1 appendix A and B, plus the empty input.
        assert_eq!(hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn a_million_a_s() {
        // The third FIPS vector. It is the one that exercises the length field
        // past a single block and past 2^16 bits.
        let mut hasher = Sha1::new();
        for _ in 0..1000 {
            hasher.update(&[b'a'; 1000]);
        }
        assert_eq!(hasher.finish(), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn the_result_does_not_depend_on_how_the_input_was_chunked() {
        // The property that matters for a streamed download: bytes arriving in
        // whatever sizes the socket produced must hash the same as one slab.
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let once = hex(&data);
        for chunk in [1usize, 7, 63, 64, 65, 127] {
            let mut hasher = Sha1::new();
            for part in data.chunks(chunk) {
                hasher.update(part);
            }
            assert_eq!(hasher.finish(), once, "chunked by {chunk}");
        }
    }

    #[test]
    fn a_length_that_lands_exactly_on_a_block_boundary() {
        // 55, 56 and 64 bytes are where a padding implementation goes wrong:
        // 56 leaves no room for the length field and needs a second block.
        for length in [54usize, 55, 56, 57, 63, 64, 65] {
            let data = vec![b'x'; length];
            // Cross-checked against the streaming path chunked byte by byte,
            // which uses the same padding but a different buffering path.
            let mut hasher = Sha1::new();
            for byte in &data {
                hasher.update(&[*byte]);
            }
            assert_eq!(hasher.finish(), hex(&data), "length {length}");
        }
    }
}
