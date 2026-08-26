//! The boss bar: one packet, six layouts, chosen by an action enum.
//!
//! The action is a VarInt at the front and decides what follows — add carries
//! the full bar, the others carry the single field they change. That is a
//! value-dependent tail, which [`crate::wire_struct`] cannot express and
//! should not learn to: like every other such tail in this crate it becomes
//! one named type with one job, and the packet definition stays declarative.

use crate::text::Component;
use crate::types::{Decode, Encode, Uuid, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, ProtocolVersion};

var_int_enum! {
    /// The colour of the bar's fill.
    ///
    /// Purely cosmetic, which is exactly why it is worth an enum: nothing in
    /// gameplay depends on it, so nothing else would ever notice a wrong id.
    pub enum BossBarColor {
        Pink = 0,
        Blue = 1,
        Red = 2,
        Green = 3,
        Yellow = 4,
        Purple = 5,
        White = 6,
    }
}

var_int_enum! {
    /// How the bar divides into notches.
    pub enum BossBarDivision {
        Solid = 0,
        Notches6 = 1,
        Notches10 = 2,
        Notches12 = 3,
        Notches20 = 4,
    }
}

/// The bar's behaviour bits: darken sky, play the dragon music, create fog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BossBarFlags(pub u8);

impl BossBarFlags {
    pub const DARKEN_SKY: u8 = 0x01;
    pub const DRAGON_BAR: u8 = 0x02;
    pub const CREATE_FOG: u8 = 0x04;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl Decode for BossBarFlags {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for BossBarFlags {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

/// One boss-bar update: which bar, and what changed on it.
///
/// `Add` carries everything; the rest carry exactly their field, because that
/// is all the client needs to update it.
#[derive(Debug, Clone, PartialEq)]
pub enum BossBarAction {
    Add {
        title: Component,
        /// Fill fraction from 0 to 1; values above 1 render extra bars rather
        /// than failing, which is the client's business and not ours.
        health: f32,
        color: BossBarColor,
        division: BossBarDivision,
        flags: BossBarFlags,
    },
    Remove,
    UpdateHealth(f32),
    UpdateTitle(Component),
    UpdateStyle {
        color: BossBarColor,
        division: BossBarDivision,
    },
    UpdateProperties(BossBarFlags),
}

impl Decode for BossBarAction {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        match VarInt::decode(input, version)?.0 {
            0 => Ok(Self::Add {
                title: Component::decode(input, version)?,
                health: input.read_f32()?,
                color: BossBarColor::decode(input, version)?,
                division: BossBarDivision::decode(input, version)?,
                flags: BossBarFlags::decode(input, version)?,
            }),
            1 => Ok(Self::Remove),
            2 => Ok(Self::UpdateHealth(input.read_f32()?)),
            3 => Ok(Self::UpdateTitle(Component::decode(input, version)?)),
            4 => Ok(Self::UpdateStyle {
                color: BossBarColor::decode(input, version)?,
                division: BossBarDivision::decode(input, version)?,
            }),
            5 => Ok(Self::UpdateProperties(BossBarFlags::decode(
                input, version,
            )?)),
            other => Err(DecodeError::UnknownVariant {
                name: "BossBarAction",
                value: other,
            }),
        }
    }
}

impl Encode for BossBarAction {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Add {
                title,
                health,
                color,
                division,
                flags,
            } => {
                out.write_var_int(0);
                title.encode(out, version)?;
                out.write_f32(*health);
                color.encode(out, version)?;
                division.encode(out, version)?;
                flags.encode(out, version)?;
                Ok(())
            }
            Self::Remove => {
                out.write_var_int(1);
                Ok(())
            }
            Self::UpdateHealth(health) => {
                out.write_var_int(2);
                out.write_f32(*health);
                Ok(())
            }
            Self::UpdateTitle(title) => {
                out.write_var_int(3);
                title.encode(out, version)
            }
            Self::UpdateStyle { color, division } => {
                out.write_var_int(4);
                color.encode(out, version)?;
                division.encode(out, version)
            }
            Self::UpdateProperties(flags) => {
                out.write_var_int(5);
                flags.encode(out, version)
            }
        }
    }
}

/// Everything after the packet id: which bar, then its action.
///
/// A pair rather than two packet fields for the same reason the action is an
/// enum: the uuid leads every layout and the action owns the rest, and one
/// type keeps them from being read apart.
#[derive(Debug, Clone, PartialEq)]
pub struct BossEventBody {
    pub uuid: Uuid,
    pub action: BossBarAction,
}

impl Decode for BossEventBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            uuid: Uuid::decode(input, version)?,
            action: BossBarAction::decode(input, version)?,
        })
    }
}

impl Encode for BossEventBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.uuid.encode(out, version)?;
        self.action.encode(out, version)
    }
}
