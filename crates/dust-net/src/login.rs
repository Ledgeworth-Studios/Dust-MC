//! The login key exchange: the server's RSA key pair, the verify token, and
//! decrypting what the client sends back.
//!
//! # What this module does
//!
//! A vanilla login in online mode runs like this, with the parts this module
//! owns marked:
//!
//! 1. Client sends Login Start.
//! 2. **Server sends Encryption Request**: a server id string, its RSA public
//!    key as DER, and a random verify token. [`ServerKey::generate`] makes the
//!    key, [`ServerKey::public_key_der`] encodes it, [`VerifyToken::generate`]
//!    makes the token.
//! 3. Client encrypts a shared secret it invented, and the verify token it was
//!    just given, each under the server's public key, and sends both back.
//! 4. **Server decrypts both.** [`ServerKey::decrypt_shared_secret`] and
//!    [`ServerKey::verify_token`]. The token check is what makes step 3 a
//!    challenge rather than a message: a replayed Encryption Response from an
//!    earlier session carries an earlier token.
//! 5. **Both sides switch to AES-128-CFB8** keyed with the shared secret.
//!    [`crate::crypt`].
//! 6. *The server asks Mojang's session server whether the player is who they
//!    say they are*, using the digest [`server_id_hash`] computes.
//!
//! # Step 6 is not done here, deliberately
//!
//! **The session-server call is Phase 1's job and this crate does not make
//! it.** No HTTP client is a dependency of `dust-net`, no Mojang endpoint
//! appears in it, and nothing in this module was tested against Mojang's
//! infrastructure — the live-server work in `tests/vanilla_status.rs` runs
//! against a local server in offline mode and nothing else.
//!
//! What is here is everything that can be built and checked without
//! credentials: the key pair, the encoding a real client parses, the
//! decryption, the token challenge, and the digest that the eventual HTTP call
//! will put in its query string. [`server_id_hash`] is included precisely
//! because it is the part of step 6 that is *protocol* rather than *network* —
//! it has published test vectors, so it can be got right now instead of got
//! wrong later against a server that only says "no".
//!
//! Until step 6 exists, a Dust server must run in offline mode, where any
//! client may claim any name. That is a statement about this crate's
//! completeness, not a defensible configuration.
//!
//! # Why RSA-1024, which is not a size anyone should choose in 2026
//!
//! Because the client's is. The key size is fixed by what vanilla generates
//! and what vanilla clients expect, and a server that used 2048 bits would
//! produce an Encryption Request that parses fine and a handshake that costs
//! more, for a secret whose lifetime is one session. Changing it is not a
//! decision this crate can make alone.
//!
//! # What the guards here do not catch
//!
//! The verify token proves the client could decrypt *something the server just
//! sent*. It does not prove who the client is — only the session server does
//! that — and it does not protect the connection against an attacker in the
//! middle, who can substitute their own key in step 2, because nothing signs
//! the server's public key. Minecraft's answer to that is the session server
//! binding the server id hash, which again is step 6.
//!
//! The comparison in [`ServerKey::verify_token`] is constant-time, which
//! matters less than it looks: the token is public — the server sent it in the
//! clear — so there is nothing to leak by comparing it quickly. It is written
//! that way because the next person to reach for this helper may be comparing
//! something that is not public.

use rand::rngs::SysRng;
use rsa::pkcs8::{DecodePrivateKey as _, EncodePublicKey as _};
use rsa::rand_core::{TryRng as _, UnwrapErr};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest as _, Sha1};

use crate::crypt::{BadSecretLength, SharedSecret};

/// The key size vanilla uses. See the module docs for why it is not a choice.
pub const KEY_BITS: usize = 1024;

/// The verify token length vanilla uses.
pub const VERIFY_TOKEN_LEN: usize = 4;

/// The server's RSA key pair for one run.
///
/// Vanilla generates a fresh pair at startup and never persists it, and Dust
/// does the same. Persisting it would buy nothing — the key authenticates
/// nothing, since no client checks it against anything — and would turn a
/// value that lives in memory for one uptime into a file somebody has to
/// protect forever.
pub struct ServerKey {
    private: RsaPrivateKey,
    /// Cached because it goes into every Encryption Request and re-encoding it
    /// per login is arithmetic nobody asked for.
    public_der: Vec<u8>,
}

// The private key must not appear in a log line.
impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey")
            .field("bits", &KEY_BITS)
            .field("public_der_len", &self.public_der.len())
            .finish_non_exhaustive()
    }
}

impl ServerKey {
    /// Generate a fresh key pair from the operating system's CSPRNG.
    ///
    /// This takes tens of milliseconds and occasionally much longer — prime
    /// search has no upper bound on its running time — so it belongs at
    /// startup, once, and not on the login path.
    pub fn generate() -> Result<Self, KeyError> {
        // `UnwrapErr` turns the fallible OS generator into the infallible
        // `CryptoRng` the RSA key generator asks for. A failure of the OS
        // CSPRNG panics rather than returning, which is the right answer: a
        // server that cannot get randomness cannot generate a key, and
        // carrying on with a degraded one is worse than stopping.
        let mut rng = UnwrapErr(SysRng);
        let private = RsaPrivateKey::new(&mut rng, KEY_BITS)
            .map_err(|error| KeyError::Generate(error.to_string()))?;
        Self::from_private(private)
    }

    /// Load a key pair from PKCS#8 DER.
    ///
    /// For tests, which need a *fixed* key so that a ciphertext produced by
    /// another implementation can be decrypted by this one. Not for
    /// production: see [`generate`](Self::generate) on why the key is not
    /// persisted.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, KeyError> {
        let private = RsaPrivateKey::from_pkcs8_der(der)
            .map_err(|error| KeyError::Decode(error.to_string()))?;
        Self::from_private(private)
    }

    fn from_private(private: RsaPrivateKey) -> Result<Self, KeyError> {
        let public_der = RsaPublicKey::from(&private)
            .to_public_key_der()
            .map_err(|error| KeyError::Encode(error.to_string()))?
            .as_bytes()
            .to_vec();
        Ok(Self {
            private,
            public_der,
        })
    }

    /// The public key as X.509 SubjectPublicKeyInfo DER.
    ///
    /// This is what goes in Encryption Request, and the encoding is not
    /// negotiable: a vanilla client hands these bytes straight to
    /// `X509EncodedKeySpec`, which parses SPKI and nothing else. In
    /// particular it is **not** the bare PKCS#1 `RSAPublicKey` structure —
    /// that is the inner `BIT STRING` of this one, it is shorter, it is what
    /// several RSA libraries call "the public key DER", and a client handed
    /// it throws `InvalidKeySpecException` before the login gets anywhere.
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_der
    }

    /// Decrypt the shared secret from an Encryption Response.
    ///
    /// The length check is not a formality. PKCS#1 v1.5 decryption of an
    /// attacker-chosen ciphertext can succeed and yield a plaintext of any
    /// length up to the modulus size, so "it decrypted" and "it is an AES-128
    /// key" are separate facts.
    pub fn decrypt_shared_secret(&self, ciphertext: &[u8]) -> Result<SharedSecret, KeyError> {
        let plain = self.decrypt(ciphertext)?;
        SharedSecret::from_slice(&plain).map_err(KeyError::SecretLength)
    }

    /// Decrypt the echoed verify token and compare it with what was sent.
    ///
    /// Returns `Ok(())` only when the token decrypts and matches. A mismatch
    /// and a failed decryption are different variants, because they mean
    /// different things: the first is a replay or a different server's
    /// response, the second is a client using the wrong key.
    pub fn verify_token(&self, ciphertext: &[u8], expected: &VerifyToken) -> Result<(), KeyError> {
        let plain = self.decrypt(ciphertext)?;
        if constant_time_eq(&plain, expected.as_bytes()) {
            Ok(())
        } else {
            Err(KeyError::TokenMismatch)
        }
    }

    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, KeyError> {
        self.private
            .decrypt(Pkcs1v15Encrypt, ciphertext)
            .map_err(|error| KeyError::Decrypt(error.to_string()))
    }
}

/// The random challenge a server puts in Encryption Request.
///
/// Fresh per login attempt, not per server. A token reused across logins is
/// not a challenge, because the answer to it is the same every time and can be
/// replayed by anyone who saw one.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifyToken([u8; VERIFY_TOKEN_LEN]);

impl VerifyToken {
    pub fn generate() -> Result<Self, KeyError> {
        let mut bytes = [0u8; VERIFY_TOKEN_LEN];
        SysRng
            .try_fill_bytes(&mut bytes)
            .map_err(|error| KeyError::Random(error.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; VERIFY_TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for VerifyToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Public once sent, but not before, and a log line does not know which
        // side of that it is on.
        f.write_str("VerifyToken(<redacted>)")
    }
}

/// Compare two byte strings without an early exit.
///
/// Length is allowed to leak; content is not. That is the standard shape, and
/// it is right here because the lengths of both operands are fixed by the
/// protocol and known to anybody reading it.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Minecraft's login digest, as the session-server call will need it.
///
/// SHA-1 over the server id, then the shared secret, then the public key DER,
/// rendered in an encoding that exists for one reason: the original
/// implementation called `new BigInteger(digest).toString(16)`, and Java's
/// `BigInteger(byte[])` reads its input as **two's complement**. So a digest
/// whose first bit is set is a negative number, and it renders with a leading
/// minus sign and the magnitude of its negation. It is not hexadecimal SHA-1;
/// it is hexadecimal signed-SHA-1, and about half of all digests differ from
/// the obvious rendering.
///
/// Nothing computes this format on purpose. It is reproduced because Mojang's
/// session server compares against it, and the three published vectors in the
/// tests are the check that this is the quirk and not a different one.
pub fn server_id_hash(server_id: &str, secret: &SharedSecret, public_key_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(secret.as_bytes());
    hasher.update(public_key_der);
    twos_complement_hex(&hasher.finalize())
}

/// Render a big-endian byte string as Java's `BigInteger::toString(16)` would.
fn twos_complement_hex(digest: &[u8]) -> String {
    let negative = digest.first().is_some_and(|byte| byte & 0x80 != 0);
    let magnitude = if negative {
        // Negate in place: two's complement is invert-and-add-one, done
        // big-endian from the least significant byte.
        let mut bytes = digest.to_vec();
        for byte in &mut bytes {
            *byte = !*byte;
        }
        let mut carry = 1u16;
        for byte in bytes.iter_mut().rev() {
            let sum = u16::from(*byte) + carry;
            *byte = sum as u8;
            carry = sum >> 8;
            if carry == 0 {
                break;
            }
        }
        bytes
    } else {
        digest.to_vec()
    };

    let mut hex = String::with_capacity(magnitude.len() * 2 + 1);
    for byte in &magnitude {
        hex.push_str(&format!("{byte:02x}"));
    }
    // `toString` emits no leading zeros, and "0" for zero.
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    if negative {
        format!("-{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Why a key operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Key generation failed. The message is the RSA library's.
    Generate(String),
    /// A key could not be read from DER.
    Decode(String),
    /// A public key could not be encoded as DER.
    Encode(String),
    /// The operating system's CSPRNG failed.
    Random(String),
    /// The ciphertext did not decrypt under this key.
    Decrypt(String),
    /// It decrypted, and was not sixteen bytes.
    SecretLength(BadSecretLength),
    /// The echoed verify token was not the one that was sent.
    TokenMismatch,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generate(m) => write!(f, "could not generate an RSA-{KEY_BITS} key pair: {m}"),
            Self::Decode(m) => write!(f, "could not read an RSA key from DER: {m}"),
            Self::Encode(m) => write!(f, "could not encode the public key as DER: {m}"),
            Self::Random(m) => write!(f, "the operating system's CSPRNG failed: {m}"),
            Self::Decrypt(m) => write!(
                f,
                "the client's encrypted blob did not decrypt under this server's key: {m}"
            ),
            Self::SecretLength(e) => write!(f, "the client's shared secret is unusable: {e}"),
            Self::TokenMismatch => write!(
                f,
                "the client echoed a verify token other than the one it was sent; this login \
                 is a replay or was addressed to a different server"
            ),
        }
    }
}

impl std::error::Error for KeyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypt::SHARED_SECRET_LEN;
    use crate::testkeys;

    fn fixture_key() -> ServerKey {
        ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER).expect("the fixture key loads")
    }

    #[test]
    fn the_public_key_der_is_the_encoding_openssl_produces() {
        // **The outside check on the encoding.** A generated key encoded and
        // re-parsed by this crate agrees with itself under any DER layout,
        // including the PKCS#1 one a Java client rejects. These bytes came out
        // of `openssl rsa -pubout -outform DER`, so this compares Dust with
        // LibreSSL.
        assert_eq!(
            fixture_key().public_key_der(),
            testkeys::PUBLIC_KEY_SPKI_DER
        );
    }

    #[test]
    fn the_encoding_is_spki_and_not_bare_pkcs1() {
        // The specific mistake the test above would catch and would not
        // explain. SPKI wraps an AlgorithmIdentifier — the rsaEncryption OID
        // 1.2.840.113549.1.1.1 — around a BIT STRING holding the PKCS#1
        // structure. The OID's DER is `06 09 2a 86 48 86 f7 0d 01 01 01`; a
        // bare PKCS#1 encoding does not contain it anywhere.
        const RSA_ENCRYPTION_OID: &[u8] = &[
            0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
        ];
        let der = fixture_key().public_key_der().to_vec();
        assert!(
            der.windows(RSA_ENCRYPTION_OID.len())
                .any(|window| window == RSA_ENCRYPTION_OID),
            "the public key DER carries no rsaEncryption OID, so it is not SubjectPublicKeyInfo \
             and X509EncodedKeySpec will refuse it"
        );
        assert_eq!(der[0], 0x30, "SPKI is a SEQUENCE");
    }

    #[test]
    fn a_ciphertext_from_openssl_decrypts_to_a_shared_secret() {
        // **The outside check on decryption.** The ciphertext was produced by
        // LibreSSL's PKCS#1 v1.5, not by this crate, so a padding scheme that
        // is self-consistently wrong fails here.
        let secret = fixture_key()
            .decrypt_shared_secret(testkeys::ENCRYPTED_SECRET)
            .expect("the fixture ciphertext decrypts");
        assert_eq!(secret.as_bytes(), testkeys::SECRET_PLAINTEXT);
    }

    #[test]
    fn a_verify_token_from_openssl_is_accepted_and_a_different_one_is_not() {
        let key = fixture_key();
        let expected =
            VerifyToken::from_bytes(testkeys::TOKEN_PLAINTEXT.try_into().expect("four bytes"));
        assert_eq!(
            key.verify_token(testkeys::ENCRYPTED_TOKEN, &expected),
            Ok(())
        );

        // The same ciphertext against a different challenge is a replay, and
        // must be refused. Without this assertion the test above passes with a
        // `verify_token` that ignores its argument.
        let other = VerifyToken::from_bytes(*b"NOPE");
        assert_eq!(
            key.verify_token(testkeys::ENCRYPTED_TOKEN, &other),
            Err(KeyError::TokenMismatch)
        );
    }

    #[test]
    fn a_ciphertext_from_a_real_jvm_decrypts_to_a_shared_secret() {
        // The interop check that matters most and is easiest to get wrong: a
        // vanilla client does not compare DER blobs, it feeds them to
        // `X509EncodedKeySpec` and encrypts with the result. This ciphertext
        // is what came back when a real JVM did exactly that against this
        // crate's encoding. If `public_key_der` emitted PKCS#1 instead of
        // SPKI, the JVM would have thrown before producing anything to paste
        // here — so the existence of this constant is half the test, and
        // decrypting it is the other half.
        let secret = fixture_key()
            .decrypt_shared_secret(testkeys::ENCRYPTED_SECRET_FROM_JVM)
            .expect("the JVM's ciphertext decrypts");
        assert_eq!(secret.as_bytes(), testkeys::SECRET_PLAINTEXT);
    }

    #[test]
    fn a_blob_encrypted_under_another_key_is_refused() {
        let stranger = ServerKey::generate().expect("keygen");
        assert!(matches!(
            stranger.decrypt_shared_secret(testkeys::ENCRYPTED_SECRET),
            Err(KeyError::Decrypt(_))
        ));
    }

    #[test]
    fn rubbish_is_refused_rather_than_decrypted() {
        let key = fixture_key();
        for length in [0usize, 1, 16, 127, 128, 129, 4096] {
            let blob = vec![0x41u8; length];
            assert!(
                key.decrypt_shared_secret(&blob).is_err(),
                "{length} bytes of rubbish decrypted"
            );
        }
    }

    #[test]
    fn a_decryption_that_is_not_sixteen_bytes_is_refused() {
        // The token ciphertext decrypts perfectly and yields four bytes, which
        // is not an AES-128 key. "It decrypted" and "it is a key" are separate
        // facts, and this is the test that says the second one is checked.
        assert_eq!(
            fixture_key().decrypt_shared_secret(testkeys::ENCRYPTED_TOKEN),
            Err(KeyError::SecretLength(BadSecretLength { got: 4 }))
        );
    }

    #[test]
    fn a_generated_key_is_the_right_size_and_round_trips() {
        let key = ServerKey::generate().expect("keygen");
        // 1024-bit SPKI is 162 bytes; the assertion is on the modulus rather
        // than the DER length, which is what actually has to be 1024 bits.
        assert_eq!(key.public_key_der().len(), 162);
        assert_eq!(key.public_key_der(), key.public_key_der());
    }

    #[test]
    fn two_generated_keys_differ() {
        // A keygen that returned a constant would pass every other test in
        // this file.
        let a = ServerKey::generate().expect("keygen");
        let b = ServerKey::generate().expect("keygen");
        assert_ne!(a.public_key_der(), b.public_key_der());
    }

    #[test]
    fn two_verify_tokens_differ() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            seen.insert(VerifyToken::generate().expect("rng").as_bytes().to_vec());
        }
        assert!(seen.len() > 32, "the verify token is not random enough");
    }

    /// The published digest vectors for Minecraft's login hash.
    ///
    /// **The outside check on the two's-complement quirk.** Each row is the
    /// digest of the literal ASCII string in its first column, which is how
    /// the vectors are published — the real input is a concatenation of three
    /// things, and using it here would mean the expected values came from this
    /// code. `jeb_` is the row that matters: its digest has the high bit set,
    /// so it renders negative, and an implementation that simply hex-encodes
    /// SHA-1 gets the other two right and this one wrong.
    ///
    /// Confirmed independently before being written down, with
    /// `hashlib.sha1(s).digest()` read back as a signed big-endian integer.
    const DIGEST_VECTORS: &[(&str, &str)] = &[
        ("Notch", "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"),
        ("jeb_", "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"),
        ("simon", "88e16a1019277b15d58faf0541e11910eb756f6"),
    ];

    #[test]
    fn the_login_digest_matches_the_published_vectors() {
        for &(input, expected) in DIGEST_VECTORS {
            let digest = Sha1::digest(input.as_bytes());
            assert_eq!(twos_complement_hex(&digest), expected, "{input}");
        }
    }

    #[test]
    fn the_negative_rendering_is_not_an_accident() {
        // `simon` is the row with a leading zero byte, which `toString(16)`
        // drops; `jeb_` is the negative one. Both are in the vectors above,
        // and this pins the two properties separately so a failure says which.
        assert!(DIGEST_VECTORS[1].1.starts_with('-'));
        assert_eq!(
            DIGEST_VECTORS[2].1.len(),
            39,
            "a leading zero must be dropped"
        );
        assert_eq!(twos_complement_hex(&[0x00; 20]), "0");
        assert_eq!(twos_complement_hex(&[0xff; 20]), "-1");
        assert_eq!(twos_complement_hex(&[0x80, 0x00]), "-8000");
    }

    #[test]
    fn the_digest_covers_all_three_inputs() {
        // The concatenation order is server id, secret, key. Changing any one
        // input must change the digest, or two logins the session server
        // should tell apart would hash the same.
        let secret = SharedSecret::from_bytes([0x11; SHARED_SECRET_LEN]);
        let other_secret = SharedSecret::from_bytes([0x12; SHARED_SECRET_LEN]);
        let key = fixture_key();
        let base = server_id_hash("", &secret, key.public_key_der());
        assert_ne!(base, server_id_hash("x", &secret, key.public_key_der()));
        assert_ne!(
            base,
            server_id_hash("", &other_secret, key.public_key_der())
        );
        assert_ne!(base, server_id_hash("", &secret, &[]));
    }

    #[test]
    fn keys_and_tokens_do_not_print_themselves() {
        let rendered = format!("{:?}", fixture_key());
        assert!(rendered.contains("ServerKey"), "{rendered}");
        assert!(!rendered.contains("30819f"), "{rendered}");
        assert_eq!(
            format!("{:?}", VerifyToken::from_bytes(*b"TOKN")),
            "VerifyToken(<redacted>)"
        );
    }

    #[test]
    fn constant_time_eq_is_still_an_eq() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
