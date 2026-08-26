//! Entity attributes: the numbers a mob's behaviour hangs off.
//!
//! # Why modifiers name themselves with an identifier
//!
//! Before 1.21 an attribute modifier carried its id as a UUID pair, and half
//! the ecosystem still calls the field that. On this version the wire spells
//! it as an [`Identifier`] — vanilla switched when modifiers became
//! data-driven — so that is what travels here. A decoder written against the
//! old layout reads the identifier's length prefix as the UUID's high half,
//! produces a plausible-looking number, and loses every field after it;
//! which is exactly why this type exists rather than inline fields in the
//! packet definition.

use crate::types::{Decode, Encode, Identifier, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::ProtocolVersion;

/// One adjustment to one attribute: who it is, how much, how it applies.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeModifier {
    pub id: Identifier,
    pub amount: f64,
    /// 0: add to the base, 1: multiply the base, 2: multiply the sum.
    ///
    /// Kept as a byte because the protocol does; the meanings live where
    /// gameplay applies them, not on the wire.
    pub operation: u8,
}

impl Decode for AttributeModifier {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            id: Identifier::decode(input, version)?,
            amount: input.read_f64()?,
            operation: input.read_u8()?,
        })
    }
}

impl Encode for AttributeModifier {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.id.encode(out, version)?;
        out.write_f64(self.amount);
        out.write_u8(self.operation);
        Ok(())
    }
}

/// One attribute's value: which attribute, its base, and everything riding
/// on it.
///
/// The attribute id is a registry row (`minecraft:attribute`); resolving
/// `minecraft:generic.movement_speed` to a number is dust-registry's job,
/// the same split as every other raw registry id this crate carries.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeProperty {
    pub attribute_id: VarInt,
    /// The value before modifiers apply.
    pub base: f64,
    pub modifiers: Vec<AttributeModifier>,
}

impl Decode for AttributeProperty {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let attribute_id = VarInt::decode(input, version)?;
        let base = input.read_f64()?;
        let modifiers = Vec::<AttributeModifier>::decode(input, version)?;
        Ok(Self {
            attribute_id,
            base,
            modifiers,
        })
    }
}

impl Encode for AttributeProperty {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.attribute_id.encode(out, version)?;
        out.write_f64(self.base);
        self.modifiers.encode(out, version)
    }
}
