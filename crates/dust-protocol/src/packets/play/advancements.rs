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
            match &self.background {
                Some(background) => background.encode(out, version)?,
                None => {
                    return Err(EncodeError::Unsupported {
                        field: "advancement display",
                        why: "the flags promise a background texture and none was given",
                    })
                }
            }
        }
        // The grid position follows whether or not there was a background:
        // the flag gates only the texture, never the coordinates.
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

/// One key-value row of the advancement sync's two maps.
///
/// The wire spells a map as rows of key then value; this is one row. Two
/// wrappers where one generic would do, because [`Advancement`] and
/// [`AdvancementProgress`] are different documents about the same tree and
/// conflating them lets a progress row be written where an advancement
/// belongs.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedAdvancement {
    pub key: Identifier,
    pub value: Advancement,
}

impl Decode for NamedAdvancement {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            key: Identifier::decode(input, version)?,
            value: Advancement::decode(input, version)?,
        })
    }
}

impl Encode for NamedAdvancement {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.key.encode(out, version)?;
        self.value.encode(out, version)
    }
}

/// One key-value row of the progress half of the sync.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedProgress {
    pub key: Identifier,
    pub value: AdvancementProgress,
}

impl Decode for NamedProgress {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            key: Identifier::decode(input, version)?,
            value: AdvancementProgress::decode(input, version)?,
        })
    }
}

impl Encode for NamedProgress {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.key.encode(out, version)?;
        self.value.encode(out, version)
    }
}

/// Everything after the packet id: the whole advancement state, in one shot.
///
/// There is no incremental form. Every sync either replaces what the client
/// holds (`reset`) or patches it, and both halves travel together because the
/// client rebuilds its screen from this one message.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementsBody {
    /// Whether the client discards everything it had before applying this.
    pub reset: bool,
    pub added: Vec<NamedAdvancement>,
    pub removed: Vec<Identifier>,
    pub progress: Vec<NamedProgress>,
}

impl Decode for AdvancementsBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            reset: input.read_bool()?,
            added: Vec::<NamedAdvancement>::decode(input, version)?,
            removed: Vec::<Identifier>::decode(input, version)?,
            progress: Vec::<NamedProgress>::decode(input, version)?,
        })
    }
}

impl Encode for AdvancementsBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_bool(self.reset);
        self.added.encode(out, version)?;
        self.removed.encode(out, version)?;
        self.progress.encode(out, version)
    }
}

var_int_enum! {
    /// What the advancement screen just did.
    ///
    /// `OpenedTab` carries the tab that opened; `ClosedScreen` carries
    /// nothing, which is why the body below holds the id as an `Option`.
    pub enum SeenAdvancementsAction {
        OpenedTab = 0,
        ClosedScreen = 1,
    }
}

/// Everything after the packet id: the action, and the tab if there is one.
///
/// A closed screen has no tab to name, so the pair stays together here — a
/// tab arriving without the opened-tab action would be a layout the peer did
/// not send.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenAdvancementsBody {
    pub action: SeenAdvancementsAction,
    pub tab: Option<Identifier>,
}

impl Decode for SeenAdvancementsBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let action = SeenAdvancementsAction::decode(input, version)?;
        let tab = match action {
            SeenAdvancementsAction::OpenedTab => Some(Identifier::decode(input, version)?),
            SeenAdvancementsAction::ClosedScreen => None,
        };
        Ok(Self { action, tab })
    }
}

impl Encode for SeenAdvancementsBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.action.encode(out, version)?;
        if self.action == SeenAdvancementsAction::OpenedTab {
            return match &self.tab {
                Some(tab) => tab.encode(out, version),
                None => Err(EncodeError::Unsupported {
                    field: "seen advancements",
                    why: "the opened-tab action names a tab and none was given",
                }),
            };
        }
        Ok(())
    }
}
