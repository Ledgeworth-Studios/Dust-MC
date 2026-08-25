//! Structures that appear inside more than one packet body.
//!
//! Also the two seams that are not the wire and not NBT: registry sync, which
//! `dust-registry` will interpret, and the parts of the protocol that carry a
//! payload this crate deliberately does not open.

use crate::nbt::{Nbt, TextComponent};
use crate::types::{BoundedString, Decode, Encode, Identifier, ProtocolString, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{wire_struct, ProtocolVersion};

wire_struct! {
    /// A signed property on a player's profile — the skin, mostly.
    pub struct ProfileProperty {
        name: BoundedString<64>,
        value: ProtocolString,
        /// Present when the property is signed by Mojang's session server. An
        /// offline-mode server sends none of these, which is why a decoder that
        /// assumed a signature is present passes every offline test and fails
        /// against the real thing.
        signature: Option<BoundedString<1024>>,
    }
}

wire_struct! {
    /// One entry of one registry, during configuration.
    ///
    /// # The `dust-registry` seam
    ///
    /// The data is an NBT blob and this crate does not open it. That is not a
    /// shortcut — interpreting it means knowing what a dimension type or a
    /// biome or a chat type *is*, and that is `dust-registry`'s subject, not
    /// the protocol's. What this layer owes the registry is the entry's id and
    /// its bytes, delimited correctly, and that is what this is.
    ///
    /// `has data` being false is a real case and not an encoding of an empty
    /// blob: it means "this entry exists and the client should use its own
    /// built-in definition", which is how the select-known-packs negotiation
    /// pays off. A decoder that collapsed `None` into an empty NBT compound
    /// would turn "use your copy" into "use this empty one", and the client
    /// would end up with a world with no dimension types.
    pub struct RegistryEntry {
        entry_id: Identifier,
        data: Option<Nbt>,
    }
}

wire_struct! {
    /// One registry's worth of tags.
    pub struct TagRegistry {
        registry: Identifier,
        tags: Vec<Tag>,
    }
}

wire_struct! {
    /// A tag: a name and the registry ids in it.
    ///
    /// The entries are raw VarInts on purpose. They are ids into the registry
    /// named by the enclosing [`TagRegistry`], and resolving them is the other
    /// half of the `dust-registry` seam — this crate has no way to know
    /// whether id 42 in `minecraft:block` is what it will be next release, and
    /// pretending otherwise would bake a version's numbering into a type.
    pub struct Tag {
        name: Identifier,
        entries: Vec<VarInt>,
    }
}

wire_struct! {
    /// A data pack both sides may already have.
    ///
    /// The point of the exchange is that a client that already has the vanilla
    /// registries does not need them sent, which is most of the several hundred
    /// kilobytes of configuration.
    pub struct KnownPack {
        namespace: ProtocolString,
        id: ProtocolString,
        version: ProtocolString,
    }
}

wire_struct! {
    /// One line of the detail a client attaches to a crash report.
    pub struct ReportDetail {
        title: BoundedString<128>,
        description: BoundedString<4096>,
    }
}

crate::var_int_enum! {
    /// The links a server can offer that the client has its own wording for.
    pub enum BuiltInLinkLabel {
        BugReport = 0,
        CommunityGuidelines = 1,
        Support = 2,
        Status = 3,
        Feedback = 4,
        Community = 5,
        Website = 6,
        Forums = 7,
        News = 8,
        Announcements = 9,
    }
}

/// A server link's label, which is a tagged union rather than an optional.
///
/// A boolean chooses between a VarInt naming one of the client's built-in
/// labels and a text component the server supplies. This needs a hand-written
/// codec because the *type* of the next field depends on the value of this one,
/// which is the one shape [`wire_struct!`](crate::wire_struct) cannot express —
/// and deliberately cannot, because a macro that could would let a definition
/// hide a branch in what is meant to be a readable list of fields.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLinkLabel {
    /// One of the labels the client already has wording for, in every language
    /// it ships. Preferred over sending text, which is only in one.
    BuiltIn(BuiltInLinkLabel),
    Custom(TextComponent),
}

impl Decode for ServerLinkLabel {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        if input.read_bool()? {
            BuiltInLinkLabel::decode(input, version).map(Self::BuiltIn)
        } else {
            TextComponent::decode(input, version).map(Self::Custom)
        }
    }
}

impl Encode for ServerLinkLabel {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::BuiltIn(label) => {
                out.write_bool(true);
                label.encode(out, version)
            }
            Self::Custom(text) => {
                out.write_bool(false);
                text.encode(out, version)
            }
        }
    }
}

wire_struct! {
    /// A link a server offers in the pause menu.
    pub struct ServerLink {
        label: ServerLinkLabel,
        url: ProtocolString,
    }
}
