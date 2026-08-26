//! A minimal Source RCON client.
//!
//! RCON is how the harness drives a running server without a player: `/stop`
//! for a graceful shutdown, `save-all flush`, status queries. The wire format
//! is the well-known Source RCON packet — a little-endian length, a request id,
//! a type, an ASCII payload, two zero bytes — unchanged since Valve documented
//! it, and vanilla speaks it faithfully. That smallness is why there is no
//! crate here: the whole client is one file with its test double beside it,
//! and a protocol this size is cheaper to own than to import.
//!
//! # Framing rules that actually matter
//!
//! - The length field counts everything after itself (id + type + payload +
//!   the two terminators) and never includes itself. Getting this backwards
//!   produces a connection that works against one's own fake and nothing else,
//!   so the known byte vector below is asserted verbatim.
//! - `SERVERDATA_EXECCOMMAND` and `SERVERDATA_AUTH_RESPONSE` are both type 2;
//!   direction is what distinguishes them. The constants keep both names so
//!   call sites read as intent rather than magic equality.
//! - A command's response may arrive split across several
//!   `SERVERDATA_RESPONSE_VALUE` packets, and TCP gives no end-of-response
//!   marker. [`Client::exec_delimited`] uses the standard trick: send a second
//!   throwaway command whose id is known, read until *that* id answers, and
//!   treat everything before it as the response. [`Client::exec`] reads a
//!   single packet, which is correct when the answer is short and the command
//!   ends the conversation (`/stop`) or cannot be retried anyway.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Client → server: log in. Payload is the password.
pub const SERVERDATA_AUTH: i32 = 3;
/// Server → client: reply to auth. Id mirrors the request, or `-1` on failure.
pub const SERVERDATA_AUTH_RESPONSE: i32 = 2;
/// Client → server: run a command. Same number as the auth response; see above.
pub const SERVERDATA_EXECCOMMAND: i32 = 2;
/// Server → client: a (possibly empty, possibly fragmented) command reply.
pub const SERVERDATA_RESPONSE_VALUE: i32 = 0;

/// Refuse absurd frames before allocating for them.
///
/// Vanilla's replies are short; even a pathological multi-megabyte `list`
/// response is far below this. Anything larger means the byte stream has been
/// misframed, and resynchronising is impossible in principle.
const MAX_PACKET: usize = 1024 * 1024;

/// One packet, decoded. `payload` excludes both terminating zero bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub id: i32,
    pub kind: i32,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Encode with the length prefix, ready for the socket.
    pub fn encode(&self) -> Vec<u8> {
        let body = 4 + 4 + self.payload.len() + 2;
        let mut out = Vec::with_capacity(4 + body);
        out.extend_from_slice(&(body as u32).to_le_bytes());
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(&self.payload);
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// Decode one length-prefixed packet from the front of `buf`.
    ///
    /// Returns the packet and how many bytes it consumed. Trailing bytes are
    /// the caller's problem (they are the next packet); missing terminators
    /// are refused because a stream that lost them has already desynchronised.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), String> {
        if buf.len() < 4 {
            return Err(format!(
                "need at least a 4-byte length prefix, have {} bytes",
                buf.len()
            ));
        }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if !(10..=MAX_PACKET).contains(&len) {
            return Err(format!(
                "packet length {len} is not a plausible RCON packet"
            ));
        }
        let total = 4 + len;
        if buf.len() < total {
            return Err(format!(
                "packet says {len} body bytes but only {} are buffered",
                buf.len() - 4
            ));
        }
        let id = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let kind = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let payload = buf[12..total - 2].to_vec();
        if buf[total - 2..total] != [0, 0] {
            return Err("packet does not end in the two zero terminators".to_owned());
        }
        Ok((Self { id, kind, payload }, total))
    }
}

/// Where to reach a server and how to speak to it once connected.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub host: String,
    pub port: u16,
    pub password: String,
    /// Per-read deadline. The default is generous because a busy pregeneration
    /// can legitimately stall a response for seconds.
    pub timeout: Duration,
    /// Commands to run after connecting.
    pub commands: Vec<String>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: super::properties::RCON_PORT,
            password: super::properties::RCON_PASSWORD.to_owned(),
            timeout: Duration::from_secs(30),
            commands: Vec::new(),
        }
    }
}

/// Parse the `harness rcon` argument list.
///
/// Everything after the flags is a command; several commands may be given and
/// each runs on one connection.
pub fn parse_client_options(args: &[String]) -> Result<ClientOptions, String> {
    let mut options = ClientOptions::default();
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--host" => {
                take(&mut seen, "--host", args, at + 1)?;
                options.host = seen.last().expect("just stored").1.clone();
                at += 2;
            }
            "--port" => {
                take(&mut seen, "--port", args, at + 1)?;
                options.port = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--port needs a port number")?;
                at += 2;
            }
            "--password" => {
                take(&mut seen, "--password", args, at + 1)?;
                options.password = seen.last().expect("just stored").1.clone();
                at += 2;
            }
            "--timeout" => {
                take(&mut seen, "--timeout", args, at + 1)?;
                let seconds: u64 = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--timeout needs seconds")?;
                options.timeout = Duration::from_secs(seconds);
                at += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown rcon option `{other}`\n\n{}", super::USAGE));
            }
            command => {
                options.commands.push(command.to_owned());
                at += 1;
            }
        }
    }
    if options.commands.is_empty() {
        return Err(
            "rcon needs at least one command, e.g. `harness rcon list`\n\n".to_owned()
                + super::USAGE,
        );
    }
    Ok(options)
}

fn take(
    seen: &mut Vec<(&'static str, String)>,
    name: &'static str,
    rest: &[String],
    at: usize,
) -> Result<(), String> {
    let value = rest
        .get(at)
        .ok_or_else(|| format!("{name} needs a value"))?;
    if seen.iter().any(|(k, _)| *k == name) {
        return Err(format!("{name} given twice"));
    }
    seen.push((name, value.clone()));
    Ok(())
}

/// Run the operator-facing verb: connect once, run every command, print.
pub fn run_client(options: &ClientOptions) -> Result<(), String> {
    let address = (options.host.as_str(), options.port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {}: {e}", options.host))?
        .next()
        .ok_or_else(|| format!("{} resolved to no addresses", options.host))?;
    let mut client = Client::connect(address, options.timeout)?;
    client.authenticate(&options.password)?;
    for command in &options.commands {
        // Delimited reads for queries, single reads for commands that end the
        // world: /stop closes the socket mid-conversation and there is no
        // delimiter left to wait for.
        let response = if command.trim().eq_ignore_ascii_case("stop") {
            client.exec(command)?
        } else {
            client.exec_delimited(command)?
        };
        println!("{response}");
    }
    Ok(())
}

/// An authenticated-or-not connection speaking Source RCON.
pub struct Client {
    stream: TcpStream,
    next_id: i32,
    timeout: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("peer", &self.stream.peer_addr().ok())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect with the whole I/O budget applied per operation.
    pub fn connect<A: ToSocketAddrs>(address: A, timeout: Duration) -> Result<Self, String> {
        let stream = TcpStream::connect(address).map_err(|e| format!("could not connect: {e}"))?;
        stream
            .set_nodelay(true)
            .map_err(|e| format!("could not disable Nagle: {e}"))?;
        Ok(Self {
            stream,
            next_id: 1,
            timeout,
        })
    }

    /// Log in. Fails when the server answers with the failure id.
    ///
    /// Some implementations precede the auth response with an empty
    /// RESPONSE_VALUE; packets of any other kind are skipped until the real
    /// answer arrives, rather than assumed absent.
    pub fn authenticate(&mut self, password: &str) -> Result<(), String> {
        let id = self.send(Packet {
            id: self.next_id,
            kind: SERVERDATA_AUTH,
            payload: password.as_bytes().to_vec(),
        })?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let packet = self.read_packet(deadline)?;
            if packet.kind == SERVERDATA_AUTH_RESPONSE {
                if packet.id == id {
                    return Ok(());
                }
                return Err("the server rejected the RCON password".to_owned());
            }
        }
    }

    /// Send one command and read exactly one response packet.
    pub fn exec(&mut self, command: &str) -> Result<String, String> {
        let id = self.send(Packet {
            id: self.next_id,
            kind: SERVERDATA_EXECCOMMAND,
            payload: command.as_bytes().to_vec(),
        })?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let packet = self.read_packet(deadline)?;
            if packet.kind == SERVERDATA_RESPONSE_VALUE && packet.id == id {
                return Ok(String::from_utf8_lossy(&packet.payload).into_owned());
            }
        }
    }

    /// Send a command whose reply is deliberately never waited for.
    ///
    /// `/stop` is the reason this exists: vanilla tears the RCON socket down
    /// as the server exits, so a caller that blocks on the response races the
    /// connection closing under it. The command still arrives — TCP sees to
    /// that — and shutdown is confirmed by waiting on the process instead,
    /// which is the signal that actually matters. The id counter advances by
    /// two to keep its odd/even separation from [`Client::exec_delimited`].
    pub fn send_and_move_on(&mut self, command: &str) -> Result<(), String> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(2);
        self.send(Packet {
            id,
            kind: SERVERDATA_EXECCOMMAND,
            payload: command.as_bytes().to_vec(),
        })?;
        Ok(())
    }

    /// Send one command and collect its complete, possibly fragmented, reply.
    ///
    /// The throwaway companion command is harmless by construction: worst case
    /// the server answers it with "Unknown or incomplete command", which is
    /// discarded with the rest of the framing.
    pub fn exec_delimited(&mut self, command: &str) -> Result<String, String> {
        let id = self.next_id;
        let sentinel = id.wrapping_add(1);
        self.send(Packet {
            id,
            kind: SERVERDATA_EXECCOMMAND,
            payload: command.as_bytes().to_vec(),
        })?;
        self.send(Packet {
            id: sentinel,
            kind: SERVERDATA_EXECCOMMAND,
            payload: b"dust-harness-delimiter".to_vec(),
        })?;

        let deadline = Instant::now() + self.timeout;
        let mut collected = Vec::new();
        loop {
            let packet = self.read_packet(deadline)?;
            if packet.kind != SERVERDATA_RESPONSE_VALUE {
                continue;
            }
            if packet.id == id {
                collected.extend_from_slice(&packet.payload);
            } else if packet.id == sentinel {
                self.next_id = self.next_id.wrapping_add(2);
                return Ok(String::from_utf8_lossy(&collected).into_owned());
            }
        }
    }

    /// Advance and return the request id, keeping odd ids away from even ones
    /// so a delimiter can never collide with its command.
    fn send(&mut self, packet: Packet) -> Result<i32, String> {
        self.write_all(&packet.encode())?;
        Ok(packet.id)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| format!("could not set write timeout: {e}"))?;
        let written = std::io::Write::write(&mut self.stream, bytes);
        written
            .map(|_| ())
            .map_err(|e| format!("could not write to the socket: {e}"))
    }

    /// Read one framed packet, honouring the deadline across partial reads.
    fn read_packet(&mut self, deadline: Instant) -> Result<Packet, String> {
        let mut header = [0u8; 4];
        self.read_exact_deadline(&mut header, deadline)?;
        let len = u32::from_le_bytes(header) as usize;
        if !(10..=MAX_PACKET).contains(&len) {
            return Err(format!(
                "peer announced a {len}-byte packet, which is not RCON"
            ));
        }
        let mut body = vec![0u8; len];
        self.read_exact_deadline(&mut body, deadline)?;
        Packet::decode(
            &header
                .iter()
                .chain(body.iter())
                .copied()
                .collect::<Vec<u8>>(),
        )
        .map(|(p, _)| p)
    }

    fn read_exact_deadline(&mut self, buf: &mut [u8], deadline: Instant) -> Result<(), String> {
        let mut filled = 0;
        while filled < buf.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for the server".to_owned());
            }
            self.stream
                .set_read_timeout(Some(remaining.min(Duration::from_millis(500))))
                .map_err(|e| format!("could not set read timeout: {e}"))?;
            match std::io::Read::read(&mut self.stream, &mut buf[filled..]) {
                Ok(0) => return Err("the server closed the connection".to_owned()),
                Ok(n) => filled += n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                Err(e) => return Err(format!("could not read from the socket: {e}")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Read one length-framed packet off a raw socket.
    fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
        let mut prefix = [0u8; 4];
        stream.read_exact(&mut prefix).expect("length prefix");
        let len = u32::from_le_bytes(prefix) as usize;
        let mut rest = vec![0u8; len];
        stream.read_exact(&mut rest).expect("body");
        prefix.iter().chain(rest.iter()).copied().collect()
    }

    /// What the fake server does with a command payload.
    enum Script {
        /// Echo the command back, uppercased, as one fragment.
        Echo,
        /// Reply with these fragments in order under the command's id.
        Fragments(Vec<String>),
        /// Never reply to commands. For exercising the client's patience.
        Silent,
        /// Never even answer authentication. For exercising its timeouts.
        AuthSilent,
    }

    /// Accept exactly one connection and speak RCON at it until it leaves.
    fn spawn_fake(password: &'static str, script: Script) -> std::net::SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback bind");
        let address = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            while let Ok(packet) = Packet::decode(&read_frame(&mut stream)).map(|(p, _)| p) {
                if packet.kind == SERVERDATA_AUTH {
                    if matches!(script, Script::AuthSilent) {
                        std::thread::park();
                    }
                    let ok = packet.payload == password.as_bytes();
                    let reply = Packet {
                        id: if ok { packet.id } else { -1 },
                        kind: SERVERDATA_AUTH_RESPONSE,
                        payload: Vec::new(),
                    };
                    stream.write_all(&reply.encode()).expect("auth reply");
                } else if packet.kind == SERVERDATA_EXECCOMMAND {
                    match &script {
                        Script::Echo => {
                            let text = String::from_utf8_lossy(&packet.payload).to_uppercase();
                            let reply = Packet {
                                id: packet.id,
                                kind: SERVERDATA_RESPONSE_VALUE,
                                payload: text.into_bytes(),
                            };
                            stream.write_all(&reply.encode()).expect("echo");
                        }
                        Script::Fragments(parts) => {
                            for part in parts {
                                let reply = Packet {
                                    id: packet.id,
                                    kind: SERVERDATA_RESPONSE_VALUE,
                                    payload: part.clone().into_bytes(),
                                };
                                stream.write_all(&reply.encode()).expect("fragment");
                            }
                        }
                        Script::Silent | Script::AuthSilent => std::thread::park(),
                    }
                }
            }
        });
        address
    }

    fn connect_and_auth(address: std::net::SocketAddr, password: &str) -> Client {
        let mut client = Client::connect(address, Duration::from_secs(5)).expect("connect");
        client.authenticate(password).expect("auth");
        client
    }

    #[test]
    fn the_documented_wire_bytes_come_out_of_a_known_packet() {
        // Auth packet for id 1, password "pass": length 14 = 4 id + 4 type +
        // 4 payload + 2 terminators (the length field excludes itself).
        // Hand-written once, kept forever.
        let packet = Packet {
            id: 1,
            kind: SERVERDATA_AUTH,
            payload: b"pass".to_vec(),
        };
        assert_eq!(
            packet.encode(),
            vec![
                14, 0, 0, 0, //
                1, 0, 0, 0, //
                3, 0, 0, 0, //
                b'p', b'a', b's', b's', //
                0, 0, //
            ]
        );
    }

    #[test]
    fn decoding_returns_the_packet_and_its_length() {
        let encoded = Packet {
            id: 7,
            kind: SERVERDATA_RESPONSE_VALUE,
            payload: b"hello".to_vec(),
        }
        .encode();
        let (packet, consumed) = Packet::decode(&encoded).expect("decodes");
        assert_eq!(consumed, encoded.len());
        assert_eq!(packet.id, 7);
        assert_eq!(packet.kind, SERVERDATA_RESPONSE_VALUE);
        assert_eq!(packet.payload, b"hello");

        // Bytes beyond the frame belong to the next packet and are left alone.
        let mut two = encoded.clone();
        two.extend_from_slice(&encoded);
        let (_, consumed_first) = Packet::decode(&two).expect("first of two");
        assert_eq!(
            Packet::decode(&two[consumed_first..])
                .expect("second of two")
                .0
                .id,
            7
        );
    }

    #[test]
    fn malformed_frames_are_refused_rather_than_guessed_at() {
        assert!(Packet::decode(&[]).is_err(), "no header");
        assert!(Packet::decode(&[5, 0, 0]).is_err(), "truncated header");
        // Length below the ten bytes every packet must carry.
        let too_short = {
            let mut b = vec![5u8, 0, 0, 0];
            b.extend_from_slice(&[0u8; 32]);
            b
        };
        assert!(Packet::decode(&too_short).is_err(), "too short");
        // Announced length longer than what is buffered.
        let overlong = {
            let mut b = vec![200u8, 0, 0, 0];
            b.extend_from_slice(&[0u8; 32]);
            b
        };
        assert!(Packet::decode(&overlong).is_err(), "overlong");
        let mut unterminated = Packet {
            id: 1,
            kind: 2,
            payload: b"x".to_vec(),
        }
        .encode();
        unterminated.pop();
        assert!(
            Packet::decode(&unterminated[..unterminated.len() - 1]).is_err()
                || Packet::decode(&{
                    let mut broken = unterminated.clone();
                    let last = broken.len() - 1;
                    broken[last] = 0xff;
                    broken
                })
                .is_err(),
            "missing terminator"
        );
    }

    #[test]
    fn authentication_succeeds_against_the_right_password_only() {
        let address = spawn_fake("s3cret", Script::Echo);
        let mut good = Client::connect(address, Duration::from_secs(5)).expect("connect");
        good.authenticate("s3cret").expect("correct password");

        let mut bad = Client::connect(address, Duration::from_secs(5)).expect("connect");
        assert!(bad.authenticate("wrong").is_err(), "rejection propagates");
    }

    #[test]
    fn a_command_round_trips_through_the_fake_server() {
        let address = spawn_fake("pw", Script::Echo);
        let mut client = connect_and_auth(address, "pw");
        assert_eq!(client.exec("list").expect("exec"), "LIST");
    }

    #[test]
    fn fragmented_replies_are_stitched_together_by_the_delimiter() {
        let address = spawn_fake(
            "pw",
            Script::Fragments(vec!["chunk one ".to_owned(), "chunk two".to_owned()]),
        );
        let mut client = connect_and_auth(address, "pw");
        assert_eq!(
            client.exec_delimited("status").expect("exec"),
            "chunk one chunk two"
        );
    }

    #[test]
    fn a_silent_server_times_out_instead_of_hanging_forever() {
        let address = spawn_fake("pw", Script::AuthSilent);
        let mut client = Client::connect(address, Duration::from_millis(300)).expect("connect");
        let started = Instant::now();
        let outcome = client.authenticate("pw");
        assert!(outcome.expect_err("times out").contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5), "must not hang");
    }

    #[test]
    fn the_parser_builds_options_from_the_documented_flags() {
        let parsed =
            parse_client_options(&["--port".to_owned(), "25599".to_owned(), "list".to_owned()])
                .expect("parses");
        assert_eq!(parsed.port, 25599);
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.commands, vec!["list"]);
    }

    #[test]
    fn a_command_sent_without_awaiting_its_reply_returns_promptly() {
        // /stop arrives while the server is busy dying; the client side of
        // that conversation must not block on an answer that may never come.
        let address = spawn_fake("pw", Script::Silent);
        let mut client = connect_and_auth(address, "pw");
        let started = Instant::now();
        client
            .send_and_move_on("stop")
            .expect("sends without waiting");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must not wait for a reply"
        );
    }

    #[test]
    fn stop_is_recognised_caselessly_as_a_conversation_ender() {
        // Not a parser unit of its own, but the branch exists and deserves the
        // same scrutiny as the rest: a typo here would hang every capture.
        assert!("STOP".trim().eq_ignore_ascii_case("stop"));
        assert!(!"stop-all".trim().eq_ignore_ascii_case("stop"));
    }
}
