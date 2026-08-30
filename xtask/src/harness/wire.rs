//! A 1.21.1 client, written by hand, that shares no code with the server.
//!
//! # Why this exists rather than a call into `dust-net`
//!
//! Everything this module reads, it reads to compare against what a real
//! Minecraft server sent. A client built on the server's own framing would
//! agree with the server by construction — under any convention, including a
//! wrong one — and would say nothing at all about whether Dust and Minecraft
//! agree with each other. So the VarInts, the length prefixes, the compression
//! threshold and the network NBT are all written again here, deliberately, and
//! the whole file is the judge rather than a participant.
//!
//! That is the same reasoning `harness::nbt` and `harness::region` are written
//! under, and the opposite of `harness::rewrite`, which *has* to be Dust's own
//! code because what it tests is Dust's reader against Dust's writer.
//!
//! # What it does and does not implement
//!
//! Enough to reach the end of configuration: handshake, login in offline mode,
//! the known-packs answer, and then every configuration packet read until
//! `finish_configuration`. No encryption — the servers this points at run with
//! `online-mode=false`, and a handshake that negotiated a session key would be
//! testing Mojang's auth servers rather than either of these.
//!
//! It answers the known-packs request with an **empty list**, which is the
//! whole point: a client that acknowledges nothing is told the registries'
//! contents rather than their names, and those contents are what there is to
//! compare.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use super::nbt;

/// Clientbound configuration packet ids on 1.21.1, from the generated table
/// and confirmed against a running server.
mod configuration {
    pub const REGISTRY_DATA: i32 = 0x07;
    pub const UPDATE_TAGS: i32 = 0x0d;
    pub const SELECT_KNOWN_PACKS: i32 = 0x0e;
    pub const FINISH: i32 = 0x03;
    pub const DISCONNECT: i32 = 0x02;
    /// Serverbound: the client's answer to `SELECT_KNOWN_PACKS`.
    pub const ANSWER_KNOWN_PACKS: i32 = 0x07;
}

mod login {
    pub const DISCONNECT: i32 = 0x00;
    pub const ENCRYPTION_REQUEST: i32 = 0x01;
    pub const SUCCESS: i32 = 0x02;
    pub const SET_COMPRESSION: i32 = 0x03;
    /// Serverbound.
    pub const START: i32 = 0x00;
    /// Serverbound.
    pub const ACKNOWLEDGED: i32 = 0x03;
}

/// The 1.21.1 protocol number.
const PROTOCOL: i32 = 767;

/// What a server said during configuration.
#[derive(Debug, Default)]
pub struct Configuration {
    /// Every `registry_data` packet, in the order sent.
    pub registries: Vec<Registry>,
    /// The one `update_tags` packet, if the server sent one.
    pub tags: Option<Vec<TagRegistry>>,
}

/// One synced registry as it arrived.
#[derive(Debug)]
pub struct Registry {
    /// The registry's namespaced id.
    pub name: String,
    /// Its entries, in the order sent — which is the order that assigns ids.
    pub entries: Vec<Entry>,
}

/// One entry of a synced registry.
#[derive(Debug)]
pub struct Entry {
    /// The entry's namespaced id.
    pub name: String,
    /// Its contents, or `None` where the server said the client already has
    /// them. The difference matters: `None` is "use your own copy" and an
    /// empty compound is "here is an empty definition".
    pub data: Option<nbt::Node>,
}

/// One registry's tags as they arrived.
#[derive(Debug)]
pub struct TagRegistry {
    /// The registry the tags group.
    pub name: String,
    /// Each tag's id and the registry ids in it, flattened by the server.
    pub tags: Vec<(String, Vec<i32>)>,
}

/// Connect, log in as `username`, and read to the end of configuration.
///
/// # Errors
///
/// Any transport failure, a disconnect from the server (with its reason), or a
/// packet whose body does not parse.
pub fn configuration_of(
    address: SocketAddr,
    username: &str,
    timeout: Duration,
) -> Result<Configuration, String> {
    let mut conn = Conn::connect(address, timeout)?;
    conn.handshake(address.port())?;
    conn.login(username)?;
    conn.read_configuration()
}

/// One connection, with the compression threshold it negotiated.
struct Conn {
    stream: TcpStream,
    /// `None` until the server sets one. Below the threshold a frame carries
    /// an uncompressed length of zero and its body raw, which is a rule with
    /// no way to discover it from a frame that got it wrong.
    threshold: Option<usize>,
}

impl Conn {
    fn connect(address: SocketAddr, timeout: Duration) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|e| format!("could not connect to {address}: {e}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|e| format!("could not set a timeout on {address}: {e}"))?;
        Ok(Self {
            stream,
            threshold: None,
        })
    }

    fn handshake(&mut self, port: u16) -> Result<(), String> {
        let mut body = Vec::new();
        write_var_int(&mut body, PROTOCOL);
        write_string(&mut body, "127.0.0.1");
        body.extend_from_slice(&port.to_be_bytes());
        // 2 = "next state is login". 1 would be the status ping.
        write_var_int(&mut body, 2);
        self.send(0x00, &body)
    }

    fn login(&mut self, username: &str) -> Result<(), String> {
        let mut body = Vec::new();
        write_string(&mut body, username);
        // Sixteen raw mandatory bytes since 1.20.5, not an optional behind a
        // presence flag — the shape a real client sends, and the one this
        // project's first mineflayer run found the server refusing.
        body.extend_from_slice(&[0x11; 16]);
        self.send(login::START, &body)?;

        loop {
            let (id, body) = self.recv()?;
            match id {
                login::SET_COMPRESSION => {
                    let (threshold, _) = read_var_int(&body)?;
                    self.threshold = usize::try_from(threshold).ok();
                }
                login::SUCCESS => return self.send(login::ACKNOWLEDGED, &[]),
                login::DISCONNECT => {
                    return Err(format!("login refused: {}", String::from_utf8_lossy(&body)))
                }
                login::ENCRYPTION_REQUEST => {
                    return Err(
                        "the server asked for encryption; point this at one running \
                         offline mode"
                            .to_owned(),
                    )
                }
                other => return Err(format!("unexpected login packet {other:#04x}")),
            }
        }
    }

    fn read_configuration(&mut self) -> Result<Configuration, String> {
        let mut out = Configuration::default();
        loop {
            let (id, body) = self.recv()?;
            match id {
                configuration::SELECT_KNOWN_PACKS => {
                    // An empty list: "I have no data packs." A server with
                    // contents to send then sends them.
                    let mut answer = Vec::new();
                    write_var_int(&mut answer, 0);
                    self.send(configuration::ANSWER_KNOWN_PACKS, &answer)?;
                }
                configuration::REGISTRY_DATA => out.registries.push(read_registry(&body)?),
                configuration::UPDATE_TAGS => out.tags = Some(read_tags(&body)?),
                configuration::FINISH => return Ok(out),
                configuration::DISCONNECT => {
                    return Err(format!(
                        "disconnected during configuration: {}",
                        String::from_utf8_lossy(&body)
                    ))
                }
                // Brand, feature flags, and anything else a server volunteers.
                // Read and dropped so the stream stays in step.
                _ => {}
            }
        }
    }

    fn send(&mut self, id: i32, body: &[u8]) -> Result<(), String> {
        let mut payload = Vec::with_capacity(body.len() + 5);
        write_var_int(&mut payload, id);
        payload.extend_from_slice(body);

        let frame = match self.threshold {
            None => payload,
            Some(threshold) if payload.len() >= threshold => {
                let mut inner = Vec::new();
                write_var_int(&mut inner, payload.len() as i32);
                let mut encoder =
                    flate2::write::ZlibEncoder::new(inner, flate2::Compression::default());
                encoder
                    .write_all(&payload)
                    .map_err(|e| format!("could not compress a frame: {e}"))?;
                encoder
                    .finish()
                    .map_err(|e| format!("could not finish a frame: {e}"))?
            }
            Some(_) => {
                let mut inner = Vec::with_capacity(payload.len() + 1);
                write_var_int(&mut inner, 0);
                inner.extend_from_slice(&payload);
                inner
            }
        };

        let mut out = Vec::with_capacity(frame.len() + 5);
        write_var_int(&mut out, frame.len() as i32);
        out.extend_from_slice(&frame);
        self.stream
            .write_all(&out)
            .map_err(|e| format!("could not write a frame: {e}"))
    }

    fn recv(&mut self) -> Result<(i32, Vec<u8>), String> {
        let length = self.read_var_int_from_stream()?;
        let length =
            usize::try_from(length).map_err(|_| format!("a frame declared length {length}"))?;
        let mut frame = vec![0u8; length];
        self.stream
            .read_exact(&mut frame)
            .map_err(|e| format!("could not read a frame of {length} bytes: {e}"))?;

        let frame = match self.threshold {
            None => frame,
            Some(_) => {
                let (uncompressed, rest) = read_var_int(&frame)?;
                if uncompressed == 0 {
                    rest.to_vec()
                } else {
                    let mut out = Vec::with_capacity(uncompressed as usize);
                    flate2::read::ZlibDecoder::new(rest)
                        .read_to_end(&mut out)
                        .map_err(|e| format!("could not decompress a frame: {e}"))?;
                    if out.len() != uncompressed as usize {
                        return Err(format!(
                            "a frame said it was {uncompressed} bytes and was {}",
                            out.len()
                        ));
                    }
                    out
                }
            }
        };
        let (id, body) = read_var_int(&frame)?;
        Ok((id, body.to_vec()))
    }

    /// A VarInt read a byte at a time off the socket, because the length
    /// prefix is what says how much to read next.
    fn read_var_int_from_stream(&mut self) -> Result<i32, String> {
        let mut value: u32 = 0;
        for shift in 0..5 {
            let mut byte = [0u8; 1];
            self.stream
                .read_exact(&mut byte)
                .map_err(|e| format!("could not read a length prefix: {e}"))?;
            value |= u32::from(byte[0] & 0x7F) << (shift * 7);
            if byte[0] & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err("a length prefix ran past five bytes".to_owned())
    }
}

fn read_registry(body: &[u8]) -> Result<Registry, String> {
    let (name, rest) = read_string(body)?;
    let (count, mut rest) = read_var_int(rest)?;
    let mut entries = Vec::with_capacity(count.max(0) as usize);
    for _ in 0..count {
        let (entry, after) = read_string(rest)?;
        let (&has_data, after) = after
            .split_first()
            .ok_or_else(|| format!("{name} ended mid-entry"))?;
        let (data, after) = if has_data == 0 {
            (None, after)
        } else {
            let (node, used) = nbt::read_network(after)?;
            (Some(node), &after[used..])
        };
        entries.push(Entry { name: entry, data });
        rest = after;
    }
    if !rest.is_empty() {
        return Err(format!("{name} had {} bytes left over", rest.len()));
    }
    Ok(Registry { name, entries })
}

fn read_tags(body: &[u8]) -> Result<Vec<TagRegistry>, String> {
    let (count, mut rest) = read_var_int(body)?;
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for _ in 0..count {
        let (name, after) = read_string(rest)?;
        let (tag_count, after) = read_var_int(after)?;
        rest = after;
        let mut tags = Vec::with_capacity(tag_count.max(0) as usize);
        for _ in 0..tag_count {
            let (tag, after) = read_string(rest)?;
            let (entry_count, mut after) = read_var_int(after)?;
            let mut ids = Vec::with_capacity(entry_count.max(0) as usize);
            for _ in 0..entry_count {
                let (id, next) = read_var_int(after)?;
                ids.push(id);
                after = next;
            }
            tags.push((tag, ids));
            rest = after;
        }
        out.push(TagRegistry { name, tags });
    }
    if !rest.is_empty() {
        return Err(format!("update_tags had {} bytes left over", rest.len()));
    }
    Ok(out)
}

fn write_var_int(out: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_var_int(out, value.len() as i32);
    out.extend_from_slice(value.as_bytes());
}

fn read_var_int(input: &[u8]) -> Result<(i32, &[u8]), String> {
    let mut value: u32 = 0;
    for shift in 0..5 {
        let byte = *input
            .get(shift)
            .ok_or_else(|| "a VarInt ran off the end of a packet".to_owned())?;
        value |= u32::from(byte & 0x7F) << (shift * 7);
        if byte & 0x80 == 0 {
            return Ok((value as i32, &input[shift + 1..]));
        }
    }
    Err("a VarInt ran past five bytes".to_owned())
}

fn read_string(input: &[u8]) -> Result<(String, &[u8]), String> {
    let (length, rest) = read_var_int(input)?;
    let length =
        usize::try_from(length).map_err(|_| format!("a string declared length {length}"))?;
    if rest.len() < length {
        return Err(format!(
            "a string said {length} bytes and {} remain",
            rest.len()
        ));
    }
    Ok((
        String::from_utf8_lossy(&rest[..length]).into_owned(),
        &rest[length..],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_ints_round_trip_through_this_module_s_own_pair() {
        // Not a claim that these match Minecraft — that is what pointing the
        // client at a real server checks. This is the weaker, still worth
        // having claim that the two halves here agree, so a comparison that
        // fails is a difference between servers and not between these two
        // functions.
        for value in [0, 1, 127, 128, 255, 2_097_151, i32::MAX, -1, i32::MIN] {
            let mut out = Vec::new();
            write_var_int(&mut out, value);
            let (read, rest) = read_var_int(&out).expect("reads");
            assert_eq!(read, value);
            assert!(rest.is_empty(), "{value} left {} bytes", rest.len());
        }
    }

    #[test]
    fn the_five_byte_cap_is_a_refusal_and_not_a_wrap() {
        let runaway = [0x80u8; 6];
        assert!(read_var_int(&runaway).is_err());
    }

    #[test]
    fn a_string_longer_than_the_packet_is_refused() {
        let mut out = Vec::new();
        write_var_int(&mut out, 100);
        out.extend_from_slice(b"short");
        assert!(read_string(&out).is_err());
    }

    #[test]
    fn strings_round_trip_and_leave_the_rest_alone() {
        let mut out = Vec::new();
        write_string(&mut out, "minecraft:worldgen/biome");
        out.extend_from_slice(&[9, 9, 9]);
        let (read, rest) = read_string(&out).expect("reads");
        assert_eq!(read, "minecraft:worldgen/biome");
        assert_eq!(rest, &[9, 9, 9]);
    }
}
