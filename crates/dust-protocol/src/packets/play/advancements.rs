//! Advancement sync: the tree, its displays, and each player's progress.
//!
//! # Why the envelope is exact and the icons are not
//!
//! An advancement entry is a chain of optional sections — parent, display,
//! criteria, telemetry — none of them length-prefixed. Finding the end of one
//! advancement means reading every section's shape, so the structure here is
//! complete even though the display's icon bottoms out in [`Slot`] and
//! therefore refuses component-bearing stacks by name. That is the same seam
//! as everywhere else in this crate: the envelope is ours, the item stack is
//! not, and the refusal names which of those failed.

use crate::nbt::TextComponent;
use crate::types::{Decode, Encode, Identifier, ProtocolString, Slot};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, ProtocolVersion};

var_int_enum! {
    /// How an advancement presents itself: task, challenge or goal.
    pub enum FrameType {
        Task = 0,
        Challenge = 1,
        Goal = 2,
    }
}

/// The display card shown in the advancement screen, when there is one.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementDisplay {
    pub title: TextComponent,
    pub description: TextComponent,
    pub icon: Slot,
    pub frame: FrameType,
    /// Bit 0: `background` follows. Bits 1 and 2: toast and hidden.
    pub flags: i32,
    pub background: Option<Identifier>,
    pub x: f32,
    pub y: f32,
}

impl AdvancementDisplay {
    pub const HAS_BACKGROUND: i32 = 0x01;
    pub const SHOW_TOAST: i32 = 0x02;
    pub const HIDDEN: i32 = 0x04;
}

impl Decode for AdvancementDisplay {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let title = TextComponent::decode(input, version)?;
        let description = TextComponent::decode(input, version)?;
        let icon = Slot::decode(input, version)?;
        let frame = FrameType::decode(input, version)?;
        let flags = input.read_i32()?;
        let background = if flags & Self::HAS_BACKGROUND != 0 {
            Some(Identifier::decode(input, version)?)
        } else {
            None
        };
        let x = input.read_f32()?;
        let y = input.read_f32()?;
        Ok(Self {
            title,
            description,
            icon,
            frame,
            flags,
            background,
            x,
            y,
        })
    }
}

impl Encode for AdvancementDisplay {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.title.encode(out, version)?;
        self.description.encode(out, version)?;
        self.icon.encode(out, version)?;
        self.frame.encode(out, version)?;
        out.write_i32(self.flags);
        if self.flags & Self::HAS_BACKGROUND != 0 {
            // A flag promising a background without one would leave the peer
            // reading the coordinates as an identifier — refusing beats that.
            return match &self.background {
                Some(background) => background.encode(out, version),
                None => Err(EncodeError::Unsupported {
                    field: "advancement display",
                    why: "the flags promise a background texture and none was given",
                }),
            };
        }
        out.write_f32(self.x);
        out.write_f32(self.y);
        Ok(())
    }
}

/// One advancement in the tree: lineage, display, criteria, telemetry.
///
/// The requirements are a list of *lists* of criterion names: the outer list
/// holds alternatives, each inner list the criteria that must all be met. The
/// names travel as bounded strings, which is what [`ProtocolString`] is — the
/// wire spells them plain, with no component structure to refuse.
#[derive(Debug, Clone, PartialEq)]
pub struct Advancement {
    pub parent: Option<Identifier>,
    pub display: Option<AdvancementDisplay>,
    pub criteria: Vec<Identifier>,
    pub requirements: Vec<Vec<ProtocolString>>,
    pub sends_telemetry: bool,
}

impl Decode for Advancement {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let has_parent = input.read_bool()?;
        let parent = if has_parent {
            Some(Identifier::decode(input, version)?)
        } else {
            None
        };
        let has_display = input.read_bool()?;
        let display = if has_display {
            Some(AdvancementDisplay::decode(input, version)?)
        } else {
            None
        };
        let criteria = Vec::<Identifier>::decode(input, version)?;
        let requirements = Vec::<Vec<ProtocolString>>::decode(input, version)?;
        let sends_telemetry = input.read_bool()?;
        Ok(Self {
            parent,
            display,
            criteria,
            requirements,
            sends_telemetry,
        })
    }
}

impl Encode for Advancement {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match &self.parent {
            Some(parent) => {
                out.write_bool(true);
                parent.encode(out, version)?;
            }
            None => out.write_bool(false),
        }
        match &self.display {
            Some(display) => {
                out.write_bool(true);
                display.encode(out, version)?;
            }
            None => out.write_bool(false),
        }
        self.criteria.encode(out, version)?;
        self.requirements.encode(out, version)?;
        out.write_bool(self.sends_telemetry);
        Ok(())
    }
}

/// One criterion's state for one player.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionProgress {
    pub identifier: Identifier,
    /// When the criterion was achieved, in epoch milliseconds.
    pub achieved_at: Option<i64>,
}

impl Decode for CriterionProgress {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let identifier = Identifier::decode(input, version)?;
        let achieved_at = Option::<i64>::decode(input, version)?;
        Ok(Self {
            identifier,
            achieved_at,
        })
    }
}

impl Encode for CriterionProgress {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.identifier.encode(out, version)?;
        self.achieved_at.encode(out, version)
    }
}

/// One advancement's progress for one player.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementProgress {
    pub key: Identifier,
    pub criteria: Vec<CriterionProgress>,
}

impl Decode for AdvancementProgress {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            key: Identifier::decode(input, version)?,
            criteria: Vec::<CriterionProgress>::decode(input, version)?,
        })
    }
}

impl Encode for AdvancementProgress {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.key.encode(out, version)?;
        self.criteria.encode(out, version)
    }
}
