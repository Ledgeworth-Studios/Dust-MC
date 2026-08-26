//! The scoreboard family's value-dependent tails.
//!
//! # Why these are bodies and not packet fields
//!
//! Three of the four scoreboard packets pick their remaining layout from an
//! early field: an objective's update mode, a team's method byte, and the
//! presence booleans around a score's display name. A field cannot see the
//! field before it, so each conditional tail becomes one named body type —
//! the same treatment [`crate::packets::play::player_info`] gets.
//!
//! The number format shared by objectives and scores lives in
//! [`crate::packets::play::containers::NumberFormat`]; this module only
//! decides *when* one travels.

use crate::packets::play::containers::NumberFormat;
use crate::text::Component;
use crate::types::{Decode, Encode, ProtocolString, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, ProtocolVersion};

var_int_enum! {
    /// How a scoreboard renders its numbers.
    pub enum ObjectiveRenderType {
        Integer = 0,
        Hearts = 1,
    }
}

/// Which slot of the screen a scoreboard objective occupies.
///
/// The ids past 2 are team-coloured sidebars, indexed by chat colour; the
/// colour table is the text-formatting one, which is why the ids run from 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreboardSlot {
    List,
    Sidebar,
    BelowName,
    TeamColor(u8),
}

impl ScoreboardSlot {
    pub const FIRST_TEAM_COLOR: i32 = 3;
    pub const LAST_TEAM_COLOR: i32 = 18;

    pub const fn discriminant(self) -> i32 {
        match self {
            Self::List => 0,
            Self::Sidebar => 1,
            Self::BelowName => 2,
            Self::TeamColor(colour) => Self::FIRST_TEAM_COLOR + colour as i32,
        }
    }

    pub fn from_discriminant(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::List),
            1 => Some(Self::Sidebar),
            2 => Some(Self::BelowName),
            3..=Self::LAST_TEAM_COLOR => {
                Some(Self::TeamColor((value - Self::FIRST_TEAM_COLOR) as u8))
            }
            _ => None,
        }
    }
}

impl Decode for ScoreboardSlot {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let value = input.read_var_int()?;
        Self::from_discriminant(value).ok_or(DecodeError::UnknownVariant {
            name: "ScoreboardSlot",
            value,
        })
    }
}

impl Encode for ScoreboardSlot {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.discriminant());
        Ok(())
    }
}

var_int_enum! {
    /// What an objective update asks the client to do.
    ///
    /// Create and update carry the same fields; remove carries none, which is
    /// why [`UpdateObjectivesBody`] holds the rest as options.
    pub enum ObjectiveMode {
        Create = 0,
        Remove = 1,
        Update = 2,
    }
}

fn missing(field: &'static str) -> EncodeError {
    EncodeError::Unsupported {
        field,
        why: "this update mode carries the field and none was given",
    }
}

/// Everything after the objective's name: what to do, and the fields only
/// some modes carry.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateObjectivesBody {
    pub mode: ObjectiveMode,
    /// The displayed text. Present for create and update.
    pub display_name: Option<Component>,
    /// How scores render. Present for create and update.
    pub render_type: Option<ObjectiveRenderType>,
    /// An override for how every score under this objective prints its
    /// number. Present for create and update, and itself optional — the wire
    /// spells "absent" as a false boolean ahead of the format, so this is an
    /// option of an option: outer absent means the mode carried no format at
    /// all, inner absent means "no override".
    pub number_format: Option<Option<NumberFormat>>,
}

impl UpdateObjectivesBody {
    fn carries_display(mode: ObjectiveMode) -> bool {
        matches!(mode, ObjectiveMode::Create | ObjectiveMode::Update)
    }
}

impl Decode for UpdateObjectivesBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let mode = ObjectiveMode::decode(input, version)?;
        if !Self::carries_display(mode) {
            return Ok(Self {
                mode,
                display_name: None,
                render_type: None,
                number_format: None,
            });
        }
        let display_name = Some(Component::decode(input, version)?);
        let render_type = Some(ObjectiveRenderType::decode(input, version)?);
        let number_format = Option::<Option<NumberFormat>>::decode(input, version)?;
        Ok(Self {
            mode,
            display_name,
            render_type,
            number_format,
        })
    }
}

impl Encode for UpdateObjectivesBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.mode.encode(out, version)?;
        if !Self::carries_display(self.mode) {
            return Ok(());
        }
        let display_name = self
            .display_name
            .as_ref()
            .ok_or_else(|| missing("objective display name"))?;
        display_name.encode(out, version)?;
        let render_type = self
            .render_type
            .ok_or_else(|| missing("objective render type"))?;
        render_type.encode(out, version)?;
        self.number_format.encode(out, version)
    }
}

/// Everything after the entity name on a score update.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateScoreBody {
    pub objective: ProtocolString,
    pub score: VarInt,
    /// The score's custom display text, if one was set.
    pub display: Option<Component>,
    /// This score's own number format, overriding the objective's. Absent
    /// means inherit; the inner option distinguishes "no format" from
    /// "inherit" exactly as the wire does.
    pub number_format: Option<Option<NumberFormat>>,
}

impl Decode for UpdateScoreBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            objective: ProtocolString::decode(input, version)?,
            score: VarInt::decode(input, version)?,
            display: Option::<Component>::decode(input, version)?,
            number_format: Option::<Option<NumberFormat>>::decode(input, version)?,
        })
    }
}

impl Encode for UpdateScoreBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.objective.encode(out, version)?;
        self.score.encode(out, version)?;
        self.display.encode(out, version)?;
        self.number_format.encode(out, version)
    }
}

var_int_enum! {
    /// What a team update asks the client to do.
    ///
    /// Create and update-info share the team's descriptive fields; add and
    /// remove-members share the member list; remove-team shares nothing.
    pub enum TeamMethod {
        Create = 0,
        Remove = 1,
        UpdateInfo = 2,
        AddEntities = 3,
        RemoveEntities = 4,
    }
}

/// When members' names are visible, spelled as the wire spells it: one of a
/// small set of words, travelling as a bounded string. It stays a closed
/// enum here because the four words are what every 1.21.1 peer sends; an
/// invented word is refused by name rather than forwarded to game code that
/// cannot match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameTagVisibility {
    Always,
    HideForOtherTeams,
    HideForOwnTeam,
    Never,
}

impl NameTagVisibility {
    pub const ALL: [Self; 4] = [
        Self::Always,
        Self::HideForOtherTeams,
        Self::HideForOwnTeam,
        Self::Never,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::HideForOtherTeams => "hideForOtherTeams",
            Self::HideForOwnTeam => "hideForOwnTeam",
            Self::Never => "never",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "always" => Some(Self::Always),
            "hideForOtherTeams" => Some(Self::HideForOtherTeams),
            "hideForOwnTeam" => Some(Self::HideForOwnTeam),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    fn read<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let text = ProtocolString::decode(input, version)?;
        Self::parse(text.as_str()).ok_or(DecodeError::UnknownField {
            container: "name tag visibility",
            key: text.as_str().to_owned(),
        })
    }

    fn write<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let text = ProtocolString::new(self.as_str()).map_err(|_| EncodeError::StringTooLong {
            limit: crate::types::DEFAULT_STRING_LIMIT,
            actual: self.as_str().len(),
        })?;
        text.encode(out, version)
    }
}

/// How members push against each other, same shape as
/// [`NameTagVisibility`] and string-typed for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionRule {
    Always,
    PushOtherTeams,
    PushOwnTeam,
    Never,
}

impl CollisionRule {
    pub const ALL: [Self; 4] = [
        Self::Always,
        Self::PushOtherTeams,
        Self::PushOwnTeam,
        Self::Never,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::PushOtherTeams => "pushOtherTeams",
            Self::PushOwnTeam => "pushOwnTeam",
            Self::Never => "never",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "always" => Some(Self::Always),
            "pushOtherTeams" => Some(Self::PushOtherTeams),
            "pushOwnTeam" => Some(Self::PushOwnTeam),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    fn read<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let text = ProtocolString::decode(input, version)?;
        Self::parse(text.as_str()).ok_or(DecodeError::UnknownField {
            container: "collision rule",
            key: text.as_str().to_owned(),
        })
    }

    fn write<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let text = ProtocolString::new(self.as_str()).map_err(|_| EncodeError::StringTooLong {
            limit: crate::types::DEFAULT_STRING_LIMIT,
            actual: self.as_str().len(),
        })?;
        text.encode(out, version)
    }
}

/// A team's descriptive fields, shared by create and update-info.
///
/// `friendly_flags` is a bit mask — 0x01 allows friendly fire, 0x02 seeing
/// invisible team-mates — kept raw because it is two independent bits and
/// nothing downstream benefits from an accessor today.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamInfo {
    pub display_name: Component,
    pub friendly_flags: u8,
    pub name_tag_visibility: NameTagVisibility,
    pub collision_rule: CollisionRule,
    /// A colour/formatting id from the text-formatting table, 0..=21.
    pub colour: VarInt,
    pub prefix: Component,
    pub suffix: Component,
}

impl TeamInfo {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            display_name: Component::decode(input, version)?,
            friendly_flags: input.read_u8()?,
            name_tag_visibility: NameTagVisibility::read(input, version)?,
            collision_rule: CollisionRule::read(input, version)?,
            colour: VarInt::decode(input, version)?,
            prefix: Component::decode(input, version)?,
            suffix: Component::decode(input, version)?,
        })
    }

    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.display_name.encode(out, version)?;
        out.write_u8(self.friendly_flags);
        self.name_tag_visibility.write(out, version)?;
        self.collision_rule.write(out, version)?;
        self.colour.encode(out, version)?;
        self.prefix.encode(out, version)?;
        self.suffix.encode(out, version)
    }
}

/// Everything after the team's name: the method and whichever fields it
/// drags behind it.
///
/// Members are names, not ids — players travel as usernames and other
/// entities as UUID strings — so they stay strings rather than being forced
/// into either shape.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamBody {
    pub method: TeamMethod,
    /// Present for create and update-info.
    pub info: Option<TeamInfo>,
    /// Present for create, add-entities and remove-entities.
    pub members: Vec<ProtocolString>,
}

impl TeamBody {
    fn carries_members(method: TeamMethod) -> bool {
        matches!(
            method,
            TeamMethod::Create | TeamMethod::AddEntities | TeamMethod::RemoveEntities
        )
    }
}

impl Decode for TeamBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let method = TeamMethod::decode(input, version)?;
        let info = if matches!(method, TeamMethod::Create | TeamMethod::UpdateInfo) {
            Some(TeamInfo::decode(input, version)?)
        } else {
            None
        };
        let members = if Self::carries_members(method) {
            Vec::<ProtocolString>::decode(input, version)?
        } else {
            Vec::new()
        };
        Ok(Self {
            method,
            info,
            members,
        })
    }
}

impl Encode for TeamBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.method.encode(out, version)?;
        if matches!(self.method, TeamMethod::Create | TeamMethod::UpdateInfo) {
            // A create or update without the fields it promises would leave
            // the client reading members as team info; refusing beats that.
            let info = self.info.as_ref().ok_or(EncodeError::Unsupported {
                field: "team info",
                why: "this method carries the team's fields and none were given",
            })?;
            info.encode(out, version)?;
        }
        if Self::carries_members(self.method) {
            self.members.encode(out, version)?;
        }
        Ok(())
    }
}
