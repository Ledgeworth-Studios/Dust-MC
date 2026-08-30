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
//! # A client that acknowledges nothing
//!
//! Sending names alone to a client that has no copy of the contents was tried,
//! against `mineflayer` — a third-party client that does not track packs and
//! answers with an empty list. It fails inside its own registry loader,
//! reading `undefined` where a dimension type's contents should be, and never
//! reaches the world. So names alone are not an option for such a client, and
//! for a long time it was refused.
//!
//! It is now served, from the operator's own copy of Minecraft's data. Dust
//! ships no Mojang content: `[data] path` points at a directory in the
//! ordinary datapack layout, `crate::registries` reads the two registries a
//! client cannot manage without — `dimension_type` and `worldgen/biome` — and
//! those entries go out carrying their NBT while the other nine go out as
//! names. See decision record 0007 for why the line falls there.
//!
//! With no `[data] path` set, the behaviour is what it was: such a client is
//! disconnected, and the message names the setting that would admit it.
//!
//! **Why the other nine registries are still names to such a client, and why
//! that is not obviously wrong:** it was measured. `mineflayer` reaches the
//! world with those two and no others. The remaining nine describe things a
//! client can and does fall back on — chat types, damage types, paintings —
//! and the moment one of them turns out to be load-bearing for some client,
//! it gains a schema and joins the list. What decides that is a client
//! failing, not a guess made here.
//!
//! # Tags
//!
//! All thirteen registries, every tag, flattened to ids —  25 kilobytes, and
//! the same 6,362 memberships a real 1.21.1 server sends. It is all thirteen
//! or none: a client told that `minecraft:mineable/pickaxe` contains eleven
//! blocks believes the other nine hundred are not mineable, where a client
//! told nothing falls back to its own copy. That is why nothing went out while
//! five of the thirteen were extracted.
//!
//! Sent to *every* client, unlike the registry contents. Tags are not
//! covered by the known-packs exchange — vanilla sends them whether or not the
//! client acknowledged `minecraft:core`, and both captures show it.

use dust_net::io::{Conn, ConnError};
use dust_protocol::nbt::Nbt;
use dust_protocol::packets::common::{self, KnownPack, RegistryEntry};
use dust_protocol::packets::configuration;
use dust_protocol::types::{Identifier, ProtocolString, RestOfPacket, VarInt};
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
    Ready {
        /// How far the client asked to see, if it said. A *request*, and the
        /// server serves the smaller of this and its own setting: a client
        /// asking for thirty-two on a server configured for eight gets eight,
        /// and one asking for two gets two and is spared 285 columns it does
        /// not want.
        view_distance: Option<u32>,
    },
    /// The client did not acknowledge the core pack, so its registries have
    /// to carry their contents and no `[data] path` supplied any.
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
    let mut requested_view_distance = None;
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
            // Language, chat settings, skin parts — read so the frame is
            // consumed and the stream stays in step. The view distance is the
            // one field acted on, and it is kept rather than used here because
            // what consumes it is the join, several packets later.
            //
            // A client may send this again during play to change its settings.
            // That is not handled: the distance a player is served is settled
            // at the join, and re-streaming on a settings change is work with
            // nothing behind it until there is a per-tick budget to do it in.
            configuration::serverbound::Packet::ClientInformation(info) => {
                requested_view_distance = u32::try_from(info.view_distance).ok();
            }
            configuration::serverbound::Packet::CustomPayload(_) => {}
            other => {
                return Err(SessionError::OutOfTurn {
                    state: "configuration",
                    packet: other.name(),
                })
            }
        }
    };

    // A client that acknowledged the pack has the contents already and is sent
    // names, exactly as vanilla does. One that did not needs them from
    // somewhere, and the only somewhere is the operator's own data.
    if !acknowledged && ctx.registry_contents.is_empty() {
        return Ok(Configured::UnknownContent);
    }
    let contents = (!acknowledged).then_some(&ctx.registry_contents);

    for registry in dust_registry::synced::all() {
        // `None` for this registry means names, which is right in both of the
        // cases that produce it: the client has its own copy, or Dust has no
        // schema for this one and the client has been observed not to need it.
        let payloads = contents.and_then(|loaded| loaded.get(registry.name));
        if !acknowledged && payloads.is_none() {
            continue;
        }
        let entries = registry
            .entries
            .iter()
            .map(|entry| {
                Ok(RegistryEntry {
                    entry_id: identifier(entry)?,
                    // `None`, not an empty compound. The difference is "use
                    // your own copy" against "here is an empty definition", and
                    // the second gives a client a world with no biomes.
                    // The wire form is a bare network NBT tag: a type
                    // byte and a payload, with no root name. `to_vec_network`
                    // is the writer that leaves the name off, and using the
                    // named one here would put two bytes of empty string in
                    // front of every entry and shift everything after it.
                    data: payloads
                        .and_then(|contents| contents.get(entry))
                        .map(encode_entry)
                        .transpose()?,
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

    // Tags, after the registries and before the finish, which is where
    // vanilla puts them. They have to come after: a tag's entries are ids into
    // a registry, and a client that has not built that registry's mapping yet
    // has nothing to resolve them against.
    send(
        conn,
        configuration::clientbound::UpdateTags {
            registries: tag_registries()?,
        },
        version,
    )
    .await?;

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
            // A client that sends its settings *after* the pack answer is as
            // ordinary as one that sends them before, and the first loop above
            // may never have seen them.
            configuration::serverbound::Packet::ClientInformation(info) => {
                requested_view_distance = u32::try_from(info.view_distance).ok();
            }
            configuration::serverbound::Packet::CustomPayload(_)
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

    Ok(Configured::Ready {
        view_distance: requested_view_distance,
    })
}

/// Every tag of every registry, in the form `update_tags` carries.
///
/// Built per connection rather than once at boot, and that is a deliberate
/// cost of about a millisecond: the ids of the five datapack registries come
/// from the sync this session performed, and a table cached across sessions
/// would outlive the thing that gave its numbers meaning. When a datapack can
/// change a registry, this is the code that must already be per-session.
fn tag_registries() -> Result<Vec<common::TagRegistry>, SessionError> {
    dust_registry::tags::TagRegistry::ALL
        .into_iter()
        .map(|registry| {
            let tags = dust_registry::tags::wire(registry)
                .map_err(|e| SessionError::RegistryContents(e.to_string()))?
                .into_iter()
                .map(|tag| {
                    Ok(common::Tag {
                        name: identifier(tag.id)?,
                        entries: tag
                            .entries
                            .into_iter()
                            .map(|id| VarInt(id as i32))
                            .collect(),
                    })
                })
                .collect::<Result<Vec<_>, SessionError>>()?;
            Ok(common::TagRegistry {
                registry: identifier(registry.name())?,
                tags,
            })
        })
        .collect()
}

/// One registry entry's compound, as the bytes the packet carries.
///
/// The failure case is a compound too deep or too large for the NBT writer,
/// which cannot come from either of the two schemas — but it arrives from a
/// directory somebody else wrote, and "cannot happen" about a file on disk is
/// not a claim worth betting a panic on.
fn encode_entry(compound: &dust_nbt::Compound) -> Result<Nbt, SessionError> {
    dust_nbt::write::to_vec_network(Some(&dust_nbt::Tag::Compound(compound.clone())))
        .map(Nbt)
        .map_err(|e| SessionError::RegistryContents(e.to_string()))
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
