//! The map update: icons and a colour patch, each optional, one after the
//! other.
//!
//! # Why this is a body type and not packet fields
//!
//! Both halves of the map update are value-dependent: the icon list exists
//! only if a boolean says so, and the colour patch's trailing fields exist
//! only if its column count is non-zero. No field can see the field before it,
//! so the whole tail is one type here — the same treatment
//! [`crate::packets::play::player_info`] gets for the same reason.

use crate::text::Component;
use crate::types::{BoundedString, Decode, Encode};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, wire_struct, ProtocolVersion};

/// A bounded page of a written book, shared by the book packets.
pub type BookPage = BoundedString<8192>;

var_int_enum! {
    /// Which marker a map icon shows.
    ///
    /// The ids are the protocol's own table of arrows, crosses, structure
    /// markers and the banner colours; 1.21 lengthened that table with the
    /// village and trial-chamber explorer-map markers a cartographer can sell,
    /// which is why the tail of this list is longer than pre-1.21
    /// documentation says. A newer peer's id is refused rather than drawn as
    /// something else.
    pub enum MapIconKind {
        WhiteArrow = 0,
        GreenArrow = 1,
        RedArrow = 2,
        BlueArrow = 3,
        WhiteCross = 4,
        RedPointer = 5,
        WhiteCircle = 6,
        SmallWhiteCircle = 7,
        Mansion = 8,
        Monument = 9,
        WhiteBanner = 10,
        OrangeBanner = 11,
        MagentaBanner = 12,
        LightBlueBanner = 13,
        YellowBanner = 14,
        LimeBanner = 15,
        PinkBanner = 16,
        GrayBanner = 17,
        LightGrayBanner = 18,
        CyanBanner = 19,
        PurpleBanner = 20,
        BlueBanner = 21,
        BrownBanner = 22,
        GreenBanner = 23,
        RedBanner = 24,
        BlackBanner = 25,
        TreasureMarker = 26,
        VillageDesert = 27,
        VillagePlains = 28,
        VillageSavanna = 29,
        VillageSnowy = 30,
        VillageTaiga = 31,
        JungleTemple = 32,
        SwampHut = 33,
        TrialChambers = 34,
    }
}

wire_struct! {
    /// One marker on the map: what it is, where it sits in map coordinates,
    /// which way it faces, and an optional label.
    ///
    /// The label travels as a string here — not a component — because that is
    /// how this field spells it; everything else about it behaves like one.
    pub struct MapIcon {
        kind: MapIconKind,
        x: i8,
        z: i8,
        /// Facing in steps of 22.5°, zero being straight up.
        direction: i8,
        display_name: Option<Component>,
    }
}

/// The rectangular patch of pixels an update repaints.
///
/// `columns` gates everything behind it: a zero-column patch updates nothing,
/// carries no rows and no data, and is how a server says "icons only".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapPatch {
    pub columns: u8,
    pub rows: u8,
    /// Offset of the westernmost column.
    pub x: u8,
    /// Offset of the northernmost row.
    pub z: u8,
    pub data: Vec<u8>,
}

impl Decode for MapPatch {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let columns = input.read_u8()?;
        if columns == 0 {
            return Ok(Self {
                columns,
                rows: 0,
                x: 0,
                z: 0,
                data: Vec::new(),
            });
        }
        let rows = input.read_u8()?;
        let x = input.read_u8()?;
        let z = input.read_u8()?;
        let data = Vec::<u8>::decode(input, version)?;
        Ok(Self {
            columns,
            rows,
            x,
            z,
            data,
        })
    }
}

impl Encode for MapPatch {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.columns);
        if self.columns == 0 {
            return Ok(());
        }
        out.write_u8(self.rows);
        out.write_u8(self.x);
        out.write_u8(self.z);
        self.data.encode(out, version)
    }
}

/// Everything after the map id: scale, lock state, icons, patch.
#[derive(Debug, Clone, PartialEq)]
pub struct MapDataBody {
    /// Zoom level, 0 (block per pixel) through 4 (sixteen).
    pub scale: i8,
    pub locked: bool,
    pub icons: Option<Vec<MapIcon>>,
    pub patch: MapPatch,
}

impl Decode for MapDataBody {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let scale = input.read_i8()?;
        let locked = input.read_bool()?;
        let has_icons = input.read_bool()?;
        let icons = if has_icons {
            Some(Vec::<MapIcon>::decode(input, version)?)
        } else {
            None
        };
        let patch = MapPatch::decode(input, version)?;
        Ok(Self {
            scale,
            locked,
            icons,
            patch,
        })
    }
}

impl Encode for MapDataBody {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_i8(self.scale);
        out.write_bool(self.locked);
        match &self.icons {
            Some(icons) => {
                out.write_bool(true);
                icons.encode(out, version)?;
            }
            None => out.write_bool(false),
        }
        self.patch.encode(out, version)
    }
}

impl MapIcon {
    /// The icon kinds' count, for tests.
    pub const KIND_COUNT: usize = 35;
}
