//! The sound field types: how a sound names itself, and where it plays from.
//!
//! # Why a sound is a tagged union and not an id
//!
//! Since 1.19.3 a sound on the wire is either the id of a registry entry or
//! the entry itself, inline. The inline form exists so custom content can play
//! a sound the client has no registry row for; the id form exists because
//! almost every sound a server plays *is* in the registry, and an identifier
//! costs twenty bytes where an id costs one.
//!
//! The two are told apart by the value of the leading VarInt: zero means "the
//! name follows", anything else means "this is the registry id plus one". That
//! plus-one is not decoration — it is what reserves zero as the escape hatch,
//! exactly as [`crate::packets::play::chat::AcknowledgedMessage`] does for
//! message references. Reading the VarInt as a plain id silently corrupts
//! every inline sound into the wrong entry.

use crate::types::{Decode, Encode, Identifier, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::ProtocolVersion;

/// A world coordinate as the positioned sound packets carry it: eighths of a
/// block.
///
/// Vanilla's `ClientboundSoundPacket` stores `(int)(x * 8.0)` and the client
/// divides by eight again, which gives a sound three bits of sub-block
/// precision and no more. It is a free function rather than a field type
/// because the packet's fields are what the wire holds and this is what a
/// caller has — and because the failure it prevents is silent: a block
/// coordinate written straight into the field is a legal int that puts the
/// sound an eighth of the way to the origin.
///
/// Truncating toward zero, as the cast vanilla writes does. A sound at
/// `x = -0.05` therefore plays at 0 on both implementations rather than at
/// -1/8 on one of them.
#[must_use]
pub fn eighths(blocks: f64) -> i32 {
    (blocks * 8.0) as i32
}

/// Where in the client's mixer a sound plays from, which is what the volume
/// sliders control.
///
/// The ids are the protocol's own and stable across versions in practice, but
/// they are wire data and not Rust truth: an unknown category from a future
/// peer is refused by name rather than mapped onto `Master`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundCategory {
    Master = 0,
    Music = 1,
    Record = 2,
    Weather = 3,
    Block = 4,
    Hostile = 5,
    Neutral = 6,
    Player = 7,
    Ambient = 8,
    Voice = 9,
}

impl SoundCategory {
    /// Every category, for tests and iteration.
    pub const ALL: [Self; 10] = [
        Self::Master,
        Self::Music,
        Self::Record,
        Self::Weather,
        Self::Block,
        Self::Hostile,
        Self::Neutral,
        Self::Player,
        Self::Ambient,
        Self::Voice,
    ];

    pub const fn discriminant(self) -> i32 {
        self as i32
    }

    pub const fn from_discriminant(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Master),
            1 => Some(Self::Music),
            2 => Some(Self::Record),
            3 => Some(Self::Weather),
            4 => Some(Self::Block),
            5 => Some(Self::Hostile),
            6 => Some(Self::Neutral),
            7 => Some(Self::Player),
            8 => Some(Self::Ambient),
            9 => Some(Self::Voice),
            _ => None,
        }
    }
}

impl Decode for SoundCategory {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let value = input.read_var_int()?;
        Self::from_discriminant(value).ok_or(DecodeError::UnknownVariant {
            name: "SoundCategory",
            value,
        })
    }
}

impl Encode for SoundCategory {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.discriminant());
        Ok(())
    }
}

/// A named sound: either a registry id or the sound event itself, inline.
///
/// The inline form carries its own optional fixed range — a sound that always
/// reaches the same distance regardless of volume. Registry sounds have their
/// range defined by their entry; only the inline form needs to carry it.
#[derive(Debug, Clone, PartialEq)]
pub enum SoundId {
    /// An id into the `minecraft:sound_event` registry, already offset: the
    /// wire carries id + 1, and zero would mean the inline form instead.
    Id(VarInt),
    Inline {
        name: Identifier,
        /// The sound's maximum range when it has a fixed one.
        fixed_range: Option<f32>,
    },
}

impl Decode for SoundId {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let raw = input.read_var_int()?;
        if raw == 0 {
            let name = Identifier::decode(input, version)?;
            let fixed_range = Option::<f32>::decode(input, version)?;
            Ok(Self::Inline { name, fixed_range })
        } else {
            Ok(Self::Id(VarInt(raw - 1)))
        }
    }
}

impl Encode for SoundId {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Id(id) => {
                // The wire carries id + 1 so that zero can mark the inline
                // form. An id of -1 would therefore encode as zero and be
                // read back as something else entirely — refused instead of
                // crossing forms.
                if id.0 < 0 {
                    return Err(EncodeError::Unsupported {
                        field: "sound id",
                        why: "a negative registry id would encode as the inline form's marker",
                    });
                }
                out.write_var_int(id.0.wrapping_add(1));
                Ok(())
            }
            Self::Inline { name, fixed_range } => {
                out.write_var_int(0);
                name.encode(out, version)?;
                fixed_range.encode(out, version)
            }
        }
    }
}

/// Everything after the packet id: which sound to stop, if any.
///
/// One flags byte selects among four layouts — neither half, the category
/// alone, the name alone, or both. There is no "stop everything" sentinel
/// beyond the empty layout, and an unknown flag is refused rather than read
/// as a partial match, because a guessed layout would eat the next packet's
/// bytes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StopSoundBody {
    /// The mixer category to silence; `None` means every category.
    pub source: Option<SoundCategory>,
    /// The specific sound to stop; `None` means every sound in `source`.
    pub name: Option<Identifier>,
}

impl StopSoundBody {
    const HAS_SOURCE: u8 = 0x01;
    const HAS_NAME: u8 = 0x02;
}

impl Decode for StopSoundBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let flags = input.read_u8()?;
        let known = flags & !(Self::HAS_SOURCE | Self::HAS_NAME) == 0;
        if !known {
            return Err(DecodeError::UnknownVariant {
                name: "StopSoundBody",
                value: i32::from(flags),
            });
        }
        let source = if flags & Self::HAS_SOURCE != 0 {
            Some(SoundCategory::decode(input, version)?)
        } else {
            None
        };
        let name = if flags & Self::HAS_NAME != 0 {
            Some(Identifier::decode(input, version)?)
        } else {
            None
        };
        Ok(Self { source, name })
    }
}

impl Encode for StopSoundBody {
    fn encode<W: crate::wire::WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), crate::wire::EncodeError> {
        let mut flags = 0u8;
        if self.source.is_some() {
            flags |= Self::HAS_SOURCE;
        }
        if self.name.is_some() {
            flags |= Self::HAS_NAME;
        }
        out.write_u8(flags);
        if let Some(source) = self.source {
            source.encode(out, version)?;
        }
        if let Some(name) = &self.name {
            name.encode(out, version)?;
        }
        Ok(())
    }
}
