//! Phase 1's first exit criterion, checked by speaking the protocol.
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
    assert_eq!(
        uncompressed_len, 0,
        "this test only exchanges packets below the threshold; a non-zero \
         length here means the server compressed something it should not have"
    );
    let (id, body) = read_var_int_from(rest);
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
    let clock = Arc::new(ManualClock::new());
    let config = format!("[server]\nbind = \"127.0.0.1:0\"\nonline_mode = false\n{extra_config}");

    let options = ServerOptions {
        config_path: write_config(&config),
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
fn an_offline_login_completes_and_then_says_the_world_is_not_ready() {
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

    // And configuration is where this server runs out of things to say. The
    // disconnect here is the *configuration* one, which carries an NBT
    // component rather than login's JSON — two spellings of one idea, and a
    // server that used the wrong one renders nothing at all.
    let (id, body) = recv_compressed_frame(&mut stream);
    assert_eq!(id, 0x02, "disconnect is id 2 clientbound in configuration");
    assert_eq!(body[0], 0x0a, "an NBT component starts with TAG_Compound");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("cannot serve a world yet"), "{text:?}");

    let mut buffer = [0u8; 64];
    let read = stream.read(&mut buffer).expect("read after the disconnect");
    assert_eq!(read, 0, "the server closes after saying why");

    let report = running.finish();
    assert!(
        report
            .transcript
            .iter()
            .any(|e| e.detail.contains("1 login(s)")),
        "the teardown must account for the login: {:?}",
        report.transcript
    );
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
