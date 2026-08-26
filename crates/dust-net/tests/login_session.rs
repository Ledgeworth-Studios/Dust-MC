//! The whole pre-play conversation, played by both ends, over in-memory
//! transports.
//!
//! Every other test here checks one layer against another or against fixed
//! vectors. This one walks the real sequence a real client and server walk —
//! handshake, status ping, then login with the encryption switch — using the
//! crate's own driver on both ends of a duplex, with packet bodies assembled
//! byte by byte. The bodies are deliberately hand-rolled: `dust-net` must not
//! know what any id means, so nothing in it can be trusted to build them, and
//! a test that builds them by hand is also the only kind that would notice
//! the day an id moved.
//!
//! What this proves that the unit tests cannot:
//!
//! * The key exchange *end to end*: a secret invented on the client side
//!   survives RSA encryption, the wire, decryption on the server side, and
//!   comes out identical — and both sides then hold a working AES-128-CFB8
//!   stream keyed with it.
//! * The verify-token round trip across the same exchange: the challenge the
//!   server sent inside Encryption Request is the one that comes back.
//! * The mid-stream mode switches in the order the protocol demands: the
//!   response goes out plaintext, everything after either end's switch is
//!   ciphertext, and neither end trips over the boundary.
//! * The authenticated-state transition: Login Acknowledged moves the server
//!   connection from Login to Configuration — the checked state machine,
//!   driven through the driver, counting its configuration entries.
//!
//! The client's shared secret is fixed rather than random. Nothing about the
//! exchange depends on the secret being unpredictable to a test — the
//! unpredictability matters against eavesdroppers, and there are none closer
//! than these two duplex halves.

use std::time::Duration;

use dust_net::crypt::{SharedSecret, SHARED_SECRET_LEN};
use dust_net::frame::Frame;
use dust_net::io::{Conn, ConnConfig, Timeouts};
use dust_net::login::{ServerKey, VerifyToken};
use dust_net::state::State;
use dust_net::testkeys;
use dust_net::varint::{read_var_int, write_var_int};
use rsa::pkcs8::DecodePublicKey as _;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

/// A deterministic generator for test-only bytes. The one randomness this
/// file genuinely needs — the RSA padding — comes from the operating system
/// via the `rsa` crate; everything else may be reproducible, and reproducible
/// failures are kinder than random ones.
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
}

// ---------------------------------------------------------------------------
// Wire helpers. Strings are u16 big-endian length plus UTF-8; byte arrays are
// VarInt length plus raw bytes; integers are as the protocol writes them.
// ---------------------------------------------------------------------------

fn put_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn get_string(input: &[u8]) -> (&str, usize) {
    let len = u16::from_be_bytes([input[0], input[1]]) as usize;
    (
        std::str::from_utf8(&input[2..2 + len]).expect("utf8"),
        2 + len,
    )
}

fn put_byte_array(out: &mut Vec<u8>, value: &[u8]) {
    write_var_int(value.len() as i32, out);
    out.extend_from_slice(value);
}

fn get_byte_array(input: &[u8]) -> (&[u8], usize) {
    let (len, used) = read_var_int(input).expect("array length");
    let len = len as usize;
    (&input[used..used + len], used + len)
}

fn var_int(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    write_var_int(value, &mut out);
    out
}

fn config() -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle: Some(Duration::from_secs(10)),
            pre_auth_budget: Some(Duration::from_secs(10)),
        },
        ..ConnConfig::default()
    }
}

// ---------------------------------------------------------------------------
// The status ping: proof the unauthenticated path works before anything is
// asked to keep a secret.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_server_list_ping_runs_to_completion_and_ends_cleanly() {
    let flow = tokio::time::timeout(Duration::from_secs(30), async {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let mut server = Conn::new(server_io, config());
        let mut client = Conn::new(client_io, config());

        // Handshake, next state 1: status.
        let mut body = var_int(767);
        put_string(&mut body, "localhost");
        body.extend_from_slice(&25565u16.to_be_bytes());
        body.extend_from_slice(&var_int(1));
        client.handshake(1).expect("status intent");
        client.send(Frame::new(0x00, body)).await.expect("send");

        let handshake = server.next_frame().await.expect("read").expect("frame");
        assert_eq!(handshake.id, 0x00);
        let (_, used) = read_var_int(&handshake.body).expect("protocol");
        let (host, more) = get_string(&handshake.body[used..]);
        assert_eq!(host, "localhost");
        let port =
            u16::from_be_bytes([handshake.body[used + more], handshake.body[used + more + 1]]);
        assert_eq!(port, 25565);
        let (next_state, _) = read_var_int(&handshake.body[used + more + 2..]).expect("state");
        server.handshake(next_state).expect("apply handshake");
        assert_eq!(server.state(), State::Status);
        assert_eq!(server.intent(), Some(dust_net::state::Intent::Status));

        // Status Request → Response.
        client
            .send(Frame::new(0x00, Vec::new()))
            .await
            .expect("send");
        let _ = server.next_frame().await.expect("read").expect("request");
        let mut response = Vec::new();
        put_string(
            &mut response,
            r#"{"version":{"name":"Dust","protocol":767}}"#,
        );
        server.send(Frame::new(0x00, response)).await.expect("send");

        let answered = client.next_frame().await.expect("read").expect("response");
        assert_eq!(answered.id, 0x00);
        let (json, _) = get_string(&answered.body);
        assert!(json.contains("\"Dust\""), "{json}");

        // Ping → Pong, then a clean end on both sides.
        let payload = 0x0102_0304_0506_0708u64.to_be_bytes();
        client.send(Frame::new(0x01, payload)).await.expect("send");
        let ping = server.next_frame().await.expect("read").expect("ping");
        assert_eq!(ping.body, payload);
        server
            .send(Frame::new(0x01, ping.body))
            .await
            .expect("send");

        let pong = client.next_frame().await.expect("read").expect("pong");
        assert_eq!(pong.body, payload);

        client.close().await.expect("client close");
        let ended = server.next_frame().await.expect("read");
        assert_eq!(ended, None, "the server sees the clean end");
        // The connection has ended by its own observation of the EOF; there
        // is nothing left for `close` to do, and it says so. Dropping is the
        // whole teardown.
        assert!(server.has_ended());
    })
    .await;
    assert!(flow.is_ok(), "the status ping stalled");
}

// ---------------------------------------------------------------------------
// Login with the encryption exchange.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_login_exchanges_keys_and_reaches_the_authenticated_state() {
    let outcome = tokio::time::timeout(Duration::from_secs(30), run_login()).await;
    let outcome = outcome.expect("the login stalled");
    let outcome = outcome.expect("the login failed");

    // The exchange math, end to end: what the client invented is what the
    // server recovered, byte for byte, after RSA and the wire stood between
    // them.
    assert_eq!(
        outcome.recovered_secret, outcome.client_secret,
        "the server decrypted a different secret than the client sent"
    );
    assert!(
        outcome.token_round_tripped,
        "the verify token did not return"
    );
    assert_eq!(outcome.server_state, State::Configuration);
    assert_eq!(outcome.server_configurations, 1);
    assert_eq!(
        outcome.round_trip_frame,
        Some(Frame::new(0x63, b"still encrypted"))
    );
}

struct LoginOutcome {
    client_secret: [u8; 16],
    recovered_secret: [u8; 16],
    token_round_tripped: bool,
    server_state: State,
    server_configurations: u32,
    round_trip_frame: Option<Frame>,
}

async fn run_login() -> Result<LoginOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server = Conn::new(server_io, config());
    let mut client = Conn::new(client_io, config());

    // -- Handshake, next state 2 ------------------------------------------
    let mut body = var_int(767);
    put_string(&mut body, "localhost");
    body.extend_from_slice(&25565u16.to_be_bytes());
    body.extend_from_slice(&var_int(2));
    client.handshake(2).expect("login intent");
    client.send(Frame::new(0x00, body)).await?;

    let handshake = server.next_frame().await?.expect("handshake frame");
    assert_eq!(handshake.id, 0x00);
    let (_, used) = read_var_int(&handshake.body)?;
    let (_, more) = get_string(&handshake.body[used..]);
    let port_end = used + more + 2;
    let (next_state, _) = read_var_int(&handshake.body[port_end..])?;
    server.handshake(next_state)?;
    assert_eq!(server.state(), State::Login);

    // -- Login Start --------------------------------------------------------
    let mut start_body = Vec::new();
    put_string(&mut start_body, "steve");
    client.send(Frame::new(0x00, start_body)).await?;
    let start = server.next_frame().await?.expect("login start");
    assert_eq!(start.id, 0x00);
    let (name, _) = get_string(&start.body);
    assert_eq!(name, "steve");

    // -- Encryption Request -------------------------------------------------
    // The fixture key keeps this deterministic; see `testkeys` for why a
    // fixed pair is the honest choice for tests.
    let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)?;
    let token = VerifyToken::generate()?;
    let mut request_body = Vec::new();
    put_string(&mut request_body, ""); // server id: empty, as vanilla sends
    put_byte_array(&mut request_body, server_key.public_key_der());
    put_byte_array(&mut request_body, token.as_bytes());
    server.send(Frame::new(0x01, request_body)).await?;

    // The client parses the request exactly as a vanilla client would: DER
    // into a public key, then PKCS#1 v1.5 over the secret and the challenge.
    let request = client.next_frame().await?.expect("encryption request");
    assert_eq!(request.id, 0x01);
    let (_, server_id_len) = get_string(&request.body);
    let (public_der, der_used) = get_byte_array(&request.body[server_id_len..]);
    let public = RsaPublicKey::from_public_key_der(public_der)?;
    let (wire_token, _) = get_byte_array(&request.body[server_id_len + der_used..]);
    assert_eq!(wire_token, token.as_bytes());

    let mut seeded = SplitMix64::new(0x5EED_5EED);
    let mut client_secret = [0u8; SHARED_SECRET_LEN];
    client_secret[..8].copy_from_slice(&seeded.next().to_le_bytes());
    client_secret[8..].copy_from_slice(&seeded.next().to_le_bytes());
    // The padding needs genuine randomness; the operating system supplies it,
    // adapted exactly as `login.rs` adapts it on the server side.
    let rng = &mut rsa::rand_core::UnwrapErr(rand::rngs::SysRng);
    let encrypted_secret = public.encrypt(rng, Pkcs1v15Encrypt, &client_secret)?;
    let encrypted_token = public.encrypt(rng, Pkcs1v15Encrypt, wire_token)?;

    let mut response_body = Vec::new();
    put_byte_array(&mut response_body, &encrypted_secret);
    put_byte_array(&mut response_body, &encrypted_token);
    // Queued plaintext: the response itself travels in the clear.
    client.send(Frame::new(0x02, response_body)).await?;
    // Everything from here is ciphertext, both ways.
    client
        .enable_encryption(&SharedSecret::from_bytes(client_secret))
        .await?;

    // -- The server decrypts the response -----------------------------------
    let response = server.next_frame().await?.expect("encryption response");
    assert_eq!(response.id, 0x02);
    let (secret_blob, blob_used) = get_byte_array(&response.body);
    let (token_blob, _) = get_byte_array(&response.body[blob_used..]);

    let recovered = server_key.decrypt_shared_secret(secret_blob)?;
    server_key.verify_token(token_blob, &token)?;
    assert_eq!(
        token.as_bytes(),
        wire_token,
        "the challenge returned unchanged"
    );

    // Both sides now key their streams with the same sixteen bytes.
    server.enable_encryption(&recovered).await?;

    // -- Login Success, still pre-authentication until acknowledged ----------
    let mut success_body = Vec::new();
    put_string(&mut success_body, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
    put_string(&mut success_body, "steve");
    server.send(Frame::new(0x04, success_body)).await?;

    let success = client.next_frame().await?.expect("login success");
    assert_eq!(success.id, 0x04);
    let (uuid, used) = get_string(&success.body);
    assert_eq!(uuid, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
    let (seen_name, _) = get_string(&success.body[used..]);
    assert_eq!(seen_name, "steve");

    // -- Login Acknowledged: the authenticated transition --------------------
    client.send(Frame::new(0x03, Vec::<u8>::new())).await?;
    client.transition(State::Configuration)?;
    assert_eq!(client.configuration_count(), 1);

    let acked = server.next_frame().await?.expect("acknowledged");
    assert_eq!(acked.id, 0x03);
    server.transition(State::Configuration)?;
    assert_eq!(server.configuration_count(), 1);

    // -- One more frame each way, to prove the streams stay synchronized -----
    server.send(Frame::new(0x63, b"still encrypted")).await?;
    let round_trip = client.next_frame().await?.expect("round trip");
    assert_eq!(round_trip, Frame::new(0x63, b"still encrypted"));

    client.send(Frame::new(0x64, b"and back")).await?;
    let back = server.next_frame().await?.expect("return trip");
    assert_eq!(back, Frame::new(0x64, b"and back"));

    client.close().await?;
    let ended = server.next_frame().await?;
    assert_eq!(ended, None, "clean close after authentication");

    let result = LoginOutcome {
        client_secret,
        recovered_secret: *recovered.as_bytes(),
        token_round_tripped: true,
        server_state: server.state(),
        server_configurations: server.configuration_count(),
        round_trip_frame: Some(round_trip),
    };
    Ok(result)
}
