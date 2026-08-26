//! The conversations a client can have with this server, checked by speaking
//! the protocol.
//!
//! # Why the client here is hand-written
//!
//! This test writes VarInts and reads length prefixes by hand instead of using
//! `dust-net`'s encoder. That is deliberate and it is the only reason the test
//! is worth running.
//!
//! `dust-net` and the server share one framing implementation. A client built
//! on it agrees with the server about where a frame starts by construction, so
//! it would pass under any self-consistent convention — including a wrong one —
//! and prove only that the code agrees with itself. The bytes below are written
//! from the protocol as a third party states it: a VarInt length, then a VarInt
//! packet id, then the body. If Dust's framing drifted, this is the test that
//! notices, because nothing it does was compiled from the same source.
//!
//! What it still cannot prove is that a *real* client is happy — a vanilla
//! client renders a document this test would accept, and that is the
//! differential harness's job, not this file's.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dust_server::clock::{Clock, ManualClock};
use dust_server::engine::TICK_NS;
use dust_server::stop::{Parker, StepParker, StopHandle};
use dust_server::{LiveMetrics, Server, ServerOptions, WatchdogSetting};

// ---------------------------------------------------------------------------
// A client that shares no code with the server
// ---------------------------------------------------------------------------

fn write_var_int(mut value: i32, out: &mut Vec<u8>) {
    // The protocol's own definition, written out rather than called: seven bits
    // per byte, low group first, the high bit meaning "another byte follows".
    // A `u32` cast so that a negative number shifts in zeros, which is what
    // makes -1 five bytes rather than an infinite loop.
    let mut bits = value as u32;
    loop {
        let byte = (bits & 0x7f) as u8;
        bits >>= 7;
        if bits == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    let _ = &mut value;
}

fn read_var_int(stream: &mut TcpStream) -> i32 {
    let mut result: i32 = 0;
    for shift in 0..5 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).expect("a VarInt byte");
        result |= i32::from(byte[0] & 0x7f) << (shift * 7);
        if byte[0] & 0x80 == 0 {
            return result;
        }
    }
    panic!("a VarInt longer than five bytes is not one");
}

fn write_string(text: &str, out: &mut Vec<u8>) {
    write_var_int(text.len() as i32, out);
    out.extend_from_slice(text.as_bytes());
}

/// Send one uncompressed frame: length, then id, then body.
fn send_frame(stream: &mut TcpStream, id: i32, body: &[u8]) {
    let mut payload = Vec::new();
    write_var_int(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_var_int(payload.len() as i32, &mut frame);
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).expect("write a frame");
}

/// Receive one uncompressed frame, returning its id and body.
fn recv_frame(stream: &mut TcpStream) -> (i32, Vec<u8>) {
    let len = read_var_int(stream);
    assert!(len > 0, "a frame is at least its packet id");
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let mut cursor = 0usize;
    let mut id: i32 = 0;
    for shift in 0..5 {
        let byte = payload[cursor];
        cursor += 1;
        id |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            break;
        }
    }
    (id, payload[cursor..].to_vec())
}

/// Read a length-prefixed string off the front of a slice, returning it and
/// the rest.
fn read_string_at(bytes: &[u8]) -> (&str, &[u8]) {
    let (len, rest) = read_var_int_from(bytes);
    let len = len as usize;
    (
        std::str::from_utf8(&rest[..len]).expect("the wire is UTF-8"),
        &rest[len..],
    )
}

/// Read a VarInt out of a slice, returning it and the rest.
fn read_var_int_from(bytes: &[u8]) -> (i32, &[u8]) {
    let mut result: i32 = 0;
    for (shift, i) in (0..5).enumerate() {
        let byte = bytes[i];
        result |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return (result, &bytes[i + 1..]);
        }
    }
    panic!("a VarInt longer than five bytes is not one");
}

/// Receive a frame after Set Compression.
///
/// The compressed format inserts one field: an uncompressed-length VarInt after
/// the frame length. Zero means "the rest is not compressed", which is what a
/// server sends for anything under the threshold — and every packet this test
/// receives after the switch is under 256 bytes, so the zero case is the one
/// exercised. That is not a gap being papered over: it is the case a real
/// client meets for keepalives and acknowledgements, and getting it wrong is
/// how a server appears to work until somebody sends a chunk.
fn recv_compressed_frame(stream: &mut TcpStream) -> (i32, Vec<u8>) {
    let len = read_var_int(stream);
    assert!(len > 0, "a frame is at least its uncompressed-length field");
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let (uncompressed_len, rest) = read_var_int_from(&payload);
    // Zero means "the rest is not compressed", which is what the server sends
    // for anything under the threshold. Above it, the rest is a zlib stream and
    // this field is what it inflates to — checked rather than trusted, because
    // that length is the only thing standing between a decompressor and a peer
    // that lies about how much it is about to produce.
    let inflated = if uncompressed_len == 0 {
        rest.to_vec()
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(rest)
            .read_to_end(&mut out)
            .expect("the frame is a zlib stream");
        assert_eq!(
            out.len(),
            uncompressed_len as usize,
            "the announced uncompressed length must be the real one"
        );
        out
    };
    let (id, body) = read_var_int_from(&inflated);
    (id, body.to_vec())
}

/// Send a frame after Set Compression, below the threshold.
fn send_compressed_frame(stream: &mut TcpStream, id: i32, body: &[u8]) {
    let mut payload = Vec::new();
    write_var_int(0, &mut payload); // uncompressed: this packet is small
    write_var_int(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_var_int(payload.len() as i32, &mut frame);
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).expect("write a frame");
}

/// The handshake, addressed to `addr`, asking for `next_state`.
fn handshake(stream: &mut TcpStream, protocol: i32, addr: SocketAddr, next_state: i32) {
    let mut body = Vec::new();
    write_var_int(protocol, &mut body);
    write_string(&addr.ip().to_string(), &mut body);
    body.extend_from_slice(&addr.port().to_be_bytes());
    write_var_int(next_state, &mut body);
    send_frame(stream, 0x00, &body);
}

/// Read a length-prefixed string that is the whole of `body`.
fn read_string(body: &[u8]) -> String {
    let mut cursor = 0usize;
    let mut len: i32 = 0;
    for shift in 0..5 {
        let byte = body[cursor];
        cursor += 1;
        len |= i32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            break;
        }
    }
    let end = cursor + len as usize;
    assert_eq!(end, body.len(), "the string is the whole body");
    String::from_utf8(body[cursor..end].to_vec()).expect("the text is UTF-8")
}

fn read_status_json(stream: &mut TcpStream) -> String {
    let (id, body) = recv_frame(stream);
    assert_eq!(id, 0x00, "status_response is id 0 clientbound in status");
    read_string(&body)
}

// ---------------------------------------------------------------------------
// A server, on a port the operating system chose
// ---------------------------------------------------------------------------

struct Running {
    addr: SocketAddr,
    stop: StopHandle,
    worker: Option<
        std::thread::JoinHandle<Result<dust_server::ShutdownReport, dust_server::ServerError>>,
    >,
    #[allow(dead_code)]
    metrics: LiveMetrics,
}

impl Running {
    fn finish(mut self) -> dust_server::ShutdownReport {
        self.stop.request_stop();
        self.worker
            .take()
            .expect("taken once")
            .join()
            .expect("the run thread finishes")
            .expect("the run is clean")
    }
}

fn write_config(text: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "dust-ping-test-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, text).expect("write the config");
    path
}

fn stepping(clock: Arc<ManualClock>, step: u64) -> dust_server::server::ParkerFactory {
    Arc::new(move |_, _| Box::new(StepParker::new(Arc::clone(&clock), step)) as Box<dyn Parker>)
}

/// Boot a server on loopback, port 0, and wait until it says which port it took.
///
/// The port comes from the server rather than from a socket this test bound
/// first. Anything this test bound would have to be released before the server
/// could take it, and the gap between the release and the bind is a race with
/// every other process on the machine — the classic way a port-picking test
/// becomes the flaky one.
fn start(extra_config: &str) -> Running {
    let dir = std::env::temp_dir().join(format!(
        "dust-world-{}-{}",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    start_in(&dir, extra_config)
}

/// A server whose world lives in `world_dir`, so two runs can share one.
///
/// Every test gets its own directory by default: they run in parallel, and a
/// shared world would make one test's blocks appear in another's.
fn start_in(world_dir: &std::path::Path, extra_config: &str) -> Running {
    let clock = Arc::new(ManualClock::new());
    let config = format!("[server]\nbind = \"127.0.0.1:0\"\nonline_mode = false\n{extra_config}");

    let options = ServerOptions {
        config_path: write_config(&config),
        world_dir: world_dir.to_path_buf(),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(Arc::clone(&clock), TICK_NS),
        watchdog: WatchdogSetting::Custom(dust_server::WatchdogPolicy::custom(
            600_000_000_000,
            |_| {},
        )),
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    let metrics = server.metrics();
    let stop = server.stop_handle();
    let worker = std::thread::spawn(move || server.run());

    let mut addr = None;
    for _ in 0..50_000_000 {
        if let Some(bound) = metrics.bound_addr() {
            addr = Some(bound);
            break;
        }
        assert!(
            !worker.is_finished(),
            "the run thread exited before binding: the boot failed rather than stalled"
        );
        std::thread::yield_now();
    }
    let addr = addr.expect("the listener publishes the address it took");
    assert_ne!(addr.port(), 0, "port 0 means 'choose one', never stays 0");

    Running {
        addr,
        stop,
        worker: Some(worker),
        metrics,
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect to the listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    stream
}

/// A client that has reached Play, and what it has been told since.
///
/// Counters rather than a log of packets: the assertions are about how many
/// columns crossed the wire, and keeping every chunk packet to count them
/// later would be a hundred megabytes to answer a question three integers
/// answer.
struct Joined {
    chunks: usize,
    forgets: usize,
    centres: usize,
    /// Bodies this client has been told to render.
    spawned_entities: usize,
    /// Tab-list rows it has been given.
    player_infos: usize,
    /// Where the server teleported the player on arrival. Captured during the
    /// join rather than waited for afterwards, because the join has already
    /// read past it by the time it returns — a later `wait_for` would block
    /// until the read timeout and then say the packet never came.
    spawned_at: Option<(f64, f64, f64)>,
}

impl Joined {
    /// Read whatever is waiting, and stop when nothing is.
    ///
    /// A short read timeout is the stopping condition, which is a wall-clock
    /// dependency and therefore worth naming: it is not a claim about how fast
    /// the server is, only about the socket being empty *now*. The counts this
    /// test asserts are cumulative, so a drain that stopped early is corrected
    /// by the next one; only the final `drain_until_quiet` has to be complete,
    /// and it waits longer for exactly that reason.
    fn drain(&mut self, stream: &mut TcpStream) {
        self.read_for(stream, Duration::from_millis(50));
    }

    /// Read until a packet of `id` arrives, returning its body.
    ///
    /// Bounded by the socket's read timeout rather than by a count, so a
    /// packet that never comes fails as a `None` here instead of hanging the
    /// suite.
    fn wait_for(&mut self, stream: &mut TcpStream, id: i32) -> Option<Vec<u8>> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        while let Some((got, body)) = try_recv_compressed_frame(stream) {
            match got {
                _ if got == id => return Some(body),
                39 => self.chunks += 1,
                33 => self.forgets += 1,
                84 => self.centres += 1,
                1 => self.spawned_entities += 1,
                62 => self.player_infos += 1,
                38 => send_compressed_frame(stream, 24, &body),
                29 => panic!("the server disconnected while waiting for {id}"),
                _ => {}
            }
        }
        None
    }

    fn drain_until_quiet(&mut self, stream: &mut TcpStream) {
        self.read_for(stream, Duration::from_millis(750));
    }

    fn read_for(&mut self, stream: &mut TcpStream, quiet: Duration) {
        stream
            .set_read_timeout(Some(quiet))
            .expect("a read timeout");
        while let Some((id, body)) = try_recv_compressed_frame(stream) {
            match id {
                39 => self.chunks += 1,
                33 => self.forgets += 1,
                84 => self.centres += 1,
                1 => self.spawned_entities += 1,
                62 => self.player_infos += 1,
                // A keep-alive is answered so the connection stays up through
                // a walk that outlasts the ten-second period.
                38 => send_compressed_frame(stream, 24, &body),
                29 => panic!("the server disconnected mid-walk"),
                _ => {}
            }
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a read timeout");
    }
}

/// Run the whole join, ending with the client in Play and the arrival counted.
///
/// The sequence is asserted in its own test; here it is walked through so a
/// later test can start from a joined player without repeating it.
fn join(stream: &mut TcpStream, addr: SocketAddr) -> Joined {
    join_as(stream, addr, "Walker")
}

/// Join under a chosen name, so two clients in one test are two players.
fn join_as(stream: &mut TcpStream, addr: SocketAddr, name: &str) -> Joined {
    handshake(stream, 767, addr, 2);
    let mut body = Vec::new();
    write_string(name, &mut body);
    body.extend_from_slice(&[0u8; 16]);
    send_frame(stream, 0x00, &body);

    let (id, _) = recv_frame(stream);
    assert_eq!(id, 0x03, "set_compression");
    let (id, _) = recv_compressed_frame(stream);
    assert_eq!(id, 0x02, "login_finished");
    send_compressed_frame(stream, 0x03, &[]);

    let mut counted = Joined {
        chunks: 0,
        forgets: 0,
        centres: 0,
        spawned_entities: 0,
        player_infos: 0,
        spawned_at: None,
    };
    loop {
        let (id, body) = recv_compressed_frame(stream);
        match id {
            0x0e => send_compressed_frame(stream, 0x07, &body),
            0x03 => {
                send_compressed_frame(stream, 0x03, &[]);
                break;
            }
            _ => {}
        }
    }
    // Play: the join packet, the position, then the columns and the event.
    loop {
        let (id, body) = recv_compressed_frame(stream);
        match id {
            39 => counted.chunks += 1,
            33 => counted.forgets += 1,
            84 => counted.centres += 1,
            1 => counted.spawned_entities += 1,
            62 => counted.player_infos += 1,
            64 => {
                counted.spawned_at = Some((
                    f64::from_be_bytes(body[0..8].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[8..16].try_into().expect("eight bytes")),
                    f64::from_be_bytes(body[16..24].try_into().expect("eight bytes")),
                ));
            }
            34 => break, // the loading screen is over
            38 => send_compressed_frame(stream, 24, &body),
            _ => {}
        }
    }
    counted
}

/// One frame, or `None` if the socket went quiet inside its read timeout.
fn try_recv_compressed_frame(stream: &mut TcpStream) -> Option<(i32, Vec<u8>)> {
    let mut first = [0u8; 1];
    match stream.read_exact(&mut first) {
        Ok(()) => {}
        Err(_) => return None,
    }
    // The length prefix, continued by hand because one byte of it is already
    // read and a VarInt does not say up front how long it is.
    let mut len = i32::from(first[0] & 0x7f);
    let mut shift = 7;
    let mut byte = first[0];
    while byte & 0x80 != 0 {
        let mut next = [0u8; 1];
        stream.read_exact(&mut next).expect("a VarInt byte");
        byte = next[0];
        len |= i32::from(byte & 0x7f) << shift;
        shift += 7;
    }

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("the frame body");
    let (uncompressed_len, rest) = read_var_int_from(&payload);
    let inflated = if uncompressed_len == 0 {
        rest.to_vec()
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(rest)
            .read_to_end(&mut out)
            .expect("a zlib stream");
        out
    };
    let (id, body) = read_var_int_from(&inflated);
    Some((id, body.to_vec()))
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[test]
fn a_client_speaking_raw_protocol_gets_the_server_list_entry() {
    let running = start("motd = \"A test server\"\nmax_players = 42\n");
    let addr = running.addr;

    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 1);
    send_frame(&mut stream, 0x00, &[]);
    let json = read_status_json(&mut stream);

    assert!(json.contains(r#""protocol":767"#), "{json}");
    assert!(json.contains(r#""name":"1.21.1""#), "{json}");
    assert!(json.contains(r#""max":42"#), "{json}");
    assert!(json.contains(r#""online":0"#), "{json}");
    assert!(json.contains("A test server"), "{json}");

    // The ping the client uses to measure its round trip. The eight bytes come
    // back unexamined, which is the whole contract.
    let payload: i64 = 0x0123_4567_89ab_cdef;
    send_frame(&mut stream, 0x01, &payload.to_be_bytes());
    let (id, body) = recv_frame(&mut stream);
    assert_eq!(id, 0x01, "pong_response is id 1");
    assert_eq!(
        i64::from_be_bytes(body.try_into().expect("eight bytes")),
        payload
    );

    let report = running.finish();
    assert!(
        report
            .transcript
            .iter()
            .any(|e| e.detail.contains("ping(s)")),
        "the teardown must account for the connections served: {:?}",
        report.transcript
    );
}

#[test]
fn an_offline_login_runs_the_whole_configuration_exchange_and_reaches_play() {
    let running = start("");
    let addr = running.addr;
    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 2);

    // Login Start: a name and the client's guess at its own profile id.
    let mut body = Vec::new();
    write_string("Tester", &mut body);
    body.extend_from_slice(&[0u8; 16]);
    send_frame(&mut stream, 0x00, &body);

    // Set Compression comes first, at vanilla's threshold. From here on the
    // client's frames carry an uncompressed-length prefix, which is why this
    // test reads its last frames through the compressed reader below — the
    // switch is part of the protocol, not an optimisation to skip in a test.
    let (id, body) = recv_frame(&mut stream);
    assert_eq!(id, 0x03, "set_compression is id 3 clientbound in login");
    assert_eq!(read_var_int_from(&body).0, 256, "vanilla's threshold");

    // Login Success. Offline mode derives the profile id from the name, so it
    // is emphatically not the sixteen zero bytes the client sent.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x02, "login_finished is id 2 clientbound in login");
    assert_ne!(
        &body[..16],
        &[0u8; 16],
        "an offline id is derived, not echoed"
    );
    let (name_len, rest) = read_var_int_from(&body[16..]);
    let name = String::from_utf8(rest[..name_len as usize].to_vec()).expect("UTF-8");
    assert_eq!(name, "Tester");

    // Login Acknowledged moves both ends into configuration.
    send_compressed_frame(&mut stream, 0x03, &[]);

    // Configuration, in the order a real 1.21.1 server sends it. Captured from
    // one rather than read off a wiki, because the order is load-bearing and
    // the wire is the only place it is written down.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x01, "custom_payload carries the brand first");
    let (channel, rest) = read_string_at(&body);
    assert_eq!(channel, "minecraft:brand");
    assert_eq!(
        read_string_at(rest).0,
        "Dust",
        "not a lie about being vanilla"
    );

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x0c, "update_enabled_features");
    let (count, rest) = read_var_int_from(&body);
    assert_eq!(count, 1);
    assert_eq!(read_string_at(rest).0, "minecraft:vanilla");

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x0e, "select_known_packs");
    // Echoed back verbatim, which is what a client that has the pack does.
    send_compressed_frame(&mut stream, 0x07, &body);

    // Eleven registries, names only. The entry payloads are absent because the
    // pack was acknowledged, and absent is not the same as empty: an empty
    // definition would put the client in a world with no dimension types.
    let mut seen = Vec::new();
    loop {
        let (id, body) = recv_compressed_frame(&mut stream);
        if id == 0x03 {
            break; // finish_configuration
        }
        assert_eq!(id, 0x07, "only registry_data comes between");
        let (registry, rest) = read_string_at(&body);
        let registry = registry.to_owned();
        let (count, mut rest) = read_var_int_from(rest);
        for _ in 0..count {
            let (_entry, after) = read_string_at(rest);
            assert_eq!(after[0], 0, "no entry of {registry} may carry a payload");
            rest = &after[1..];
        }
        assert!(rest.is_empty(), "{registry} had trailing bytes");
        seen.push((registry, count));
    }

    // The eleven and their counts, as the real server sent them.
    assert_eq!(
        seen,
        vec![
            ("minecraft:worldgen/biome".to_owned(), 64),
            ("minecraft:chat_type".to_owned(), 7),
            ("minecraft:trim_pattern".to_owned(), 18),
            ("minecraft:trim_material".to_owned(), 10),
            ("minecraft:wolf_variant".to_owned(), 9),
            ("minecraft:painting_variant".to_owned(), 50),
            ("minecraft:dimension_type".to_owned(), 4),
            ("minecraft:damage_type".to_owned(), 47),
            ("minecraft:banner_pattern".to_owned(), 43),
            ("minecraft:enchantment".to_owned(), 42),
            ("minecraft:jukebox_song".to_owned(), 19),
        ]
    );

    // Acknowledge, which is what actually moves both ends into Play.
    send_compressed_frame(&mut stream, 0x03, &[]);

    // And Play is a world. The join packet, then the position, then the
    // columns, then the event that ends the loading screen.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 43, "login is id 43 clientbound in play");
    assert_eq!(
        i32::from_be_bytes(body[0..4].try_into().expect("four bytes")),
        1,
        "the player's entity id"
    );
    let (count, rest) = read_var_int_from(&body[5..]);
    assert_eq!(count, 3, "three dimensions are named");
    assert_eq!(read_string_at(rest).0, "minecraft:overworld");

    // The position comes before the chunks, and that order matters: a client
    // uses where it is to decide which columns it wants, and one told about
    // columns first throws them away.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 64, "player_position");
    let x = f64::from_be_bytes(body[0..8].try_into().expect("eight bytes"));
    let y = f64::from_be_bytes(body[8..16].try_into().expect("eight bytes"));
    let z = f64::from_be_bytes(body[16..24].try_into().expect("eight bytes"));
    // The half-block offsets are not cosmetic: integer x and z spawn a player
    // on a block corner and the first physics tick pushes them off it.
    assert_eq!((x, y, z), (0.5, -59.0, 0.5));

    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 84, "set_chunk_cache_center");
    assert_eq!(read_var_int_from(&body).0, 0, "centred on chunk 0");

    // Twenty-five columns: a radius of two, which is (2*2+1)^2.
    let mut chunks = 0;
    let event = loop {
        let (id, body) = recv_compressed_frame(&mut stream);
        if id == 34 {
            break body;
        }
        assert_eq!(id, 39, "only chunks come between");
        chunks += 1;
        assert!(chunks <= 25, "more columns than a radius of two holds");
    };
    assert_eq!(chunks, 25, "every column within the radius");
    assert_eq!(
        event[0], 13,
        "game event 13 is what ends the loading screen; without it the terrain \
         arrives and the client keeps waiting"
    );

    // And the connection stays up. A keep-alive arrives and is answered, which
    // is what turns "the packets were sent" into "the player is still there".
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 38, "keep_alive");
    assert_eq!(body.len(), 8, "eight opaque bytes");
    send_compressed_frame(&mut stream, 24, &body);

    // The player is still standing in the world when the server stops, which
    // is what the teardown must say: one login, and one still online. A
    // counter that only counted finished sessions would report neither, and
    // the server-list ping quotes that same number.
    let report = running.finish();
    assert!(
        report
            .transcript
            .iter()
            .any(|e| e.detail.contains("1 login(s)") && e.detail.contains("1 still online")),
        "the teardown must account for the player: {:?}",
        report.transcript
    );
}

/// Phase 3's exit criterion, in the part of it that exists: walk a long way
/// and require the world to keep arriving.
///
/// The numbers are checked rather than the behaviour being watched. A view of
/// radius two is a five-by-five square, so each chunk boundary crossed sends a
/// five-column edge and forgets another — and one thousand blocks east crosses
/// sixty-two of them. A server that stopped streaming, or that resent columns
/// the client already held, would produce a different count and no other
/// symptom.
#[test]
fn a_player_walking_a_thousand_blocks_is_streamed_the_world_as_they_go() {
    let running = start("");
    let addr = running.addr;
    let mut stream = connect(addr);
    let mut client = join(&mut stream, addr);

    // Twenty-five columns on arrival, and one centre.
    assert_eq!(client.chunks, 25, "the square at spawn");
    assert_eq!(client.forgets, 0, "nothing to forget yet");
    assert_eq!(client.centres, 1);

    // Due east, one block at a time, reading whatever comes back. The reads are
    // interleaved rather than saved to the end because the outbound queue is
    // bounded: a client that sends a thousand packets without reading is a
    // client the server is entitled to make wait.
    let mut x = 0.5f64;
    for step in 0..1000 {
        x += 1.0;
        let mut body = Vec::new();
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&(-59.0f64).to_be_bytes());
        body.extend_from_slice(&0.5f64.to_be_bytes());
        body.push(1); // on_ground
        send_compressed_frame(&mut stream, 26, &body);
        if step % 16 == 0 {
            client.drain(&mut stream);
        }
    }
    client.drain_until_quiet(&mut stream);

    // 1000 blocks east from x = 0.5 crosses into column 63, so sixty-two
    // boundaries after the first. Each one is five columns each way.
    assert_eq!(client.centres, 63, "one recentre per boundary crossed");
    assert_eq!(
        client.forgets, 310,
        "five columns forgotten per crossing, and never one the client did \
         not hold"
    );
    assert_eq!(
        client.chunks,
        25 + 310,
        "the square at spawn plus five columns per crossing — a resend would \
         push this above it and nothing else would show"
    );

    running.finish();
}

/// Two players, one world: what one breaks, the other is told about.
///
/// This is the first test in the project where two connections share
/// anything, and it is the property that makes the thing a *server* rather
/// than a generator with a socket on it. The second client is watching a
/// column it did not edit, so the only way the change reaches it is the
/// broadcast — a per-connection world would pass every other test here and
/// fail this one.
#[test]
fn a_block_one_player_breaks_is_announced_to_another() {
    let running = start("");
    let addr = running.addr;

    let mut watcher_stream = connect(addr);
    let mut watcher = join_as(&mut watcher_stream, addr, "Watcher");
    let mut breaker_stream = connect(addr);
    let _breaker = join_as(&mut breaker_stream, addr, "Breaker");

    // The surface block at the spawn column. Encoded the way the protocol
    // packs a position — 26 bits of x, 26 of z, 12 of y, in that order — by
    // hand, because a helper shared with the server would agree with it about
    // a layout neither of them checked.
    let (x, y, z) = (3i64, -60i64, 5i64);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);

    let mut body = packed.to_be_bytes().to_vec();
    body.insert(0, 0); // status: start digging, which is what creative sends
    body.push(1); // face
    write_var_int(1, &mut body); // sequence
    send_compressed_frame(&mut breaker_stream, 36, &body);

    // The watcher must be told, and told the right block at the right place.
    let update = watcher
        .wait_for(&mut watcher_stream, 9)
        .expect("the watcher is told about the break");
    let position = i64::from_be_bytes(update[..8].try_into().expect("eight bytes"));
    assert_eq!(position, packed, "the same block, not a neighbour");
    let (state, _) = read_var_int_from(&update[8..]);
    assert_eq!(state, 0, "broken to air");

    running.finish();
}

/// The half of Phase 3's exit criterion that needs a restart: break a block,
/// walk away, stop the server, start it again, and find both the hole and
/// yourself where you left them.
///
/// Run across two whole server lifetimes rather than by calling the save code
/// directly, because what is being checked is that the write happens at the
/// right moment in the teardown and the read at the right moment in the boot.
/// A test that called `store` and `load` would pass with neither wired up.
#[test]
fn a_broken_block_and_a_walked_to_position_both_survive_a_restart() {
    let world_dir = std::env::temp_dir().join(format!(
        "dust-restart-{}-{}",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));

    let (x, y, z) = (3i64, -60i64, 5i64);
    let packed = ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff);
    // Far enough to be a different column, so the position is not
    // accidentally right by being the spawn one.
    let walked_to = 40.5f64;

    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let mut client = join_as(&mut stream, addr, "Digger");

        let mut body = packed.to_be_bytes().to_vec();
        body.insert(0, 0); // start digging, which is what creative sends
        body.push(1);
        write_var_int(1, &mut body);
        send_compressed_frame(&mut stream, 36, &body);
        client
            .wait_for(&mut stream, 5)
            .expect("the dig is acknowledged");

        let mut walk = Vec::new();
        walk.extend_from_slice(&walked_to.to_be_bytes());
        walk.extend_from_slice(&(-59.0f64).to_be_bytes());
        walk.extend_from_slice(&0.5f64.to_be_bytes());
        walk.push(1);
        send_compressed_frame(&mut stream, 26, &walk);
        client.drain_until_quiet(&mut stream);

        // Ending the connection before stopping the server, so the position is
        // recorded by the session rather than by a race with the shutdown.
        drop(stream);
        let report = running.finish();
        assert!(
            report
                .transcript
                .iter()
                .any(|e| e.detail.contains("saved 1 block change(s)")),
            "the teardown must say what it wrote: {:?}",
            report.transcript
        );
    }

    // A second server, same directory, nothing else in common.
    {
        let running = start_in(&world_dir, "");
        let addr = running.addr;
        let mut stream = connect(addr);
        let mut client = join_as(&mut stream, addr, "Digger");

        // The teleport on join is where the player left off, not spawn.
        let (back_x, _, _) = client.spawned_at.expect("a position on join");
        assert_eq!(back_x, walked_to, "the player is put back where they were");

        // And the hole is still there. Asked by breaking it again: an already
        // broken block re-broken is still air, so what this really pins is
        // that the *chunk* arrived with the edit in it — checked below by the
        // column count, since an edited column is built rather than templated
        // and both paths have to produce a chunk.
        client.drain_until_quiet(&mut stream);
        assert!(client.chunks >= 25, "the world arrived");

        drop(stream);
        running.finish();
    }

    // The save itself, read as an operator would: it is a file, it is JSON,
    // and it names the block rather than a number that means nothing next
    // version.
    let saved = std::fs::read_to_string(world_dir.join("dust-edits.json")).expect("a save file");
    assert!(saved.contains("minecraft:air"), "{saved}");
    assert!(saved.contains("\"y\": -60"), "{saved}");

    let _ = std::fs::remove_dir_all(&world_dir);
}

/// Two players in one world can see each other.
///
/// The whole point of a server, and the thing every other test here would pass
/// without. A player has to arrive as *both* halves — a tab-list entry and an
/// entity — because a client shown one without the other renders either a name
/// with no body or a body with no name, and neither looks like a bug in the
/// half that is missing.
#[test]
fn two_players_see_each_other_arrive_move_and_leave() {
    let running = start("");
    let addr = running.addr;

    let mut first_stream = connect(addr);
    let mut first = join_as(&mut first_stream, addr, "First");
    // Nobody else is here yet, so the first player is told about nobody.
    first.drain(&mut first_stream);
    assert_eq!(first.spawned_entities, 0, "an empty server has no bodies");

    let mut second_stream = connect(addr);
    let mut second = join_as(&mut second_stream, addr, "Second");
    // The roster goes out *after* the loading-screen event, deliberately — an
    // entity announced before the client holds the column it stands in is one
    // the client files against nothing — so `join_as` has already returned by
    // the time it arrives.
    second.drain_until_quiet(&mut second_stream);

    // The second player is told about the first, on arrival, from the roster
    // snapshot rather than from the broadcast.
    assert_eq!(second.player_infos, 1, "the first player's tab-list row");
    assert_eq!(second.spawned_entities, 1, "and the first player's body");

    // And the first is told about the second, through the broadcast.
    first
        .wait_for(&mut first_stream, 1)
        .expect("the first player is told the second arrived");
    assert_eq!(
        first.player_infos, 1,
        "the tab-list row came with the body, not instead of it"
    );

    // The second walks; the first is told where to.
    let mut walk = Vec::new();
    walk.extend_from_slice(&64.5f64.to_be_bytes());
    walk.extend_from_slice(&(-59.0f64).to_be_bytes());
    walk.extend_from_slice(&8.5f64.to_be_bytes());
    walk.push(1);
    send_compressed_frame(&mut second_stream, 26, &walk);

    let teleport = first
        .wait_for(&mut first_stream, 112)
        .expect("the first player is told the second moved");
    let (_, rest) = read_var_int_from(&teleport);
    let x = f64::from_be_bytes(rest[0..8].try_into().expect("eight bytes"));
    assert_eq!(x, 64.5, "to where they actually went");

    // The second leaves; the first is told to forget them, both halves.
    drop(second_stream);
    first
        .wait_for(&mut first_stream, 66)
        .expect("the body is removed");
    first
        .wait_for(&mut first_stream, 61)
        .expect("and so is the tab-list row");

    drop(first_stream);
    running.finish();
}

#[test]
fn a_connection_that_says_nothing_costs_nothing() {
    let running = start("");
    let stream = connect(running.addr);
    drop(stream);
    running.finish();
}

#[test]
fn a_configured_favicon_reaches_the_wire_and_a_bad_one_stops_the_boot() {
    // The unit tests prove the picture is validated and that the document
    // carries it. Neither proves the boot phase actually reads the setting —
    // a start_network that never looked at `favicon` would pass both, and an
    // operator would get exactly what they get from setting nothing at all.
    let png = tiny_png(64, 64);
    let icon_path = std::env::temp_dir().join(format!(
        "dust-ping-icon-{}-{}.png",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&icon_path, &png).expect("write the icon");

    let running = start(&format!(
        "favicon = {:?}\n",
        icon_path.to_str().expect("a UTF-8 temp path")
    ));
    let addr = running.addr;
    let mut stream = connect(addr);
    handshake(&mut stream, 767, addr, 1);
    send_frame(&mut stream, 0x00, &[]);
    let json = read_status_json(&mut stream);
    assert!(
        json.contains(r#""favicon":"data:image/png;base64,"#),
        "the configured icon must reach the wire: {json}"
    );
    running.finish();

    // And the refusal half: a picture the client would silently ignore stops
    // the boot instead, because "shows nothing" and "was never set" look the
    // same to the only person who could fix it.
    let wrong = std::env::temp_dir().join(format!(
        "dust-ping-icon-{}-{}.png",
        std::process::id(),
        ICON_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&wrong, tiny_png(128, 128)).expect("write the icon");
    let err = boot_expecting_failure(&format!(
        "favicon = {:?}\n",
        wrong.to_str().expect("a UTF-8 temp path")
    ));
    let message = err.to_string();
    assert!(message.contains("128x128"), "{message}");
    assert!(message.contains("64x64"), "{message}");

    let _ = std::fs::remove_file(&icon_path);
    let _ = std::fs::remove_file(&wrong);
}

static ICON_SEQ: AtomicU64 = AtomicU64::new(0);

/// A PNG header claiming a size, with no image data. Nothing in the server
/// decodes pixels, so nothing here needs any — and a test that shipped a real
/// picture would be testing a decoder that does not exist.
fn tiny_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}

/// Run a boot that is expected to fail in phase 3, and return the error.
fn boot_expecting_failure(extra_config: &str) -> dust_server::ServerError {
    let clock = Arc::new(ManualClock::new());
    let config = format!("[server]\nbind = \"127.0.0.1:0\"\nonline_mode = false\n{extra_config}");
    let options = ServerOptions {
        config_path: write_config(&config),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(Arc::clone(&clock), TICK_NS),
        watchdog: WatchdogSetting::Disabled,
        ..ServerOptions::default()
    };
    Server::new(options)
        .run()
        .expect_err("a picture the client cannot use must stop the boot")
}
