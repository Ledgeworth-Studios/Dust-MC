//! The whole driver surface, over real loopback sockets.
//!
//! Every other conversation in this crate runs over duplexes, which proves
//! things about the driver precisely because nothing in it branches on what
//! the stream is. This file closes the remaining gap: the same conversations
//! run over actual TCP — kernel buffers, segmentation, Nagle and all —
//! because a transport that never met a socket is a claim about sockets
//! nobody has made. Loopback needs no network and no credentials:
//! `127.0.0.1` on an OS-chosen port is as reliable in CI as it is here, so
//! these tests run unconditionally, never behind `#[ignore]`.
//!
//! The scripts cover the three shapes production exercises:
//!
//! * **Status**: the unauthenticated ping a server list sends constantly.
//! * **Offline login**: Start → Set Compression → Success → Acknowledged,
//!   compression live in both directions.
//! * **Online login**: key exchange, Mojang scripted, encryption *and*
//!   compression live.
//!
//! Each authenticated connection then ends with the mid-session case that
//! motivated the two-clock design: the client falls silent after entering
//! configuration, and the idle timeout must cut it off — reported through
//! the counters as exactly one idle timeout and nothing else.

use std::sync::{LockResult, Mutex, MutexGuard};
use std::time::Duration;

use dust_net::crypt::{SharedSecret, SHARED_SECRET_LEN};
use dust_net::frame::{Compress, Frame};
use dust_net::io::{Conn, ConnConfig, ConnError, Timeouts};
use dust_net::login::ServerKey;
use dust_net::session::{
    JoinRequest, Profile, ProfileId, ProfileProperty, SessionError, SessionServer,
};
use dust_net::state::State;
use dust_net::testkeys;
use dust_net::varint::{read_var_int, write_var_int};

/// A listening loopback socket and the address to reach it on.
struct Loopback {
    listener: std::net::TcpListener,
    address: std::net::SocketAddr,
}

impl Loopback {
    /// Bind on an OS-chosen port: no collisions with anything, including a
    /// parallel CI job on the same host.
    fn bind() -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback bind");
        let address = listener.local_addr().expect("local addr");
        Self { listener, address }
    }

    /// Connect one client and accept its server half, both already async.
    ///
    /// Blocking mode until the last moment, nonblocking before `from_std`,
    /// which is the conversion every real accept loop above this crate will
    /// perform.
    async fn pair(&self) -> (Conn<tokio::net::TcpStream>, Conn<tokio::net::TcpStream>) {
        let client_std = std::net::TcpStream::connect(self.address).expect("connect");
        let (server_std, _) = self.listener.accept().expect("accept");
        let client_io = to_tokio(client_std);
        let server_io = to_tokio(server_std);
        (
            Conn::new(
                client_io,
                config(Duration::from_secs(10), Duration::from_secs(20)),
            ),
            Conn::new(
                server_io,
                config(Duration::from_secs(10), Duration::from_secs(20)),
            ),
        )
    }
}

fn to_tokio(stream: std::net::TcpStream) -> tokio::net::TcpStream {
    stream.set_nonblocking(true).expect("nonblocking");
    tokio::net::TcpStream::from_std(stream).expect("async socket")
}

fn config(idle: Duration, pre_auth: Duration) -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle: Some(idle),
            pre_auth_budget: Some(pre_auth),
        },
        ..ConnConfig::default()
    }
}

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

async fn shake_hands(
    client: &mut Conn<tokio::net::TcpStream>,
    server: &mut Conn<tokio::net::TcpStream>,
    intent: i32,
) {
    let mut body = var_int(767);
    put_string(&mut body, "localhost");
    body.extend_from_slice(&25565u16.to_be_bytes());
    body.extend_from_slice(&var_int(intent));
    client.send(Frame::new(0x00, body)).await.expect("send");

    let handshake = server.next_frame().await.expect("read").expect("handshake");
    assert_eq!(handshake.id, 0x00);
    let (_, used) = read_var_int(&handshake.body).expect("protocol");
    let (_, more) = get_string(&handshake.body[used..]);
    let (next_state, _) = read_var_int(&handshake.body[used + more + 2..]).expect("next state");
    assert_eq!(next_state, intent);
    server.handshake(next_state).expect("apply handshake");
}

async fn send_login_start(client: &mut Conn<tokio::net::TcpStream>, name: &str) {
    let mut start = Vec::new();
    put_string(&mut start, name);
    // The profile id: sixteen raw bytes, mandatory since 1.20.5, no presence
    // flag. Its value is ignored by the server — offline mode derives its own
    // and online mode takes Mojang's — but its *length* is checked, because a
    // body of the wrong length is a client that cannot be talked to.
    start.extend_from_slice(&[0x11; 16]);
    client.send(Frame::new(0x00, start)).await.expect("send");
}

async fn apply_compression(client: &mut Conn<tokio::net::TcpStream>) -> i32 {
    let announced = client.next_frame().await.expect("read").expect("setcomp");
    assert_eq!(announced.id, 0x03);
    let (threshold, _) = read_var_int(&announced.body).expect("threshold");
    client.set_compression(Compress::At {
        threshold: threshold as usize,
    });
    threshold
}

/// The scripted Mojang both online conversations here use.
#[derive(Debug)]
struct ScriptedMojang(Mutex<Option<Profile>>);

impl ScriptedMojang {
    fn answering(profile: Profile) -> Self {
        Self(Mutex::new(Some(profile)))
    }

    fn locked(&self) -> LockResult<MutexGuard<'_, Option<Profile>>> {
        self.0.lock()
    }
}

impl SessionServer for ScriptedMojang {
    async fn join(&self, _request: JoinRequest<'_>) -> Result<(), SessionError> {
        Err(SessionError::Malformed {
            reason: "the server side never joins".to_owned(),
        })
    }

    async fn has_joined(
        &self,
        _username: &str,
        _server_id_hash: &str,
    ) -> Result<Option<Profile>, SessionError> {
        match self.locked() {
            LockResult::Ok(mut slot) => Ok(slot.take()),
            LockResult::Err(poisoned) => Ok(poisoned.into_inner().take()),
        }
    }
}

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

const CLIENT_SECRET: [u8; SHARED_SECRET_LEN] = *b"dust-fixt-secret";

async fn answer_encryption_request(client: &mut Conn<tokio::net::TcpStream>) {
    use rsa::pkcs8::DecodePublicKey as _;
    use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

    let request = client.next_frame().await.expect("read").expect("request");
    assert_eq!(request.id, 0x01);
    let (_, id_len) = get_string(&request.body);
    let (public_der, der_used) = get_byte_array(&request.body[id_len..]);
    let (wire_token, _) = get_byte_array(&request.body[id_len + der_used..]);

    let public = RsaPublicKey::from_public_key_der(public_der).expect("spki parses");
    // PKCS#1 v1.5 padding is randomised; the operating system supplies that,
    // exactly as `login.rs` adapts it server-side.
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
        .expect("send");
    client
        .enable_encryption(&SharedSecret::from_bytes(CLIENT_SECRET))
        .await
        .expect("client switches");
}

/// Wrap a conversation body in the wall clock everything here must beat even
/// when the machine is busy.
async fn run<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("the conversation stalled")
}

// ---------------------------------------------------------------------------
// Status.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_server_list_ping_survives_a_real_kernel() {
    run(async {
        let loopback = Loopback::bind();
        let (mut client, mut server) = loopback.pair().await;
        shake_hands(&mut client, &mut server, 1).await;
        assert_eq!(server.state(), State::Status);

        client
            .send(Frame::new(0x00, Vec::new()))
            .await
            .expect("send");
        let request = server.next_frame().await.expect("read").expect("request");
        assert_eq!(request.id, 0x00);

        let mut response = Vec::new();
        put_string(
            &mut response,
            r#"{"version":{"name":"Dust","protocol":767}}"#,
        );
        server.send(Frame::new(0x00, response)).await.expect("send");
        let answered = client.next_frame().await.expect("read").expect("response");
        let (json, _) = get_string(&answered.body);
        assert!(json.contains("\"Dust\""), "{json}");

        // Ping-pong, then a clean end the kernel itself delivers.
        let payload = 0x0102_0304_0506_0708u64.to_be_bytes();
        client.send(Frame::new(0x01, payload)).await.expect("send");
        let ping = server.next_frame().await.expect("read").expect("ping");
        assert_eq!(ping.body, payload.to_vec());
        server
            .send(Frame::new(0x01, ping.body))
            .await
            .expect("send");
        let pong = client.next_frame().await.expect("read").expect("pong");
        assert_eq!(pong.body, payload.to_vec());

        client.close().await.expect("close");
        let ended = server.next_frame().await.expect("read");
        assert_eq!(ended, None, "FIN arrives as a clean end");
        let stats = server.stats();
        assert_eq!(stats.frames_in, 3, "handshake, request, ping");
        assert!(stats.bytes_in > 0 && stats.bytes_out > 0);
    })
    .await;
}

// ---------------------------------------------------------------------------
// Offline login, compression live, ending in a mid-session idle timeout.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_login_runs_to_configuration_and_the_idle_clock_ends_silence() {
    run(async {
        let loopback = Loopback::bind();
        let (mut client, mut server) = loopback.pair().await;
        shake_hands(&mut client, &mut server, 2).await;
        send_login_start(&mut client, "Steve").await;

        // The handler runs against the real socket on its own task; the test
        // plays the client here.
        let mojang = std::sync::Arc::new(ScriptedMojang(std::sync::Mutex::new(None)));
        let driver = tokio::spawn(async move {
            let outcome = dust_net::login_flow::LoginHandler::new(
                &mut server,
                dust_net::login_flow::LoginConfig::offline(),
                mojang.as_ref(),
                None,
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        apply_compression(&mut client).await;
        let success = client.next_frame().await.expect("read").expect("success");
        assert_eq!(success.id, 0x02);
        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");

        let (outcome, mut server) = driver.await.expect("no panic");
        outcome.expect("authenticated");
        assert_eq!(server.state(), State::Configuration);
        assert_eq!(server.configuration_count(), 1);

        // One frame of play traffic each way, compressed over TCP.
        client.send(Frame::new(0x64, b"play")).await.expect("send");
        let first = server.next_frame().await.expect("read").expect("frame");
        assert_eq!(first, Frame::new(0x64, b"play"));

        // Mid-session silence: the client stops talking, and the idle clock —
        // not any pre-auth budget, which authentication retired — ends it.
        let result = tokio::time::timeout(Duration::from_secs(10), server.next_frame())
            .await
            .expect("the idle timeout itself is overdue");
        let Err(error) = result else {
            panic!("expected a terminal error, got {result:?}");
        };

        match error {
            ConnError::IdleTimeout { limit } => {
                assert_eq!(limit, Duration::from_secs(10))
            }
            other => panic!("expected an idle timeout, got {other:?}"),
        }
        let stats = server.stats();
        assert_eq!(stats.idle_timeouts, 1, "counted once, where it happened");
        assert_eq!(
            stats.pre_auth_deadlines, 0,
            "authentication retired that clock"
        );
        drop(client);
    })
    .await;
}

// ---------------------------------------------------------------------------
// Online login, encryption and compression live, same mid-session ending.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_online_login_encrypts_over_tcp_and_idle_timeouts_mid_session() {
    run(async {
        let loopback = Loopback::bind();

        // Server clocks: generous enough for RSA keygen latency on the
        // fixture key, tight enough that silence is noticed promptly.
        let client_std = std::net::TcpStream::connect(loopback.address).expect("connect");
        let (server_std, _) = loopback.listener.accept().expect("accept");
        let mut client = Conn::new(
            to_tokio(client_std),
            config(Duration::from_secs(10), Duration::from_secs(30)),
        );
        let mut server = Conn::new(
            to_tokio(server_std),
            config(Duration::from_millis(500), Duration::from_secs(30)),
        );

        shake_hands(&mut client, &mut server, 2).await;
        send_login_start(&mut client, "notch_fan").await;

        let mojang = std::sync::Arc::new(ScriptedMojang::answering(notch_profile()));
        let driver = tokio::spawn(async move {
            let server_key = ServerKey::from_pkcs8_der(testkeys::PRIVATE_KEY_PKCS8_DER)
                .expect("fixture key loads");
            let outcome = dust_net::login_flow::LoginHandler::new(
                &mut server,
                dust_net::login_flow::LoginConfig::online(),
                mojang.as_ref(),
                Some(&server_key),
            )
            .authenticate()
            .await;
            (outcome, server)
        });

        answer_encryption_request(&mut client).await;
        apply_compression(&mut client).await;

        let success = client.next_frame().await.expect("read").expect("success");
        assert_eq!(success.id, 0x02);
        let expected_id: [u8; 16] = [
            0x85, 0x3c, 0x80, 0xef, 0x3c, 0x37, 0x49, 0xfd, 0xaa, 0x49, 0x93, 0x8b, 0x67, 0x4a,
            0xda, 0xe6,
        ];
        assert_eq!(&success.body[..16], &expected_id[..]);
        let (name, _) = get_string(&success.body[16..]);
        assert_eq!(name, "Notch", "Mojang's spelling crossed the real wire");

        client
            .send(Frame::new(0x03, Vec::<u8>::new()))
            .await
            .expect("ack");
        let (outcome, mut server) = driver.await.expect("no panic");
        outcome.expect("authenticated");
        assert_eq!(server.state(), State::Configuration);

        // Encrypted, compressed frames cross TCP intact, both directions.
        server
            .send(Frame::new(0x63, b"ciphertext"))
            .await
            .expect("send");
        let round_trip = client.next_frame().await.expect("read").expect("frame");
        assert_eq!(round_trip, Frame::new(0x63, b"ciphertext"));

        // Then silence, and the idle clock fires inside the tighter window
        // this connection was given.
        let result = tokio::time::timeout(Duration::from_secs(10), server.next_frame())
            .await
            .expect("the idle timeout itself is overdue");
        let Err(error) = result else {
            panic!("expected a terminal error, got {result:?}");
        };
        assert!(
            matches!(&error, ConnError::IdleTimeout { limit } if *limit == Duration::from_millis(500)),
            "{error}"
        );
        let stats = server.stats();
        assert_eq!(stats.idle_timeouts, 1);
        drop(client);
    }).await;
}
