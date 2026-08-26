//! AES-128-CFB8 over the socket, switched on halfway through login.
//!
//! # The mode
//!
//! Minecraft encrypts with AES-128 in **CFB8**, and sets the key and the IV to
//! the *same* sixteen bytes: the shared secret the client generated and sent
//! under the server's RSA key. That is unusual on both counts. CFB8 is a
//! stream mode that runs a full AES block encryption **per byte of plaintext**
//! — sixteen times the work of CTR for the same data — and reusing the key as
//! the IV means the keystream is a deterministic function of the key alone.
//! Neither is a decision this crate gets to revisit; both are what a vanilla
//! client does, and a server that does anything else is a server no client can
//! talk to.
//!
//! Because a CFB8 block is one byte, there is no padding, no block alignment
//! and no notion of a partial block. A connection can be encrypted starting at
//! any byte, which is what makes the mid-stream switch below possible at all.
//!
//! # The switch, which is where the bug is
//!
//! Encryption starts partway through login: the server sends Encryption
//! Request in the clear, the client replies with Encryption Response in the
//! clear, and **every byte after that response is encrypted, in both
//! directions**. So there is exactly one byte position in the connection's
//! life where the codec changes, and getting it wrong has two failure modes
//! that look nothing alike:
//!
//! * **Bytes lost.** The reader had already pulled bytes past the Encryption
//!   Response into a buffer and treated them as plaintext. They were
//!   ciphertext. The connection then desynchronises somewhere later, with no
//!   error at the place the mistake happened.
//! * **Bytes encrypted twice.** The writer had a frame queued when the switch
//!   happened and the queue is drained through the new cipher, or the same
//!   bytes pass the cipher on the way into a buffer and again on the way out.
//!
//! Neither is caught by a test that encrypts a buffer and decrypts it again.
//! `tests/encryption_switch.rs` drives the real transition over an in-memory
//! stream and checks that the frames after the switch arrive intact —
//! including the case where the peer pipelines encrypted frames into the same
//! write as the Encryption Response, which is the case an honest client does
//! not produce and a hostile one does.
//!
//! [`crate::io`] handles the read side by never decrypting further than the
//! frame it is currently assembling: the length prefix is decrypted a byte at
//! a time and the body in one bulk call, so nothing past the current frame is
//! ever fed through the cipher speculatively. The switch is then simply a
//! change that takes effect on the next byte, and there is no "already
//! decrypted the wrong thing" to unwind.
//!
//! # What this does not catch
//!
//! CFB8 provides confidentiality and **no integrity whatsoever**. An attacker
//! who can modify bytes in flight can flip bits in the plaintext at the cost
//! of corrupting the following sixteen bytes, and nothing in this module will
//! notice. That is a property of the Minecraft protocol, not of this
//! implementation, and it is why the frame decoder's structural checks matter
//! as much on an encrypted connection as on a plain one.
//!
//! Nor does this module authenticate anybody. It takes a sixteen-byte secret
//! and uses it. Whether that secret came from a client who is who they claim
//! to be is [`crate::login`]'s question, and the part of that question that
//! needs Mojang's session server is not answered anywhere in this crate.

use aes::cipher::KeyIvInit as _;
use aes::Aes128;

/// The length of the shared secret, which is also the key length and the IV
/// length. AES-128, so sixteen bytes.
pub const SHARED_SECRET_LEN: usize = 16;

type Encryptor = cfb8::Encryptor<Aes128>;
type Decryptor = cfb8::Decryptor<Aes128>;

/// The symmetric key for one connection.
///
/// A named type rather than `[u8; 16]` so that the key and the verify token —
/// both short byte arrays that arrive in the same packet — cannot be passed to
/// each other's function.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SharedSecret([u8; SHARED_SECRET_LEN]);

impl SharedSecret {
    pub fn from_bytes(bytes: [u8; SHARED_SECRET_LEN]) -> Self {
        Self(bytes)
    }

    /// Read a secret from a slice, which is how it arrives: as the plaintext
    /// of an RSA decryption, whose length is not known to the type system.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, BadSecretLength> {
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| BadSecretLength { got: bytes.len() })
    }

    pub fn as_bytes(&self) -> &[u8; SHARED_SECRET_LEN] {
        &self.0
    }
}

// Deriving `Debug` would put the key in every log line that formats a
// connection. This prints the type and nothing else.
impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedSecret(<redacted>)")
    }
}

/// A shared secret of the wrong length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadSecretLength {
    pub got: usize,
}

impl std::fmt::Display for BadSecretLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the shared secret is {} bytes; AES-128 needs exactly {SHARED_SECRET_LEN}",
            self.got
        )
    }
}

impl std::error::Error for BadSecretLength {}

/// The cipher state for one connection, in both directions.
///
/// Off until [`enable`](Self::enable) is called, and then on for the rest of
/// the connection. There is no way to turn it off again, because the protocol
/// has no way to say so and an API that offers it invites a downgrade.
///
/// The two directions have **independent** cipher state despite sharing a key.
/// They have to: each direction's CFB8 feedback register is the last sixteen
/// bytes *that direction* sent, and the directions do not send the same bytes.
/// Sharing one state between them is a bug that survives every test with
/// traffic in one direction at a time.
pub struct Cipher {
    outgoing: Option<Encryptor>,
    incoming: Option<Decryptor>,
}

// The cipher types hold key material and do not implement `Debug`. This says
// only whether encryption is on, which is the part worth logging.
impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cipher")
            .field("enabled", &self.is_enabled())
            .finish()
    }
}

impl Cipher {
    /// A connection with encryption off.
    pub fn disabled() -> Self {
        Self {
            outgoing: None,
            incoming: None,
        }
    }

    /// Turn encryption on from the next byte in each direction.
    ///
    /// Both directions are switched together and there is no way to switch one
    /// alone, because the protocol changes both at the same point and an API
    /// that allowed half a switch would let a caller produce a connection that
    /// cannot be recovered.
    pub fn enable(&mut self, secret: &SharedSecret) {
        let key = secret.as_bytes();
        // Key and IV are the same bytes. This is the protocol's choice.
        self.outgoing = Some(Encryptor::new(key.into(), key.into()));
        self.incoming = Some(Decryptor::new(key.into(), key.into()));
    }

    pub fn is_enabled(&self) -> bool {
        self.outgoing.is_some()
    }

    /// Encrypt bytes on their way to the socket, in place.
    ///
    /// A no-op while encryption is off, so callers do not branch. The
    /// alternative — a caller that checks `is_enabled` first — is a caller
    /// that can forget, and forgetting sends plaintext on an encrypted
    /// connection.
    pub fn encrypt(&mut self, bytes: &mut [u8]) {
        if let Some(cipher) = self.outgoing.as_mut() {
            cipher.encrypt(bytes);
        }
    }

    /// Decrypt bytes on their way from the socket, in place.
    ///
    /// Must be given every byte read from the socket after the switch, exactly
    /// once, in order. CFB8's state is a function of the byte stream, so a
    /// byte fed twice or out of order corrupts everything after it and there
    /// is no error to observe — the bytes are simply wrong from then on.
    pub fn decrypt(&mut self, bytes: &mut [u8]) {
        if let Some(cipher) = self.incoming.as_mut() {
            cipher.decrypt(bytes);
        }
    }
}

impl Default for Cipher {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hex, for the vectors below.
    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// AES-128-CFB8 from NIST SP 800-38A, appendix F.3.7.
    ///
    /// **The outside check.** A cipher tested against itself is self-consistent
    /// and possibly wrong, and CFB8 is a mode with several plausible ways to be
    /// subtly wrong — feeding back plaintext instead of ciphertext, shifting
    /// the register the wrong way, taking the low byte of the block output
    /// instead of the high one. Every one of those produces an implementation
    /// that decrypts its own ciphertext perfectly.
    ///
    /// This vector was not taken from memory. It was produced twice, by two
    /// implementations that are not this one and not each other:
    ///
    /// ```text
    /// openssl enc -aes-128-cfb8 -K 2b7e151628aed2a6abf7158809cf4f3c \
    ///     -iv 000102030405060708090a0b0c0d0e0f -in pt.bin | xxd -p
    /// ```
    ///
    /// and `javax.crypto`'s `AES/CFB8/NoPadding` on the JVM — which is the
    /// implementation Minecraft itself runs. They agree, and they agree with
    /// the published NIST vector.
    const NIST_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const NIST_IV: &str = "000102030405060708090a0b0c0d0e0f";
    const NIST_PLAIN: &str = "6bc1bee22e409f96e93d7e117393172aae2d";
    const NIST_CIPHER: &str = "3b79424c9c0dd436bace9e0ed4586a4f32b9";

    /// The same, in Minecraft's shape: key and IV are one secret.
    ///
    /// Generated by the same two external implementations. It matters
    /// separately from the NIST vector because key-equals-IV is the case this
    /// crate always runs in, and a wrapper that swapped its key and IV
    /// arguments would pass every key-equals-IV test ever written — so the
    /// NIST vector, where they differ, is what pins the argument order, and
    /// this one is what pins the wiring [`Cipher::enable`] actually uses.
    const MC_SECRET: &str = "0102030405060708090a0b0c0d0e0f10";
    const MC_PLAIN: &[u8] = b"Dust dust-net CFB8 vector";
    const MC_CIPHER: &str = "70893fa9e7988e00faee408c24ff1d07b1f2b097841d29f7d3";

    /// Two hundred bytes, which is more than twelve AES blocks. A CFB8
    /// implementation that is right for the first sixteen bytes and wrong once
    /// the feedback register has fully turned over passes the two vectors
    /// above and fails this one.
    const MC_LONG_CIPHER: &str = "373dea276d9291e3fc4b2f9d662fe2b2bef0acd13e94069658528afd6a3863bd\
581838afc76357e460d890e3ace5f0ffc93c280967ac9108bf731b74ebd8b4d4929f1b5208719934099be4a7b78905ca\
76cf5594101cdd3e4b123225c9db3550eaa65da24df405c56e90968320e2ddbb1bb2afaa3ba7f3a0eebffed82fcabd08\
fa301454fc205bf58b465b89d25cf5e0e2f5f7cb4c2375be3bd49811a475ada8f7394f899b5077186899d639b432d30c\
febb429d8221142009cdf081bb16407a182d65f97a58a545";

    fn long_plain() -> Vec<u8> {
        (0..200u32).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn the_raw_mode_matches_the_nist_vector() {
        // Straight at the underlying mode, with key and IV different, so that
        // the argument order is pinned by something other than symmetry.
        let key = unhex(NIST_KEY);
        let iv = unhex(NIST_IV);
        let mut buffer = unhex(NIST_PLAIN);
        Encryptor::new(
            key.as_slice().try_into().expect("16 bytes"),
            iv.as_slice().try_into().expect("16 bytes"),
        )
        .encrypt(&mut buffer);
        assert_eq!(buffer, unhex(NIST_CIPHER));

        Decryptor::new(
            key.as_slice().try_into().expect("16 bytes"),
            iv.as_slice().try_into().expect("16 bytes"),
        )
        .decrypt(&mut buffer);
        assert_eq!(buffer, unhex(NIST_PLAIN));
    }

    #[test]
    fn the_connection_cipher_matches_the_external_vector() {
        // Through the public API, which is what the socket path uses.
        let secret = SharedSecret::from_slice(&unhex(MC_SECRET)).expect("16 bytes");
        let mut cipher = Cipher::disabled();
        cipher.enable(&secret);

        let mut buffer = MC_PLAIN.to_vec();
        cipher.encrypt(&mut buffer);
        assert_eq!(buffer, unhex(MC_CIPHER));
    }

    #[test]
    fn the_feedback_register_is_right_past_the_first_block() {
        let secret = SharedSecret::from_slice(&unhex(MC_SECRET)).expect("16 bytes");
        let mut cipher = Cipher::disabled();
        cipher.enable(&secret);

        let mut buffer = long_plain();
        cipher.encrypt(&mut buffer);
        assert_eq!(buffer, unhex(MC_LONG_CIPHER));
    }

    #[test]
    fn the_stream_continues_across_calls() {
        // The property the socket path depends on and a one-shot vector does
        // not test: encrypting a buffer in pieces must produce the same bytes
        // as encrypting it whole. If `Cipher` reset its state per call, this
        // is where it shows, and every round-trip test would still pass
        // because the decryptor would reset in step.
        let secret = SharedSecret::from_slice(&unhex(MC_SECRET)).expect("16 bytes");
        let expected = unhex(MC_LONG_CIPHER);

        for chunk in [1usize, 3, 16, 17, 64, 199] {
            let mut cipher = Cipher::disabled();
            cipher.enable(&secret);
            let mut out = Vec::new();
            for piece in long_plain().chunks(chunk) {
                let mut piece = piece.to_vec();
                cipher.encrypt(&mut piece);
                out.extend_from_slice(&piece);
            }
            assert_eq!(out, expected, "encrypting in chunks of {chunk}");
        }
    }

    #[test]
    fn the_two_directions_have_independent_state() {
        // One shared cipher state between the directions passes every test
        // that sends traffic one way at a time. This interleaves them: if the
        // states were shared, the second encrypt would continue from the
        // decrypt's feedback register and the bytes would not match.
        let secret = SharedSecret::from_slice(&unhex(MC_SECRET)).expect("16 bytes");
        let mut server = Cipher::disabled();
        server.enable(&secret);
        let mut client = Cipher::disabled();
        client.enable(&secret);

        for round in 0..8u8 {
            let mut down = vec![round; 40];
            server.encrypt(&mut down);
            client.decrypt(&mut down);
            assert_eq!(down, vec![round; 40], "server to client, round {round}");

            let mut up = vec![round.wrapping_add(0x80); 40];
            client.encrypt(&mut up);
            server.decrypt(&mut up);
            assert_eq!(
                up,
                vec![round.wrapping_add(0x80); 40],
                "client to server, round {round}"
            );
        }
    }

    #[test]
    fn a_disabled_cipher_is_the_identity() {
        // Callers do not branch on `is_enabled`, so "off" has to mean "leaves
        // the bytes alone" rather than "must not be called".
        let mut cipher = Cipher::disabled();
        let mut buffer = b"plaintext before the switch".to_vec();
        let original = buffer.clone();
        cipher.encrypt(&mut buffer);
        assert_eq!(buffer, original);
        cipher.decrypt(&mut buffer);
        assert_eq!(buffer, original);
        assert!(!cipher.is_enabled());
    }

    #[test]
    fn the_switch_leaves_earlier_bytes_alone() {
        // The transition, at the level this module owns it: bytes handed over
        // before `enable` are untouched, bytes handed over after are
        // encrypted, and the ciphertext of the second half is the same as if
        // the stream had started there. That last clause is what says the
        // cipher is not carrying state from the plaintext prefix.
        let secret = SharedSecret::from_slice(&unhex(MC_SECRET)).expect("16 bytes");

        let mut cipher = Cipher::disabled();
        let mut before = b"handshake and login start".to_vec();
        cipher.encrypt(&mut before);
        assert_eq!(before, b"handshake and login start");

        cipher.enable(&secret);
        let mut after = MC_PLAIN.to_vec();
        cipher.encrypt(&mut after);
        assert_eq!(after, unhex(MC_CIPHER));
    }

    #[test]
    fn a_secret_of_the_wrong_length_is_refused() {
        for length in [0usize, 1, 15, 17, 32, 128] {
            assert_eq!(
                SharedSecret::from_slice(&vec![0u8; length]),
                Err(BadSecretLength { got: length }),
                "{length} bytes"
            );
        }
        assert!(SharedSecret::from_slice(&[0u8; 16]).is_ok());
    }

    #[test]
    fn the_secret_does_not_print_itself() {
        // A key in a log file is a key on disk, forever, in a place nobody
        // thinks of as a key store.
        let secret = SharedSecret::from_bytes([0xAB; 16]);
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("ab"), "{rendered}");
        assert!(!rendered.contains("171"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }
}
