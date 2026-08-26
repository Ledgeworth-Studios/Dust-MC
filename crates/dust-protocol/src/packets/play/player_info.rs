//! The player info update: one bitmask, six optional layouts, no way around
//! them.
//!
//! # Why this file is not `packet_group` fields
//!
//! The update packet opens with a byte of action flags and then, per player,
//! writes exactly the fields the set bits select — in bit order, with nothing
//! marking which are present. A field list cannot express that, because no
//! field can see the flags that came before it. So the whole body after the id
//! is one type here, and its decode reads the flags first and passes them down
//! to every entry. That is also why an entry's "absent" fields are plain
//! `Option`s **without** the wire's boolean prefix: presence is decided by the
//! shared bitmask, not by anything inside the entry.
//!
//! Where a selected field is itself nullable on the wire — chat session data,
//! display name — there are two levels: did the action run, and did it carry a
//! value? Those spell as `Option<Option<T>>`, outer for the action and inner
//! for the boolean-prefixed value. It is ugly and it is exact; renaming it
//! into something prettier loses which layer is which.

use crate::packets::common::ProfileProperty;
use crate::text::Component;
use crate::types::{BoundedString, Decode, Encode, Uuid, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{wire_struct, ProtocolVersion};

/// The action bits, in the order their fields appear per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerInfoActions(pub u8);

impl PlayerInfoActions {
    pub const ADD_PLAYER: u8 = 0x01;
    pub const INITIALIZE_CHAT: u8 = 0x02;
    pub const UPDATE_GAME_MODE: u8 = 0x04;
    pub const UPDATE_LISTED: u8 = 0x08;
    pub const UPDATE_LATENCY: u8 = 0x10;
    pub const UPDATE_DISPLAY_NAME: u8 = 0x20;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    /// Every bit this protocol version defines.
    ///
    /// Encoding refuses anything outside the mask rather than writing it: a
    /// client that receives an action it cannot parse has no idea where the
    /// entries end, and one stray bit desynchronises the whole tab list.
    pub const ALL: u8 = Self::ADD_PLAYER
        | Self::INITIALIZE_CHAT
        | Self::UPDATE_GAME_MODE
        | Self::UPDATE_LISTED
        | Self::UPDATE_LATENCY
        | Self::UPDATE_DISPLAY_NAME;

    pub fn is_known(self) -> bool {
        self.0 & !Self::ALL == 0
    }
}

/// Everything after the packet id: the flags and the entries they select.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoBody {
    pub actions: PlayerInfoActions,
    pub entries: Vec<PlayerInfoEntry>,
}

impl Decode for PlayerInfoBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let actions = PlayerInfoActions(input.read_u8()?);
        if !actions.is_known() {
            return Err(DecodeError::UnknownVariant {
                name: "PlayerInfoActions",
                value: i32::from(actions.0),
            });
        }
        let count = VarInt::decode(input, version)?;
        let count = usize::try_from(count.0).map_err(|_| DecodeError::NegativeLength {
            field: "player info entries",
            value: count.0,
        })?;
        let mut entries = Vec::with_capacity(count.min(input.remaining()));
        for _ in 0..count {
            entries.push(PlayerInfoEntry::decode_with(&actions, input, version)?);
        }
        Ok(Self { actions, entries })
    }
}

impl Encode for PlayerInfoBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        if !self.actions.is_known() {
            return Err(EncodeError::Unsupported {
                field: "player info update",
                why: "the actions byte carries bits this protocol does not define",
            });
        }
        out.write_u8(self.actions.0);
        VarInt(self.entries.len() as i32).encode(out, version)?;
        for entry in &self.entries {
            entry.encode_with(&self.actions, out, version)?;
        }
        Ok(())
    }
}

wire_struct! {
    /// The add-player half of an entry: who this is, as the login saw them.
    pub struct ProfileAddition {
        name: BoundedString<16>,
        properties: Vec<ProfileProperty>,
    }
}

wire_struct! {
    /// The signing session a client registers, when it has one to register.
    ///
    /// Carried opaquely like every other signing artifact; see
    /// [`super::chat`] for where verification will plug in.
    pub struct ChatSession {
        session_id: Uuid,
        expires_at: i64,
        public_key: crate::types::PrefixedBytes<512>,
        key_signature: crate::types::PrefixedBytes<4096>,
    }
}

/// One player's worth of the enabled actions.
///
/// The `Option` fields have no boolean prefix of their own; see the module
/// docs. The double options are the nullable-within-action cases.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoEntry {
    pub uuid: Uuid,
    /// Present iff [`PlayerInfoActions::ADD_PLAYER`] is set.
    pub profile: Option<ProfileAddition>,
    /// Outer iff [`PlayerInfoActions::INITIALIZE_CHAT`] is set; inner is the
    /// wire's own "has signature data" boolean.
    pub chat_session: Option<Option<ChatSession>>,
    /// Present iff [`PlayerInfoActions::UPDATE_GAME_MODE`] is set. Raw id, so
    /// an unknown mode from a future version survives the trip instead of
    /// failing the whole tab list; resolve through
    /// [`super::Gamemode::from_discriminant`].
    pub game_mode: Option<VarInt>,
    /// Present iff [`PlayerInfoActions::UPDATE_LISTED`] is set.
    pub listed: Option<bool>,
    /// Present iff [`PlayerInfoActions::UPDATE_LATENCY`] is set.
    pub latency: Option<VarInt>,
    /// Outer iff [`PlayerInfoActions::UPDATE_DISPLAY_NAME`] is set; inner is
    /// the boolean-prefixed component.
    pub display_name: Option<Option<Component>>,
}

impl PlayerInfoEntry {
    fn decode_with<R: WireRead + ?Sized>(
        actions: &PlayerInfoActions,
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let uuid = Uuid::decode(input, version)?;
        let profile = if actions.has(PlayerInfoActions::ADD_PLAYER) {
            Some(ProfileAddition::decode(input, version)?)
        } else {
            None
        };
        let chat_session = if actions.has(PlayerInfoActions::INITIALIZE_CHAT) {
            Some(Option::<ChatSession>::decode(input, version)?)
        } else {
            None
        };
        let game_mode = if actions.has(PlayerInfoActions::UPDATE_GAME_MODE) {
            Some(VarInt::decode(input, version)?)
        } else {
            None
        };
        let listed = if actions.has(PlayerInfoActions::UPDATE_LISTED) {
            Some(input.read_bool()?)
        } else {
            None
        };
        let latency = if actions.has(PlayerInfoActions::UPDATE_LATENCY) {
            Some(VarInt::decode(input, version)?)
        } else {
            None
        };
        let display_name = if actions.has(PlayerInfoActions::UPDATE_DISPLAY_NAME) {
            Some(Option::<Component>::decode(input, version)?)
        } else {
            None
        };
        Ok(Self {
            uuid,
            profile,
            chat_session,
            game_mode,
            listed,
            latency,
            display_name,
        })
    }

    fn encode_with<W: WireWrite + ?Sized>(
        &self,
        actions: &PlayerInfoActions,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.uuid.encode(out, version)?;
        // Every branch below has the same failure shape: a bit is set and the
        // entry says nothing for it. Encoding defaults silently would put a
        // wrong tab-list entry on a client; refusing names the gap.
        let missing = |field: &'static str| EncodeError::Unsupported {
            field,
            why: "this action's bit is set and the entry carries no data for it",
        };
        if actions.has(PlayerInfoActions::ADD_PLAYER) {
            self.profile
                .as_ref()
                .ok_or_else(|| missing("player info update: add player"))?
                .encode(out, version)?;
        }
        if actions.has(PlayerInfoActions::INITIALIZE_CHAT) {
            match &self.chat_session {
                Some(session) => session.encode(out, version)?,
                None => return Err(missing("player info update: initialize chat")),
            }
        }
        if actions.has(PlayerInfoActions::UPDATE_GAME_MODE) {
            self.game_mode
                .as_ref()
                .ok_or_else(|| missing("player info update: game mode"))?
                .encode(out, version)?;
        }
        if actions.has(PlayerInfoActions::UPDATE_LISTED) {
            out.write_bool(
                self.listed
                    .ok_or_else(|| missing("player info update: listed"))?,
            );
        }
        if actions.has(PlayerInfoActions::UPDATE_LATENCY) {
            self.latency
                .as_ref()
                .ok_or_else(|| missing("player info update: latency"))?
                .encode(out, version)?;
        }
        if actions.has(PlayerInfoActions::UPDATE_DISPLAY_NAME) {
            match &self.display_name {
                Some(name) => name.encode(out, version)?,
                None => return Err(missing("player info update: display name")),
            }
        }
        Ok(())
    }
}
