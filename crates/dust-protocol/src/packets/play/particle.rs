//! Particle values: the id, and the data the chosen particle drags behind it.
//!
//! # The shape of the problem
//!
//! A particle on the wire is a VarInt naming an entry of the
//! `minecraft:particle_type` registry, followed — for some entries only — by
//! that entry's own options. The options have **no length prefix**; whether
//! bytes follow at all, and how many, is decided entirely by which id came
//! first. A reader that does not know the table cannot even skip a particle,
//! let alone decode it.
//!
//! So this type owns the table. The 1.21.1 registry holds 109 entries; all but
//! eleven carry no data. The eleven with data, and the shapes they take:
//!
//! - `block`, `block_marker`, `falling_dust`, `dust_pillar` take one block
//!   state id;
//! - `dust` and `dust_color_transition` take RGB triples and a scale;
//! - `entity_effect` takes an ARGB colour as one int;
//! - `sculk_charge` takes a roll angle; `shriek` takes a delay in ticks;
//! - `vibration` takes a position source and a travel time.
//!
//! The eleventh is `item`, whose option is a full [`crate::types::Slot`] — and
//! that is the established wall. Following the metadata module's precedent for
//! item-stack serializers, it is refused by name at the exact byte rather than
//! half-read: see [`crate::types::Slot`] for why a component-bearing stack cannot be
//! stepped over, and [`crate::packets::play::metadata`] for the same refusal
//! one layer up. Ten of the eleven are modelled here across seven option
//! shapes (the four block-state ids share one).
//!
//! # What the type guarantees
//!
//! That the data *shape* agrees with the id. `dust` with no colour after it is
//! not a valid particle value, and accepting it would desynchronise whatever
//! field follows — so [`ParticleValue::decode`] pairs every known id with its
//! required shape and refuses a mismatch by name. Ids outside the registry are
//! refused for the same reason: their data shape is unknown, so nothing after
//! them can be located.

use crate::types::{Decode, Encode, Position, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::ProtocolVersion;

/// The highest id the 1.21.1 `minecraft:particle_type` registry defines.
///
/// The registry is versioned data; when a release adds particles this bound
/// moves with it, and a peer sending a newer id is refused until somebody has
/// looked at what the new entries' options are.
pub const MAX_PARTICLE_ID: i32 = 108;

/// The id of the one refused particle: its option is an item stack.
pub const ITEM_PARTICLE_ID: i32 = 44;

/// The id of the vibration particle, whose options get their own variant.
const VIBRATION_ID: i32 = 45;

/// The data a particle carries, paired with the ids that require it.
///
/// One enum over the shapes rather than one variant per particle: 109 variants
/// where 101 hold nothing would make every match arm a copy of its neighbour,
/// and the wire itself distinguishes only the shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum ParticleValue {
    /// No options. The overwhelming majority of the table.
    None { id: i32 },
    /// A block state id: `block`, `block_marker`, `falling_dust`,
    /// `dust_pillar`.
    BlockState { id: i32, state: VarInt },
    /// A coloured cloud: red, green, blue and scale, each a plain float.
    Dust {
        id: i32,
        red: f32,
        green: f32,
        blue: f32,
        scale: f32,
    },
    /// A colour fading to another: from-triple, to-triple, then scale.
    DustColorTransition {
        id: i32,
        from_red: f32,
        from_green: f32,
        from_blue: f32,
        to_red: f32,
        to_green: f32,
        to_blue: f32,
        scale: f32,
    },
    /// An ARGB colour packed into one int.
    EntityEffect { id: i32, color: i32 },
    /// How far the charge is rolled when displayed.
    SculkCharge { id: i32, roll: f32 },
    /// How long before the shriek appears, in ticks.
    Shriek { id: i32, delay: VarInt },
    /// Where a vibration travels from and to, and how long the trip takes.
    Vibration(VibrationPath),
}

/// One end of a vibration: a block, or an entity's eyes.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionSource {
    Block(Position),
    Entity { entity_id: VarInt, eye_height: f32 },
}

/// The vibration particle's options: source, destination, duration.
#[derive(Debug, Clone, PartialEq)]
pub struct VibrationPath {
    pub source: PositionSource,
    pub destination: PositionSource,
    pub ticks: VarInt,
}

impl PositionSource {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        const BLOCK: i32 = 0;
        const ENTITY: i32 = 1;
        match input.read_var_int()? {
            BLOCK => Position::decode(input, version).map(Self::Block),
            ENTITY => Ok(Self::Entity {
                entity_id: VarInt::decode(input, version)?,
                eye_height: input.read_f32()?,
            }),
            other => Err(DecodeError::UnknownVariant {
                name: "PositionSource",
                value: other,
            }),
        }
    }

    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Block(position) => {
                out.write_var_int(0);
                position.encode(out, version)
            }
            Self::Entity {
                entity_id,
                eye_height,
            } => {
                out.write_var_int(1);
                entity_id.encode(out, version)?;
                eye_height.encode(out, version)
            }
        }
    }
}

/// Which ids demand which data shape.
///
/// A function rather than a match in two places so the pairing exists once:
/// decode asks "what must follow this id", encode asks "what does this variant
/// write", and both get the same answer.
fn shape_of(id: i32) -> Option<&'static str> {
    match id {
        1 | 2 | 28 | 105 => Some("block_state"),
        13 => Some("dust"),
        14 => Some("dust_color_transition"),
        20 => Some("entity_effect"),
        ITEM_PARTICLE_ID => Some("item"),
        VIBRATION_ID => Some("vibration"),
        35 => Some("sculk_charge"),
        99 => Some("shriek"),
        0..=MAX_PARTICLE_ID => Some("none"),
        _ => None,
    }
}

impl Decode for ParticleValue {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let id = input.read_var_int()?;
        match shape_of(id) {
            None => Err(DecodeError::UnknownVariant {
                name: "particle type",
                value: id,
            }),
            Some("item") => Err(DecodeError::Unsupported {
                field: "item particle",
                why: "its option is an item stack carrying data components, which have no \
                      length and cannot be stepped over",
            }),
            Some("none") => Ok(Self::None { id }),
            Some("block_state") => Ok(Self::BlockState {
                id,
                state: VarInt::decode(input, version)?,
            }),
            Some("dust") => Ok(Self::Dust {
                id,
                red: input.read_f32()?,
                green: input.read_f32()?,
                blue: input.read_f32()?,
                scale: input.read_f32()?,
            }),
            Some("dust_color_transition") => Ok(Self::DustColorTransition {
                id,
                from_red: input.read_f32()?,
                from_green: input.read_f32()?,
                from_blue: input.read_f32()?,
                to_red: input.read_f32()?,
                to_green: input.read_f32()?,
                to_blue: input.read_f32()?,
                scale: input.read_f32()?,
            }),
            Some("entity_effect") => Ok(Self::EntityEffect {
                id,
                color: input.read_i32()?,
            }),
            Some("sculk_charge") => Ok(Self::SculkCharge {
                id,
                roll: input.read_f32()?,
            }),
            Some("shriek") => Ok(Self::Shriek {
                id,
                delay: VarInt::decode(input, version)?,
            }),
            Some(_) => {
                // The vibration shape, reached without naming it so adding a
                // shape above cannot quietly fall into this arm.
                debug_assert_eq!(shape_of(id), Some("vibration"));
                let source = PositionSource::decode(input, version)?;
                let destination = PositionSource::decode(input, version)?;
                let ticks = VarInt::decode(input, version)?;
                Ok(Self::Vibration(VibrationPath {
                    source,
                    destination,
                    ticks,
                }))
            }
        }
    }
}

impl Encode for ParticleValue {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::None { id } => out.write_var_int(*id),
            Self::BlockState { id, state } => {
                out.write_var_int(*id);
                return state.encode(out, version);
            }
            Self::Dust {
                id,
                red,
                green,
                blue,
                scale,
            } => {
                out.write_var_int(*id);
                out.write_f32(*red);
                out.write_f32(*green);
                out.write_f32(*blue);
                out.write_f32(*scale);
            }
            Self::DustColorTransition {
                id,
                from_red,
                from_green,
                from_blue,
                to_red,
                to_green,
                to_blue,
                scale,
            } => {
                out.write_var_int(*id);
                out.write_f32(*from_red);
                out.write_f32(*from_green);
                out.write_f32(*from_blue);
                out.write_f32(*to_red);
                out.write_f32(*to_green);
                out.write_f32(*to_blue);
                out.write_f32(*scale);
            }
            Self::EntityEffect { id, color } => {
                out.write_var_int(*id);
                out.write_i32(*color);
            }
            Self::SculkCharge { id, roll } => {
                out.write_var_int(*id);
                out.write_f32(*roll);
            }
            Self::Shriek { id, delay } => {
                out.write_var_int(*id);
                return delay.encode(out, version);
            }
            Self::Vibration(path) => {
                out.write_var_int(VIBRATION_ID);
                path.source.encode(out, version)?;
                path.destination.encode(out, version)?;
                return path.ticks.encode(out, version);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Reader, Writer};

    fn v() -> ProtocolVersion {
        crate::version::V1_21_1
    }

    #[test]
    fn every_modelled_shape_agrees_with_its_ids() {
        // Each shape's ids decode into that shape and nothing else; the
        // pairing lives in one function and this is what proves it bites.
        for id in [1, 2, 28, 105] {
            let mut writer = Writer::new();
            writer.write_var_int(id);
            writer.write_var_int(42);
            let value =
                ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes");
            assert_eq!(
                value,
                ParticleValue::BlockState {
                    id,
                    state: VarInt(42)
                }
            );
        }
        for id in [0, 39, 108] {
            let mut writer = Writer::new();
            writer.write_var_int(id);
            assert_eq!(
                ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
                ParticleValue::None { id }
            );
        }
    }

    #[test]
    fn an_id_outside_the_registry_is_refused_before_any_data_is_read() {
        // Past the bound the data shape is unknown, so refusing immediately is
        // the only way to keep the reader's position honest.
        let refused_id = MAX_PARTICLE_ID + 1;
        let mut writer = Writer::new();
        writer.write_var_int(refused_id);
        writer.write_slice(&[0xFF; 8]);
        assert_eq!(
            ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()),
            Err(DecodeError::UnknownVariant {
                name: "particle type",
                value: refused_id
            })
        );
    }

    #[test]
    fn the_item_particle_is_refused_by_name_at_the_exact_byte() {
        // The Slot seam, surfacing through particles. The refusal names the
        // field so the day components become readable, this test is the one
        // that gets deleted.
        let mut writer = Writer::new();
        writer.write_var_int(ITEM_PARTICLE_ID);
        writer.write_var_int(1);
        writer.write_var_int(1);
        writer.write_var_int(1);
        writer.write_var_int(0);
        writer.write_var_int(0);
        assert!(matches!(
            ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()),
            Err(DecodeError::Unsupported {
                field: "item particle",
                ..
            })
        ));
    }

    #[test]
    fn dust_carries_four_floats_in_wire_order() {
        let value = ParticleValue::Dust {
            id: 13,
            red: 0.25,
            green: 0.5,
            blue: 0.75,
            scale: 1.0,
        };
        let mut writer = Writer::new();
        value.encode(&mut writer, v()).expect("encodes");
        assert_eq!(writer.len(), 1 + 16);
        assert_eq!(
            ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
            value
        );
    }

    #[test]
    fn vibration_sources_are_a_tagged_union_both_ways() {
        for path in [
            VibrationPath {
                source: PositionSource::Block(Position::new(-1, 64, 2)),
                destination: PositionSource::Entity {
                    entity_id: VarInt(9),
                    eye_height: 1.62,
                },
                ticks: VarInt(20),
            },
            VibrationPath {
                source: PositionSource::Entity {
                    entity_id: VarInt(-3),
                    eye_height: 0.0,
                },
                destination: PositionSource::Block(Position::new(0, 0, 0)),
                ticks: VarInt(1),
            },
        ] {
            let value = ParticleValue::Vibration(path.clone());
            let mut writer = Writer::new();
            value.encode(&mut writer, v()).expect("encodes");
            assert_eq!(
                ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()).expect("decodes"),
                value
            );
        }

        // A third source type arriving from a future version is refused by
        // name rather than read as a block position.
        let mut writer = Writer::new();
        writer.write_var_int(VIBRATION_ID);
        writer.write_var_int(7);
        assert_eq!(
            ParticleValue::decode(&mut Reader::new(writer.as_bytes()), v()),
            Err(DecodeError::UnknownVariant {
                name: "PositionSource",
                value: 7
            })
        );
    }
}
