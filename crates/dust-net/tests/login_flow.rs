//! The login conversation, played end to end over in-memory transports.
//!
//! [`dust_net::login_flow::LoginHandler`] drives the server side of a duplex
//! while this file plays the client exactly as a vanilla client would: same
//! packet ids, same VarInt-prefixed bodies, same order of mode switches. No
//! socket is opened — the driver is generic over byte streams precisely so
//! these conversations run in memory — and Mojang is replaced by a scripted
//! [`SessionServer`] whose answers, and whose recorded questions, are both
//! asserted on.
//!
//! The handler owns the whole conversation in one future, so it runs on its
//! own task against a moved-in connection; the client half interleaves from
//! this task. What each script proves:
//!
//! * The **happy paths** produce vanilla's exact byte stream per mode,
//!   including the switch order only queue semantics can guarantee:
//!   Encryption Request plaintext, everything after the response ciphertext,
//!   Set Compression announcing what it turns on.
//! * The **failure paths** each leave the same fingerprints: a Disconnect
//!   naming the reason, no Login Success ever written, and a structured
//!   error for the log line.
//! * The **identity rules** hold at the boundary: names are canonicalised
//!   before anything else happens, Mojang's spelling wins online, and an
//!   offline id follows `nameUUIDFromBytes` bit for bit.

use std::collections::VecDeque;
use std::sync::{LockResult, Mutex, MutexGuard};
use std::time::Duration;

use dust_net::crypt::{SharedSecret, SHARED_SECRET_LEN};
use dust_net::frame::{Compress, Frame};
use dust_net::io::{Conn, ConnConfig, Timeouts};
use dust_net::login::ServerKey;
use dust_net::login_flow::{offline_profile_id, LoginConfig, LoginError, LoginHandler};
use dust_net::session::{
    JoinRequest, Profile, ProfileId, ProfileProperty, SessionError, SessionServer,
};
use dust_net::state::State;
use dust_net::testkeys;
use dust_net::varint::{read_var_int, write_var_int};
use rsa::pkcs8::DecodePublicKey as _;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

// ---------------------------------------------------------------------------
// Scripted Mojang.
// ---------------------------------------------------------------------------

/// Answers `hasJoined` from a script and records every question asked.
#[derive(Debug)]
struct ScriptedMojang {
    answers: Mutex<VecDeque<Result<Option<Profile>, SessionError>>>,
    /// `(username, server_id_hash)` pairs as they arrived.
    questions: Mutex<Vec<(String, String)>>,
}

impl ScriptedMojang {
    /// A script with no answers at all: for conversations that must never
    /// reach Mojang, and whose assertions include exactly that.
    fn silent() -> Self {
        Self {
            answers: Mutex::new(VecDeque::new()),
            questions: Mutex::new(Vec::new()),
        }
    }
    fn answering(profile: Profile) -> Self {
        Self {
            answers: Mutex::new(VecDeque::from([Ok(Some(profile))])),
            questions: Mutex::new(Vec::new()),
        }
    }

    fn answering_no_one() -> Self {
        Self {
            answers: Mutex::new(VecDeque::from([Ok(None)])),
            questions: Mutex::new(Vec::new()),
        }
    }

    fn broken(error: SessionError) -> Self {
        Self {
            answers: Mutex::new(VecDeque::from([Err(error)])),
            questions: Mutex::new(Vec::new()),
        }
    }

    fn questions(&self) -> Vec<(String, String)> {
        self.locked(&self.questions).clone()
    }

    fn locked<'a, T>(&'a self, mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
        match mutex.lock() {
            LockResult::Ok(guard) => guard,
            LockResult::Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl SessionServer for ScriptedMojang {
    async fn join(&self, _request: JoinRequest<'_>) -> Result<(), SessionError> {
        // A Dust server never speaks the join half; see `session`'s docs.
        Err(SessionError::Malformed {
            reason: "the server side never joins".to_owned(),
        })
    }

    async fn has_joined(
        &self,
        username: &str,
        server_id_hash: &str,
    ) -> Result<Option<Profile>, SessionError> {
        self.locked(&self.questions)
            .push((username.to_owned(), server_id_hash.to_owned()));
        self.locked(&self.answers)
            .pop_front()
            .expect("every script has exactly one answer")
    }
}

// A shared reference asks the same script, so the test can hold an `Arc`
// for assertions while the spawned handler holds its own borrow.
impl SessionServer for &ScriptedMojang {
    async fn join(&self, request: JoinRequest<'_>) -> Result<(), SessionError> {
        (**self).join(request).await
    }

    async fn has_joined(
        &self,
        username: &str,
        server_id_hash: &str,
    ) -> Result<Option<Profile>, SessionError> {
        (**self).has_joined(username, server_id_hash).await
    }
}

/// The profile Mojang would answer for Notch, textures and signature intact.
fn notch_profile() -> Profile {
    Profile {
        id: ProfileId::parse("853c80ef3c3749fdaa49938b674adae6").expect("fixture id"),
        name: "Notch".to_owned(),
        properties: vec![ProfileProperty {
            name: "textures".to_owned(),
            value: "dGV4dHVyZXM=".to_owned(),
            signature: Some("c2lnbmF0dXJl".to_owned()),
        }],
    }
}

// ---------------------------------------------------------------------------
// Wire helpers, in the login phase's own shapes.
// ---------------------------------------------------------------------------

fn var_int(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    write_var_int(value, &mut out);
    out
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    write_var_int(value.len() as i32, out);
    out.extend_from_slice(value.as_bytes());
}

fn get_string(input: &[u8]) -> (&str, usize) {
    let (len, used) = read_var_int(input).expect("string length");
    (
        std::str::from_utf8(&input[used..used + len as usize]).expect("utf8"),
        used + len as usize,
    )
}

fn get_byte_array(input: &[u8]) -> (&[u8], usize) {
    let (len, used) = read_var_int(input).expect("array length");
    let len = len as usize;
    (&input[used..used + len], used + len)
}

fn put_byte_array(out: &mut Vec<u8>, value: &[u8]) {
    write_var_int(value.len() as i32, out);
    out.extend_from_slice(value);
}

fn short_clocks() -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle: Some(Duration::from_secs(10)),
            pre_auth_budget: Some(Duration::from_secs(20)),
        },
        ..ConnConfig::default()
    }
}

/// A fixed shared secret so the digest the handler computes is checkable byte
/// for byte against what the client invented. Sixteen bytes exactly.
const CLIENT_SECRET: [u8; SHARED_SECRET_LEN] = *b"dust-fixt-secret";

/// Sends the handshake that puts the server connection into the login state,
/// which every conversation here presumes happened before the handler took
/// over. Returns nothing; the server `Conn` is left ready for Login Start.
async fn shake_hands(
    client: &mut Conn<tokio::io::DuplexStream>,
    server: &mut Conn<tokio::io::DuplexStream>,
) {
    let mut body = var_int(767);
    put_string(&mut body, "localhost");
    body.extend_from_slice(&25565u16.to_be_bytes());
    body.extend_from_slice(&var_int(2));
    client.send(Frame::new(0x00, body)).await.expect("send");

    let handshake = server.next_frame().await.expect("read").expect("handshake");
    assert_eq!(handshake.id, 0x00);
    let (_, used) = read_var_int(&handshake.body).expect("protocol");
    let (_, more) = get_string(&handshake.body[used..]);
    let (next_state, _) = read_var_int(&handshake.body[used + more + 2..]).expect("next state");
    server.handshake(next_state).expect("apply handshake");
    assert_eq!(server.state(), State::Login);
}

async fn send_login_start(client: &mut Conn<tokio::io::DuplexStream>, name: &str) {
    let mut start = Vec::new();
    put_string(&mut start, name);
    client.send(Frame::new(0x00, start)).await.expect("send");
}

/// The client's honest half of the key exchange against the recorded
/// request: parse the public key, encrypt a fixed secret plus the challenge,
/// respond, and switch to ciphertext from the next byte out.
async fn answer_encryption_request(client: &mut Conn<tokio::io::DuplexStream>) {
    let request = client.next_frame().await.expect("read").expect("request");
    assert_eq!(request.id, 0x01);
    let (_, id_len) = get_string(&request.body);
    let (public_der, der_used) = get_byte_array(&request.body[id_len..]);
    let (wire_token, _) = get_byte_array(&request.body[id_len + der_used..]);

    let public = RsaPublicKey::from_public_key_der(public_der).expect("spki parses");
    // PKCS#1 v1.5 padding is randomised; the OS generator supplies it, the
    // same way `login.rs` adapts it server-side.
    let rng = &mut rsa::rand_core::UnwrapErr(rand::rngs::SysRng);
    let encrypted_secret = public
        .encrypt(rng, Pkcs1v15Encrypt, &CLIENT_SECRET)
        .expect("encrypt secret");
    let encrypted_token = public
        .encrypt(rng, Pkcs1v15Encrypt, wire_token)
        .expect("encrypt token");

    let mut response_body = Vec::new();
    put_byte_array(&mut response_body, &encrypted_secret);
    put_byte_array(&mut response_body, &encrypted_token);
    client
        .send(Frame::new(0x01, response_body))
        .await
        .expect("send response");
    client
        .enable_encryption(&SharedSecret::from_bytes(CLIENT_SECRET))
        .await
        .expect("client switches");
}

/// Read Set Compression and put the client's codec in step with it.
async fn apply_compression(client: &mut Conn<tokio::io::DuplexStream>) -> i32 {
    let announced = client
        .next_frame()
        .await
        .expect("read")
        .expect("compression");
    assert_eq!(announced.id, 0x03, "compression announces before Success");
    let (threshold, _) = read_var_int(&announced.body).expect("threshold");
    client.set_compression(Compress::At {
        threshold: threshold as usize,
    });
    threshold
}

/// Wrap a conversation body in the wall clock every one of them must beat.
async fn run<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("the conversation stalled")
}

// ---------------------------------------------------------------------------
// Offline mode.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_login_walks_compression_success_acknowledged_in_order() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "Steve").await;

        let mojang = ScriptedMojang::answering_no_one();
        let driver = tokio::spawn(async move {
            let outcome = LoginHandler::new(&mut server, LoginConfig::offline(), &mojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        // The client sees Set Compression, then Success, nothing between.
        let threshold = apply_compression(&mut client).await;
        assert_eq!(threshold, 256, "vanilla's default threshold");

        let success = client.next_frame().await.expect("read").expect("success");
        assert_eq!(success.id, 0x02);
        let expected_id = offline_profile_id("steve");
        assert_eq!(
            &success.body[..16],
            &expected_id[..],
            "the id is MD5 over OfflinePlayer:steve"
        );
        let (name, used) = get_string(&success.body[16..]);
        assert_eq!(name, "Steve", "display case survives");
        // Offsets: `used` is relative to the name slice, which itself began
        // after sixteen id bytes.
        let (property_count, _) = read_var_int(&success.body[16 + used..]).expect("properties");
        assert_eq!(property_count, 0, "offline profiles carry no properties");

        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");

        let (outcome, mut server) = driver.await.expect("no panic");
        let authenticated = outcome.expect("authenticated");
        assert_eq!(authenticated.username, "Steve");
        assert_eq!(authenticated.profile_id, expected_id);
        assert!(authenticated.profile.is_none(), "offline has no profile");
        assert_eq!(server.state(), State::Configuration);
        assert_eq!(server.configuration_count(), 1);

        // One more frame each way, encrypted nowhere but framed normally.
        client.send(Frame::new(0x64, b"hello")).await.expect("send");
        let first_play = server.next_frame().await.expect("read").expect("frame");
        assert_eq!(first_play, Frame::new(0x64, b"hello"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_arriving_with_surrounding_whitespace_is_canonicalised_once() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "\t Steve ").await;

        let mojang = ScriptedMojang::answering_no_one();
        let driver = tokio::spawn(async move {
            let outcome = LoginHandler::new(&mut server, LoginConfig::offline(), &mojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        apply_compression(&mut client).await;
        let success = client.next_frame().await.expect("read").expect("success");
        let (name, _) = get_string(&success.body[16..]);
        assert_eq!(name, "Steve", "trimmed once, at the boundary");
        assert_eq!(
            &success.body[..16],
            &offline_profile_id("steve")[..],
            "and the id derives from the comparison form"
        );

        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");
        let (outcome, _) = driver.await.expect("no panic");
        outcome.expect("authenticated");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_illegal_name_is_refused_with_a_disconnect_and_without_a_success() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "no!").await;

        let mojang = ScriptedMojang::silent();
        let driver = tokio::spawn(async move {
            let outcome = LoginHandler::new(&mut server, LoginConfig::offline(), &mojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        // The very next thing on the wire is the refusal, and it says why.
        let refused = client
            .next_frame()
            .await
            .expect("read")
            .expect("disconnect");
        assert_eq!(refused.id, 0x00, "a Disconnect, not a hangup");
        let (reason, _) = get_string(&refused.body);
        assert!(reason.contains("Invalid username"), "{reason}");

        let (outcome, _server) = driver.await.expect("no panic");
        let error = outcome.expect_err("refused");
        assert!(
            matches!(error, LoginError::BadUsername(ref bad) if bad.attempted == "no!"),
            "{error}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn compression_left_unconfigured_means_no_announcement_at_all() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "abc").await;

        let mojang = ScriptedMojang::answering_no_one();
        let driver = tokio::spawn(async move {
            let config = LoginConfig {
                compression_threshold: None,
                ..LoginConfig::offline()
            };
            let outcome = LoginHandler::new(&mut server, config, &mojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        // First frame after Start is the Success itself.
        let success = client.next_frame().await.expect("read").expect("success");
        assert_eq!(success.id, 0x02, "no compression announcement came first");

        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");
        let (outcome, _) = driver.await.expect("no panic");
        outcome.expect("authenticated");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Online mode.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_online_login_verifies_through_mojang_and_switches_both_modes() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "notch_fan").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::answering(notch_profile()));
        let asked = mojang.clone();
        let driver = tokio::spawn(async move {
            let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)
                .expect("fixture key loads");
            let outcome = LoginHandler::new(
                &mut server,
                LoginConfig::online(),
                mojang.as_ref(),
                Some(&server_key),
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        // The client plays its half honestly.
        answer_encryption_request(&mut client).await;
        apply_compression(&mut client).await;

        // Login Success carries Mojang's answer, not the client's claim.
        let success = client.next_frame().await.expect("read").expect("success");
        assert_eq!(success.id, 0x02);
        let expected_id: [u8; 16] = [
            0x85, 0x3c, 0x80, 0xef, 0x3c, 0x37, 0x49, 0xfd, 0xaa, 0x49, 0x93, 0x8b, 0x67, 0x4a,
            0xda, 0xe6,
        ];
        assert_eq!(
            &success.body[..16],
            &expected_id[..],
            "Mojang's id, raw bytes"
        );
        let (name, used) = get_string(&success.body[16..]);
        assert_eq!(name, "Notch", "Mojang's spelling, not the claimed name");
        let (count, more) = read_var_int(&success.body[16 + used..]).expect("property count");
        assert_eq!(count, 1, "the textures property rode along");
        let properties = &success.body[16 + used + more..];
        let (property_name, name_used) = get_string(properties);
        assert_eq!(property_name, "textures");
        let (property_value, value_used) = get_string(&properties[name_used..]);
        assert_eq!(property_value, "dGV4dHVyZXM=");
        let signed_at = name_used + value_used;
        let signed = properties[signed_at];
        assert_eq!(signed, 1, "signedness survives the encoding");
        let (signature, _) = get_string(&properties[signed_at + 1..]);
        assert_eq!(signature, "c2lnbmF0dXJl");

        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");
        let (outcome, mut server) = driver.await.expect("no panic");
        let authenticated = outcome.expect("authenticated");
        assert_eq!(authenticated.username, "Notch");
        assert_eq!(authenticated.profile_id, expected_id);
        assert_eq!(
            authenticated.profile.as_ref().expect("profile").name,
            "Notch"
        );

        // The question Mojang got carried the claimed name and the true
        // digest of this exact exchange — recomputed here, independently.
        let digest = dust_net::login::server_id_hash(
            "",
            &SharedSecret::from_bytes(CLIENT_SECRET),
            testkeys::PUBLIC_KEY_SPKI_DER,
        );
        assert_eq!(
            asked.questions(),
            vec![("notch_fan".to_owned(), digest)],
            "one question, the right name, the right digest"
        );

        // Both directions still work, encrypted and compressed.
        server
            .send(Frame::new(0x63, b"post-auth"))
            .await
            .expect("send");
        let round_trip = client.next_frame().await.expect("read").expect("frame");
        assert_eq!(round_trip, Frame::new(0x63, b"post-auth"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unverified_player_is_disconnected_before_any_success_is_written() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "imposter").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::answering_no_one());
        let driver = tokio::spawn(async move {
            let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)
                .expect("fixture key loads");
            let outcome = LoginHandler::new(
                &mut server,
                LoginConfig::online(),
                mojang.as_ref(),
                Some(&server_key),
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        // Play the exchange honestly; the failure comes later, from Mojang.
        answer_encryption_request(&mut client).await;

        // Next frame is the Disconnect naming the verdict — no Success first.
        let refused = client
            .next_frame()
            .await
            .expect("read")
            .expect("disconnect");
        assert_eq!(refused.id, 0x00);
        let (reason, _) = get_string(&refused.body);
        assert!(reason.contains("Failed to verify username"), "{reason}");

        let (outcome, _) = driver.await.expect("no panic");
        let error = outcome.expect_err("refused");
        assert!(
            matches!(error, LoginError::Unverified { ref username } if username == "imposter"),
            "{error}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_down_session_server_blames_the_service_not_the_player() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "anyone").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::broken(SessionError::Unavailable {
            status: 503,
        }));
        let driver = tokio::spawn(async move {
            let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)
                .expect("fixture key loads");
            let outcome = LoginHandler::new(
                &mut server,
                LoginConfig::online(),
                mojang.as_ref(),
                Some(&server_key),
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        answer_encryption_request(&mut client).await;

        let refused = client
            .next_frame()
            .await
            .expect("read")
            .expect("disconnect");
        let (reason, _) = get_string(&refused.body);
        assert!(reason.contains("authservers_down"), "{reason}");

        let (outcome, _) = driver.await.expect("no panic");
        let error = outcome.expect_err("failed");
        assert!(
            matches!(
                error,
                LoginError::Session(SessionError::Unavailable { status: 503 })
            ),
            "{error}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verify_token_that_answers_a_different_challenge_ends_the_exchange() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "replayer").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::answering(notch_profile()));
        let asked = mojang.clone();
        let driver = tokio::spawn(async move {
            let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)
                .expect("fixture key loads");
            let outcome = LoginHandler::new(
                &mut server,
                LoginConfig::online(),
                mojang.as_ref(),
                Some(&server_key),
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        // The replay: parse the request but echo a token from some other
        // challenge, correctly encrypted under the real key.
        let request = client.next_frame().await.expect("read").expect("request");
        assert_eq!(request.id, 0x01);
        let (_, id_len) = get_string(&request.body);
        let (public_der, der_used) = get_byte_array(&request.body[id_len..]);
        let (_seen_token, _) = get_byte_array(&request.body[id_len + der_used..]);
        let public = RsaPublicKey::from_public_key_der(public_der).expect("spki parses");
        let rng = &mut rsa::rand_core::UnwrapErr(rand::rngs::SysRng);
        let stale_token = b"OLD!";
        let mut response_body = Vec::new();
        put_byte_array(
            &mut response_body,
            &public
                .encrypt(rng, Pkcs1v15Encrypt, &CLIENT_SECRET)
                .expect("encrypt"),
        );
        put_byte_array(
            &mut response_body,
            &public
                .encrypt(rng, Pkcs1v15Encrypt, stale_token)
                .expect("encrypt"),
        );
        client
            .send(Frame::new(0x01, response_body))
            .await
            .expect("send");
        // The replaying client still switches; the server refuses regardless.
        client
            .enable_encryption(&SharedSecret::from_bytes(CLIENT_SECRET))
            .await
            .expect("switch");

        // No Disconnect comes, on purpose: the two ends now disagree about
        // which bytes are ciphertext, and vanilla drops here rather than
        // speak across a mode it cannot match.
        let (outcome, server) = driver.await.expect("no panic");
        drop(server);
        let error = outcome.expect_err("refused");
        assert!(
            matches!(error, LoginError::KeyExchange(_)),
            "the structured cause is the key exchange: {error}"
        );
        assert!(
            asked.questions().is_empty(),
            "a failed challenge never reaches Mojang"
        );
        // With the server connection gone, the client observes the hangup.
        let ended = client.next_frame().await.expect("read");
        assert_eq!(ended, None, "the connection ends without a verdict frame");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_from_another_conversation_is_refused_at_the_first_step() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;

        // An Encryption Response where a Login Start belongs.
        client
            .send(Frame::new(0x01, vec![0x00]))
            .await
            .expect("send");

        let mojang = ScriptedMojang::silent();
        let driver = tokio::spawn(async move {
            let outcome = LoginHandler::new(&mut server, LoginConfig::offline(), &mojang, None)
                .authenticate()
                .await;
            (outcome, server)
        });

        let refused = client
            .next_frame()
            .await
            .expect("read")
            .expect("disconnect");
        assert_eq!(refused.id, 0x00, "refused by name");
        let (outcome, _) = driver.await.expect("no panic");
        let error = outcome.expect_err("refused");
        assert!(
            matches!(error, LoginError::UnexpectedFrame { .. }),
            "wrong packet, wrong place: {error}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn online_mode_without_a_server_key_refuses_before_anything_is_sent() {
    run(async {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut client = Conn::new(client_io, short_clocks());
        let mut server = Conn::new(server_io, short_clocks());
        shake_hands(&mut client, &mut server).await;
        send_login_start(&mut client, "whoever").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::answering(notch_profile()));
        let driver = tokio::spawn(async move {
            let outcome =
                LoginHandler::new(&mut server, LoginConfig::online(), mojang.as_ref(), None)
                    .authenticate()
                    .await;
            (outcome, server)
        });

        // The caller mistake surfaces immediately — and still politely, as a
        // Disconnect rather than silence, because the transport is alive.
        let refused = client
            .next_frame()
            .await
            .expect("read")
            .expect("disconnect");
        assert_eq!(refused.id, 0x00);
        let (outcome, _) = driver.await.expect("no panic");
        let error = outcome.expect_err("failed");
        assert!(matches!(error, LoginError::MissingServerKey), "{error}");
    })
    .await;
}
