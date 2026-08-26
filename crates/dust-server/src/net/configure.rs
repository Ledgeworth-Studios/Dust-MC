//! The configuration state: everything a client is told before it can be in a
//! world.
//!
//! # The exchange, as a real 1.21.1 server performs it
//!
//! Captured off the wire rather than read off a wiki, because the order and the
//! contents are both load-bearing and neither is documented anywhere that can
//! be checked:
//!
//! ```text
//! S->C  custom_payload            minecraft:brand = "vanilla"
//! S->C  update_enabled_features   ["minecraft:vanilla"]
//! S->C  select_known_packs        [minecraft:core 1.21.1]
//! C->S  select_known_packs        the subset the client also has
//! S->C  registry_data  x11        names only, no payloads
//! S->C  update_tags               13 registries
//! S->C  finish_configuration
//! C->S  finish_configuration
//! ```
//!
//! Dust sends the same sequence, with one omission stated below.
//!
//! # Known packs are the whole reason this is small
//!
//! Every registry entry in that capture carried **no data**. The payload is
//! optional per entry, and a server omits it for any entry the client already
//! has — which the client declares by echoing back the known packs it
//! recognises. A vanilla client recognises `minecraft:core`, so a server whose
//! content is vanilla's sends three hundred names and nothing else.
//!
//! That is not an optimisation Dust is taking advantage of; it is the only
//! thing Dust can currently do. It has the names and not the contents, on
//! purpose — see `dust_registry::synced`. So a client that does *not*
//! acknowledge the core pack is told so and disconnected, rather than being
//! sent a registry with no payloads it has no copy of, which would put it in a
//! world with no dimension types and no way to know why.
//!
//! # What is deliberately not sent yet, and what it costs
//!
//! **Tags.** Vanilla sends twenty-five kilobytes of them across thirteen
//! registries. Dust has five of those registries extracted and sends none,
//! because a partial tag set is worse than none: a client told that
//! `minecraft:mineable/pickaxe` contains eleven blocks believes the other
//! nine hundred are not mineable, whereas a client told nothing falls back to
//! its own copy. Sending them is a Phase 4 job, and it is the first thing to
//! do when block behaviour starts depending on tags.

use dust_net::io::{Conn, ConnError};
use dust_protocol::packets::common::{KnownPack, RegistryEntry};
use dust_protocol::packets::configuration;
use dust_protocol::types::{Identifier, ProtocolString, RestOfPacket};
use dust_protocol::wire::{Reader, WireWrite as _, Writer};
use tokio::io::{AsyncRead, AsyncWrite};

use super::session::{SessionContext, SessionError};
use crate::to_frame;

/// The pack a vanilla client is expected to acknowledge.
///
/// Namespace, id and version, exactly as a 1.21.1 server sends them. The
/// version string is the Minecraft version and not a pack format number, which
/// is worth pinning because the two are adjacent concepts with different values.
pub const CORE_PACK: (&str, &str, &str) = ("minecraft", "core", "1.21.1");

/// What the server calls itself in `minecraft:brand`.
///
/// The client puts this in `/debug` output and in crash reports, and some
/// client mods branch on it. Saying "vanilla" would be a lie that makes those
/// reports useless; saying "Dust" is the honest answer and costs nothing,
/// because nothing in the vanilla client requires a particular value.
pub const BRAND: &str = "Dust";

/// How configuration ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Configured {
    /// The client acknowledged, the registries went out, and both ends are in
    /// Play.
    Ready,
    /// The client did not acknowledge the core pack, so its registries would
    /// have to carry their contents and Dust has none to send.
    UnknownContent,
}

/// Run the configuration exchange.
pub async fn configure<W>(
    conn: &mut Conn<W>,
    ctx: &SessionContext,
) -> Result<Configured, SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let version = ctx.version;

    // The brand. A plugin-message channel rather than a field, which is why it
    // is bytes with a length the client reads as a string.
    let mut brand = Writer::default();
    brand.write_var_int(BRAND.len() as i32);
    brand.write_slice(BRAND.as_bytes());
    send(
        conn,
        configuration::clientbound::CustomPayload {
            channel: identifier("minecraft:brand")?,
            // RestOfPacket, not a length-prefixed array: a plugin message's
            // payload is whatever remains in the frame, and the length the
            // client reads is the *string's* own, written above.
            data: RestOfPacket(brand.into_bytes()),
        },
        version,
    )
    .await?;

    // Feature flags. `minecraft:vanilla` is the one every server has on; the
    // list is what gates experimental content, and an empty list here would
    // turn off things a client expects to exist.
    send(
        conn,
        configuration::clientbound::UpdateEnabledFeatures {
            features: vec![identifier("minecraft:vanilla")?],
        },
        version,
    )
    .await?;

    // The pack negotiation.
    let (namespace, id, pack_version) = CORE_PACK;
    send(
        conn,
        configuration::clientbound::SelectKnownPacks {
            packs: vec![KnownPack {
                namespace: ProtocolString::new(namespace)?,
                id: ProtocolString::new(id)?,
                version: ProtocolString::new(pack_version)?,
            }],
        },
        version,
    )
    .await?;

    // The client's answer, plus whatever else it volunteers first. A vanilla
    // client sends `client_information` during configuration and may send it
    // before or after the pack answer, so this reads until the answer arrives
    // rather than assuming it is next.
    let acknowledged = loop {
        let Some(frame) = conn.next_frame().await? else {
            return Err(SessionError::Conn(ConnError::Closed));
        };
        let mut reader = Reader::new(&frame.body);
        match configuration::serverbound::Packet::decode_body(frame.id, &mut reader, version)? {
            configuration::serverbound::Packet::SelectKnownPacks(answer) => {
                break answer.packs.iter().any(|pack| {
                    pack.namespace.as_str() == namespace
                        && pack.id.as_str() == id
                        && pack.version.as_str() == pack_version
                });
            }
            // Language, view distance, chat settings, skin parts. Nothing here
            // acts on them yet; they are read so the frame is consumed and the
            // stream stays in step.
            configuration::serverbound::Packet::ClientInformation(_) => {}
            configuration::serverbound::Packet::CustomPayload(_) => {}
            other => {
                return Err(SessionError::OutOfTurn {
                    state: "configuration",
                    packet: other.name(),
                })
            }
        }
    };

    if !acknowledged {
        return Ok(Configured::UnknownContent);
    }

    // The registries. Names only: the client has the contents, which is what
    // acknowledging the pack said.
    for registry in dust_registry::synced::all() {
        let entries = registry
            .entries
            .iter()
            .map(|entry| {
                Ok(RegistryEntry {
                    entry_id: identifier(entry)?,
                    // `None`, not an empty compound. The difference is "use
                    // your own copy" against "here is an empty definition", and
                    // the second gives a client a world with no biomes.
                    data: None,
                })
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        send(
            conn,
            configuration::clientbound::RegistryData {
                registry_id: identifier(registry.name)?,
                entries,
            },
            version,
        )
        .await?;
    }

    send(
        conn,
        configuration::clientbound::FinishConfiguration {},
        version,
    )
    .await?;

    // The client's acknowledgement, which is what actually moves it to Play.
    // Anything else it sends first is consumed for the same reason as above.
    loop {
        let Some(frame) = conn.next_frame().await? else {
            return Err(SessionError::Conn(ConnError::Closed));
        };
        let mut reader = Reader::new(&frame.body);
        match configuration::serverbound::Packet::decode_body(frame.id, &mut reader, version)? {
            configuration::serverbound::Packet::FinishConfiguration(_) => break,
            configuration::serverbound::Packet::ClientInformation(_)
            | configuration::serverbound::Packet::CustomPayload(_)
            | configuration::serverbound::Packet::Pong(_)
            | configuration::serverbound::Packet::KeepAlive(_) => {}
            other => {
                return Err(SessionError::OutOfTurn {
                    state: "configuration",
                    packet: other.name(),
                })
            }
        }
    }

    Ok(Configured::Ready)
}

/// Parse a constant into an [`Identifier`], turning the impossible case into an
/// error rather than a panic.
///
/// Every caller here passes a literal or an entry from a generated table, so
/// this cannot fail — but "cannot fail" on a path an authenticated stranger
/// reaches is a claim worth not betting a panic on, and the generated table is
/// exactly the thing that could one day carry a name this refuses.
fn identifier(raw: &str) -> Result<Identifier, SessionError> {
    Identifier::parse(raw).map_err(SessionError::Protocol)
}

/// Encode one clientbound configuration packet and queue it.
async fn send<W, P>(
    conn: &mut Conn<W>,
    body: P,
    version: dust_protocol::ProtocolVersion,
) -> Result<(), SessionError>
where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    P: Into<configuration::clientbound::Packet>,
{
    let frame = to_frame!(body.into(), version);
    conn.send(frame).await?;
    Ok(())
}
