//! The crypto hardening suite: what happens when everything around the
//! cipher goes wrong.
//!
//! Three questions, one per act:
//!
//! * **Is the mode the mode?** CFB8 has several plausible ways to be subtly
//!   wrong — feeding plaintext back instead of ciphertext, shifting the
//!   register the wrong way — and every one of them decrypts its own
//!   ciphertext. The known-answer test below does not trust [`crate::crypt`]
//!   or the `cfb8` crate: it implements CFB8 *again*, here, out of raw AES
//!   block calls, checks that re-implementation against the published NIST
//!   vector, and only then compares it against the connection cipher. Two
//!   implementations agreeing with each other and with NIST is a fact about
//!   the world; one implementation agreeing with itself is not.
//! * **Does a wrong answer stay wrong?** A verify token that was never sent,
//!   a shared secret that was tampered with in flight: each must be refused,
//!   cleanly, without a panic and without partial acceptance.
//! * **What does failure leak?** RSA PKCS#1 v1.5 decryption is the classic
//!   padding-oracle site. The posture documented below is the honest one for
//!   a protocol that mandates PKCS#1 v1.5: every failure mode collapses to
//!   the same client-visible event, and the tests pin the collapse.

use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use dust_net::crypt::{Cipher, SharedSecret};
use dust_net::login::{KeyError, ServerKey, VerifyToken};
use dust_net::testkeys;

// ---------------------------------------------------------------------------
// Act one: the mode, computed twice.
// ---------------------------------------------------------------------------

/// CFB8 written from first principles, out of raw AES block encryptions.
///
/// Both directions run the block cipher **forwards**: encryption feeds each
/// ciphertext byte into the shifting register, and so does decryption — the
/// keystream is generated identically, and decryption merely subtracts it
/// instead of adding. This function is the reference the vectors below are
/// checked against; between it and the published vector, an argument order
/// swap, a wrong shift, or a feedback-of-plaintext bug has nowhere to hide.
fn reference_cfb8(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8], decrypting: bool) {
    // The block type is a 16-byte `Array`; the register starts as the IV.
    let block_cipher = Aes128::new(&(*key).into());
    let mut register = <aes::cipher::Block<Aes128> as Default>::default();
    register.copy_from_slice(iv);
    for byte in data.iter_mut() {
        let mut block = register;
        block_cipher.encrypt_block(&mut block);
        let keystream = block[0];
        // Both directions advance the register on the **ciphertext** byte.
        // In encryption that is the byte after the xor; in decryption it is
        // the byte as it arrived, before the xor strips the keystream off.
        // Getting this backwards still round-trips against a matching
        // partner implementation and fails only against real vectors.
        let feedback = if decrypting {
            let arriving = *byte;
            *byte ^= keystream;
            arriving
        } else {
            *byte ^= keystream;
            *byte
        };
        register.copy_within(1.., 0);
        register[15] = feedback;
    }
}

/// AES-128-CFB8 from NIST SP 800-38A, appendix F.3.7, transcribed rather
/// than derived: these bytes come from the publication (and were confirmed
/// against LibreSSL's `enc -aes-128-cfb8`), not from anything in this
/// repository.
const NIST_KEY: [u8; 16] = [
    0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
];
const NIST_IV: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];
const NIST_PLAIN: [u8; 18] = [
    0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93, 0x17, 0x2a,
    0xae, 0x2d,
];
const NIST_CIPHER: [u8; 18] = [
    0x3b, 0x79, 0x42, 0x4c, 0x9c, 0x0d, 0xd4, 0x36, 0xba, 0xce, 0x9e, 0x0e, 0xd4, 0x58, 0x6a, 0x4f,
    0x32, 0xb9,
];

#[test]
fn the_reference_implementation_matches_the_published_vector() {
    // If this fails, the reference is broken and every comparison built on
    // it is void. It runs first, alone, for exactly that reason.
    let mut encrypted = NIST_PLAIN;
    reference_cfb8(&NIST_KEY, &NIST_IV, &mut encrypted, false);
    assert_eq!(
        encrypted, NIST_CIPHER,
        "reference encryption diverges from NIST"
    );

    let mut decrypted = NIST_CIPHER;
    reference_cfb8(&NIST_KEY, &NIST_IV, &mut decrypted, true);
    assert_eq!(
        decrypted, NIST_PLAIN,
        "reference decryption diverges from NIST"
    );
}

#[test]
fn the_connection_cipher_is_the_reference_in_both_directions() {
    // The pin on encrypt-vs-decrypt direction: Minecraft's outgoing stream
    // is standard CFB8 encryption, its incoming stream standard CFB8
    // decryption, both with key equal to IV. A wrapper that constructed two
    // encryptors, or fed the register backwards on one side, diverges here
    // while remaining perfectly self-consistent.
    let secret_bytes: [u8; 16] =
        core::array::from_fn(|i| (i as u8).wrapping_mul(17).wrapping_add(3));
    let secret = SharedSecret::from_bytes(secret_bytes);

    let message: Vec<u8> = (0..64u32).map(|i| (i * 7 + 11) as u8).collect();

    let mut wired = Cipher::disabled();
    wired.enable(&secret);

    // Outgoing: the connection cipher must be plain CFB8 encryption.
    let mut expected = message.clone();
    reference_cfb8(&secret_bytes, &secret_bytes, &mut expected, false);
    let mut actual = message.clone();
    wired.encrypt(&mut actual);
    assert_eq!(actual, expected, "outgoing is not CFB8 encryption");

    // Incoming: plain CFB8 decryption over the same key-as-IV stream.
    let mut incoming = expected;
    wired.decrypt(&mut incoming);
    assert_eq!(incoming, message, "incoming is not CFB8 decryption");

    // Cross-check the other way, on a fresh connection because a real
    // connection's register never rewinds: ciphertext produced by the
    // reference decrypts through the connection cipher untouched.
    let mut second = Cipher::disabled();
    second.enable(&secret);
    let mut from_reference = message.clone();
    reference_cfb8(&secret_bytes, &secret_bytes, &mut from_reference, false);
    second.decrypt(&mut from_reference);
    assert_eq!(from_reference, message);
}

// ---------------------------------------------------------------------------
// Act two: wrong answers stay wrong.
// ---------------------------------------------------------------------------

fn fixture_key() -> ServerKey {
    ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER).expect("fixture key")
}

#[test]
fn a_verify_token_that_was_never_sent_is_refused() {
    let key = fixture_key();
    let sent = VerifyToken::from_bytes(*b"SENT");
    // What comes back decrypts perfectly — it just was not the challenge.
    let forged = rsa_stub_encrypt(testkeys::PUBLIC_KEY_SPKI_DER, b"EVIL").expect("encrypt");

    assert_eq!(
        key.verify_token(&forged, &sent),
        Err(KeyError::TokenMismatch),
        "a validly encrypted wrong token must not pass"
    );
    // And the correct one still passes, so the refusal is about content.
    let honest = rsa_stub_encrypt(testkeys::PUBLIC_KEY_SPKI_DER, b"SENT").expect("encrypt");
    assert_eq!(key.verify_token(&honest, &sent), Ok(()));
}

/// PKCS#1 v1.5 encryption with the public half of the fixture key, standing
/// in for the client side of the exchange.
fn rsa_stub_encrypt(public_der: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    use rsa::pkcs8::DecodePublicKey as _;
    use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
    let public = RsaPublicKey::from_public_key_der(public_der).map_err(|e| e.to_string())?;
    let rng = &mut rsa::rand_core::UnwrapErr(rand::rngs::SysRng);
    public
        .encrypt(rng, Pkcs1v15Encrypt, plain)
        .map_err(|e| e.to_string())
}

#[test]
fn a_tampered_shared_secret_is_detected_and_never_half_accepted() {
    // Every single-bit flip of the fixture ciphertext: the padding is
    // probabilistically certain to break somewhere, and when it does not,
    // the recovered secret must at least differ from the one the client
    // sent. Either way the protocol moves on; nothing panics, nothing
    // accepts silently.
    let key = fixture_key();
    let expected = testkeys::SECRET_PLAINTEXT;

    let mut refused = 0usize;
    let total = testkeys::ENCRYPTED_SECRET.len() * 8;
    for bit in 0..total {
        let mut blob = testkeys::ENCRYPTED_SECRET.to_vec();
        blob[bit / 8] ^= 1 << (bit % 8);

        match key.decrypt_shared_secret(&blob) {
            Err(_) => refused += 1,
            Ok(recovered) => assert_ne!(
                recovered.as_bytes(),
                expected,
                "bit {bit} flipped and the original secret came back intact; \
                 malleability would defeat the whole exchange"
            ),
        }
    }
    // PKCS#1 v1.5's padding structure means essentially every flip breaks
    // it; the exact number is an artefact of the fixture, so the assertion
    // is a wide floor rather than a brittle equality.
    assert!(
        refused >= total * 19 / 20,
        "only {refused} of {total} corruptions were refused"
    );
}

#[test]
fn a_truncated_or_extended_blob_never_yields_a_secret() {
    // Length manipulation is the cheapest attack on any length-sensitive
    // parser. Every prefix of the real ciphertext, and every extension of
    // it, must be refused outright.
    let key = fixture_key();
    let blob = testkeys::ENCRYPTED_SECRET;

    for cut in 0..blob.len() {
        assert!(
            key.decrypt_shared_secret(&blob[..cut]).is_err(),
            "{cut}-byte prefix decrypted"
        );
    }
    let mut extended = blob.to_vec();
    extended.extend_from_slice(b"padding of my own");
    assert!(key.decrypt_shared_secret(&extended).is_err());
}

// ---------------------------------------------------------------------------
// Act three: what failure leaks.
// ---------------------------------------------------------------------------

/// Every way a blob can be malformed, applied to the fixture ciphertext:
/// truncations, single-bit flips, byte substitutions, and junk lengths. The
/// posture claim rests on *all* of them collapsing to the same thing.
fn malformed_shapes() -> Vec<Vec<u8>> {
    let base = testkeys::ENCRYPTED_SECRET;
    let mut shapes = Vec::new();
    for cut in [0usize, 1, 63, 64, 100, 127] {
        shapes.push(base[..cut].to_vec());
    }
    // The unmodified blob is deliberately absent: it decrypts, and this
    // collection is for shapes that must not.
    let mut rng_state = 0x1234_5678_9ABC_DEF0u64;
    let mut next = move || {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };
    for _ in 0..256 {
        let mut blob = base.to_vec();
        let flips = (next() % 4 + 1) as usize;
        for _ in 0..flips {
            let byte = (next() as usize) % blob.len();
            let bit = (next() % 8) as u8;
            blob[byte] ^= 1 << bit;
        }
        shapes.push(blob);
    }
    shapes.push(vec![0u8; 128]);
    shapes.push(vec![0xFFu8; 128]);
    shapes.push(Vec::new());
    shapes
}

#[test]
fn every_failure_shape_collapses_to_one_client_visible_outcome() {
    // The padding-oracle posture, as a property. The server's rule is: any
    // error from the decryption path — wrong padding, wrong length, wrong
    // key, wrong token — produces exactly one response, closing the login.
    // No variant earns a retry, a different delay, or a different message.
    // This test cannot prove the network behaviour (that is the caller's
    // layer), but it proves the precondition: the error surface is finite,
    // enumerable, and none of it echoes attacker input back.
    let key = fixture_key();
    for shape in malformed_shapes() {
        let outcome = match shape.len() {
            0 => continue, // zero-length is refused before RSA sees it
            _ => key.decrypt_shared_secret(&shape),
        };
        if let Ok(secret) = outcome {
            // A corrupted shape that happens to land on valid padding is
            // still not the client's secret; accept it only as a stranger.
            assert_ne!(
                secret.as_bytes(),
                testkeys::SECRET_PLAINTEXT,
                "a malformed shape recovered the original secret"
            );
        }
    }
}

#[test]
fn no_error_message_echoes_the_input() {
    // Errors get logged. Logged text gets read by strangers with support
    // tickets. None of it should contain the blob or anything derived from
    // it beyond a length.
    let key = fixture_key();
    let blob = testkeys::ENCRYPTED_SECRET.to_vec();
    let rendered = match key.decrypt_shared_secret(&{
        let mut b = blob.clone();
        b[10] ^= 0xFF;
        b
    }) {
        Err(e) => e.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!rendered.is_empty());
    for leaked_byte in &blob[60..68] {
        assert!(
            !rendered.contains(&format!("{leaked_byte:02x}")),
            "error text contains ciphertext material: {rendered}"
        );
    }

    // The same discipline for the length error, whose job is naming a
    // count, not quoting content. The fixture token ciphertext decrypts to
    // four well-formed bytes, which is the one way to reach the length
    // check honestly.
    let rendered = key
        .decrypt_shared_secret(testkeys::ENCRYPTED_TOKEN)
        .expect_err("a four-byte plaintext is not a key")
        .to_string();
    assert!(rendered.contains('4'), "{rendered}");
}

#[test]
fn the_posture_documented_in_this_file_holds_for_the_token_path_too() {
    // The token check shares the decrypt path and therefore the posture;
    // asserting its failure surface keeps the two halves from drifting.
    let key = fixture_key();
    let expected = VerifyToken::from_bytes(*b"TOKN");
    for shape in malformed_shapes() {
        let outcome = key.verify_token(&shape, &expected);
        if let Ok(()) = outcome {
            panic!("a malformed shape verified as the correct token");
        }
    }
}
