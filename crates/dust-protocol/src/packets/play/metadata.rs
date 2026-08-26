//! Entity metadata: the key/value side channel that rides alongside entities.
//!
//! # What metadata is, and why it is an open set
//!
//! Every entity the client can see carries named slots — on fire, pose,
//! health, custom name — addressed by index and typed by a *serializer* id.
//! The serializer table is versioned data: 1.20.5 added one, older versions
//! lack it, and the next release can add another. So this file implements a
//! closed Rust enum over an **open** wire format, and the seam between those
//! two is the whole design.
//!
//! The rule is [`MetadataValue::from_discriminant`]: a serializer this crate
//! models decodes into a variant; a serializer it does not returns
//! [`DecodeError::Unsupported`] naming the id. It does not guess. A
//! serializer's value has **no length**, so stepping past an unknown one
//! without knowing its layout loses every field after it — the same wall
//! `crate::types::Slot` documents, and the same answer: refuse at the exact
//! byte rather than desynchronise.
//!
//! What is modelled is chosen by what a server sends in ordinary play: the
//! numeric and text types, positions and directions, the optional wrappers,
//! NBT, villager data, pose. What is not — item stacks (the Slot seam),
//! particles, and the registry-backed inline variants — is refused by name,
//! and each refusal says what finishing it requires.
//!
//! # The terminator
//!
//! Metadata entries run until an entry whose index byte is `0xFF`. That makes
//! the list self-delimiting and means "no metadata" is one terminator byte,
//! which is why [`MetadataEntries`] exists as a type instead of a bare
//! `Vec`: the terminator is part of the format, and a bare vector has no
//! place to put it.

use crate::nbt::Nbt;
use crate::types::{
    Decode, Encode, Identifier, Position, ProtocolString, Uuid, VarInt, VarLong,
};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{text::Component, ProtocolVersion};

/// One entry's payload, tagged by its serializer.
///
/// Variant names follow the serializers' own names so a diff against the
/// protocol documentation reads as renames and nothing else.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Byte(i8),
    VarInt(VarInt),
    VarLong(VarLong),
    Float(f32),
    String(ProtocolString),
    TextComponent(Component),
    OptionalTextComponent(Option<Component>),
    Boolean(bool),
    /// Pitch, yaw and roll in degrees, exactly as sent and never normalised.
    Rotations(f32, f32, f32),
    BlockPosition(Position),
    OptionalBlockPosition(Option<Position>),
    /// One of six directions, carried as a VarInt discriminant.
    LookDirection(LookDirection),
    OptionalUuid(Option<Uuid>),
    /// A block state id; validated only for range by whoever resolves states.
    BlockState(VarInt),
    /// Zero means absent, because air is unrepresentable as a state id.
    OptionalBlockState(Option<VarInt>),
    CompoundTag(Nbt),
    /// Villager type, profession and level — three ids and a number, kept
    /// together because splitting them across fields would let them disagree.
    VillagerData {
        kind: VarInt,
        profession: VarInt,
        level: VarInt,
    },
    /// An optional entity id, spelled as zero for absent and *value plus one*
    /// otherwise. Not a misprint: the offset is how vanilla distinguishes
    /// "no entity" from "entity zero".
    OptionalEntityId(Option<VarInt>),
    Pose(Pose),
    /// Ids into registry-backed tables — cat and frog variants, sniffer and
    /// armadillo states — which share one shape and differ only in which
    /// registry resolves them. Kept raw here for the same reason tags are:
    /// resolving an id is the registry crate's job.
    RegistryId(VarInt),
    OptionalGlobalPosition(Option<(Identifier, Position)>),
    Vector(f32, f32, f32),
    Quaternion(f32, f32, f32, f32),
}

/// The directions metadata speaks of: where a block face points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookDirection {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

crate::var_int_enum! {
    /// The named poses the client animates between.
    ///
    /// Closed at the values 1.21.1 defines; a newer peer sending a later pose
    /// is an unknown-variant refusal here, which is the honest answer until
    /// this table gains the version dimension the rest of the file will grow
    /// into.
    pub enum Pose {
        Standing = 0,
        FallFlying = 1,
        Sleeping = 2,
        Swimming = 3,
        SpinAttack = 4,
        Sneaking = 5,
        LongJumping = 6,
        Dying = 7,
        Croaking = 8,
        UsingTongue = 9,
        Sitting = 10,
        Roaring = 11,
        Sniffing = 12,
        Emerging = 13,
        Digging = 14,
    }
}

impl MetadataValue {
    /// The wire's serializer id for each modelled variant.
    fn discriminator(&self) -> i32 {
        match self {
            Self::Byte(_) => 0,
            Self::VarInt(_) => 1,
            Self::VarLong(_) => 2,
            Self::Float(_) => 3,
            Self::String(_) => 4,
            Self::TextComponent(_) => 5,
            Self::OptionalTextComponent(_) => 6,
            Self::Boolean(_) => 8,
            Self::Rotations(..) => 9,
            Self::BlockPosition(_) => 10,
            Self::OptionalBlockPosition(_) => 11,
            Self::LookDirection(_) => 12,
            Self::OptionalUuid(_) => 13,
            Self::BlockState(_) => 14,
            Self::OptionalBlockState(_) => 15,
            Self::CompoundTag(_) => 16,
            Self::VillagerData { .. } => 19,
            Self::OptionalEntityId(_) => 20,
            Self::Pose(_) => 21,
            Self::RegistryId(_) => 22,
            Self::OptionalGlobalPosition(_) => 25,
            Self::Vector(..) => 29,
            Self::Quaternion(..) => 30,
        }
    }

    fn read<R: WireRead + ?Sized>(
        serializer: i32,
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        match serializer {
            0 => input.read_i8().map(Self::Byte),
            1 => VarInt::decode(input, version).map(Self::VarInt),
            2 => VarLong::decode(input, version).map(Self::VarLong),
            3 => input.read_f32().map(Self::Float),
            4 => ProtocolString::decode(input, version).map(Self::String),
            5 => Component::decode(input, version).map(Self::TextComponent),
            6 => Option::<Component>::decode(input, version).map(Self::OptionalTextComponent),
            8 => input.read_bool().map(Self::Boolean),
            9 => Ok(Self::Rotations(
                input.read_f32()?,
                input.read_f32()?,
                input.read_f32()?,
            )),
            10 => Position::decode(input, version).map(Self::BlockPosition),
            11 => Option::<Position>::decode(input, version).map(Self::OptionalBlockPosition),
            12 => LookDirection::decode(input, version).map(Self::LookDirection),
            13 => Option::<Uuid>::decode(input, version).map(Self::OptionalUuid),
            // Block state and the registry-backed variant ids are bare ids
            // into different registries, so they share the raw treatment.
            14 => VarInt::decode(input, version).map(Self::BlockState),
            22 | 24 | 27 | 28 => VarInt::decode(input, version).map(Self::RegistryId),
            15 => match VarInt::decode(input, version)? {
                VarInt(0) => Ok(Self::OptionalBlockState(None)),
                VarInt(state) => Ok(Self::OptionalBlockState(Some(VarInt(state - 1)))),
            },
            16 => Nbt::decode(input, version).map(Self::CompoundTag),
            19 => Ok(Self::VillagerData {
                kind: VarInt::decode(input, version)?,
                profession: VarInt::decode(input, version)?,
                level: VarInt::decode(input, version)?,
            }),
            20 => match VarInt::decode(input, version)? {
                VarInt(0) => Ok(Self::OptionalEntityId(None)),
                VarInt(value) => Ok(Self::OptionalEntityId(Some(VarInt(value - 1)))),
            },
            21 => Pose::decode(input, version).map(Self::Pose),
            25 => {
                // A boolean, then the dimension and the position only if it
                // was true — the protocol's optional shape again, spelled by
                // hand because no tuple type implements it.
                if input.read_bool()? {
                    let dimension = Identifier::decode(input, version)?;
                    let position = Position::decode(input, version)?;
                    Ok(Self::OptionalGlobalPosition(Some((dimension, position))))
                } else {
                    Ok(Self::OptionalGlobalPosition(None))
                }
            }
            29 => Ok(Self::Vector(
                input.read_f32()?,
                input.read_f32()?,
                input.read_f32()?,
            )),
            30 => Ok(Self::Quaternion(
                input.read_f32()?,
                input.read_f32()?,
                input.read_f32()?,
                input.read_f32()?,
            )),
            other => Err(DecodeError::Unsupported {
                field: "entity metadata",
                why: refusal_why(other),
            }),
        }
    }
}

/// Why a serializer id is refused, per refused family.
///
/// A function rather than inline strings so that adding an arm to `read` is
/// the entire change when the subset grows, and this keeps telling the truth
/// about everything still outside it.
fn refusal_why(serializer: i32) -> &'static str {
    match serializer {
        7 => "an item stack needs component layouts, which no report extracts yet",
        17 | 18 => "particle options have no length and their layouts live outside any report",
        23 | 26 => "wolf and painting variants may be inline definitions, which are outside \
                    the supported subset",
        _ => "this serializer id is not modelled; its value has no length, so it cannot be \
              stepped over and the packet is refused",
    }
}

impl Encode for MetadataValue {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.discriminator());
        match self {
            Self::Byte(value) => out.write_i8(*value),
            Self::VarInt(value) => return value.encode(out, version),
            Self::VarLong(value) => return value.encode(out, version),
            Self::Float(value) => out.write_f32(*value),
            Self::String(value) => return value.encode(out, version),
            Self::TextComponent(value) => return value.encode(out, version),
            Self::OptionalTextComponent(value) => return value.encode(out, version),
            Self::Boolean(value) => out.write_bool(*value),
            Self::Rotations(x, y, z) | Self::Vector(x, y, z) => {
                out.write_f32(*x);
                out.write_f32(*y);
                out.write_f32(*z);
            }
            Self::Quaternion(x, y, z, w) => {
                out.write_f32(*x);
                out.write_f32(*y);
                out.write_f32(*z);
                out.write_f32(*w);
            }
            Self::BlockPosition(value) => return value.encode(out, version),
            Self::OptionalBlockPosition(value) => return value.encode(out, version),
            Self::LookDirection(value) => out.write_var_int(*value as i32),
            Self::OptionalUuid(value) => return value.encode(out, version),
            Self::BlockState(value) => return value.encode(out, version),
            Self::OptionalBlockState(value) => match value {
                None => out.write_var_int(0),
                Some(state) => out.write_var_int(state.0 + 1),
            },
            Self::CompoundTag(value) => return value.encode(out, version),
            Self::VillagerData {
                kind,
                profession,
                level,
            } => {
                kind.encode(out, version)?;
                profession.encode(out, version)?;
                return level.encode(out, version);
            }
            Self::OptionalEntityId(value) => match value {
                None => out.write_var_int(0),
                Some(id) => out.write_var_int(id.0.wrapping_add(1)),
            },
            Self::Pose(value) => out.write_var_int(value.discriminant()),
            Self::RegistryId(value) => return value.encode(out, version),
            Self::OptionalGlobalPosition(value) => {
                return match value {
                    None => {
                        out.write_bool(false);
                        Ok(())
                    }
                    Some((dimension, position)) => {
                        out.write_bool(true);
                        dimension.encode(out, version)?;
                        position.encode(out, version)
                    }
                };
            }        }
        Ok(())
    }
}

impl Decode for LookDirection {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_var_int()?;
        match raw {
            0 => Ok(Self::Down),
            1 => Ok(Self::Up),
            2 => Ok(Self::North),
            3 => Ok(Self::South),
            4 => Ok(Self::West),
            5 => Ok(Self::East),
            other => Err(DecodeError::UnknownVariant {
                name: "LookDirection",
                value: other,
            }),
        }
    }
}

impl Encode for LookDirection {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(*self as i32);
        Ok(())
    }
}

/// One indexed slot on an entity, short of its terminator.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    /// Which slot this is. The meaning depends on the entity kind, which is
    /// knowledge the sim owns; this layer only promises the byte travels
    /// unchanged.
    pub index: u8,
    pub value: MetadataValue,
}

/// The full metadata list of one entity update, terminator included.
///
/// Distinct from `Vec<MetadataEntry>` precisely so the terminator has an
/// owner: encoding writes it, decoding consumes it, and neither side can
/// forget the byte that ends the list without failing to compile against the
/// packet definition.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetadataEntries(pub Vec<MetadataEntry>);

const TERMINATOR: u8 = 0xFF;

impl Decode for MetadataEntries {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let mut entries = Vec::new();
        loop {
            let index = input.read_u8()?;
            if index == TERMINATOR {
                return Ok(Self(entries));
            }
            let serializer = VarInt::decode(input, version)?.0;
            entries.push(MetadataEntry {
                index,
                value: MetadataValue::read(serializer, input, version)?,
            });
        }
    }
}

impl Encode for MetadataEntries {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        for entry in &self.0 {
            out.write_u8(entry.index);
            entry.value.encode(out, version)?;
        }
        out.write_u8(TERMINATOR);
        Ok(())
    }
}
