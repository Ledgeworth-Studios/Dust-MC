//! What a stack's `minecraft:enchantments` component says, by name.
//!
//! The component carries `(registry id, level)` pairs, and a registry id means
//! nothing without the registry. Dust does not *serve*
//! `minecraft:enchantment` — decision record 0009 says why, and it has not
//! changed — but it has always held the entry **names** in the order that
//! assigns their ids, in [`crate::synced`], captured off a real server's own
//! sync and checked against it by a test. Forty-two of them.
//!
//! That is the whole of what is needed here. A server that cannot serve a
//! registry can still read a number the client sent against it, and the
//! distinction is exactly decision record 0007's: **the names are a fact about
//! the protocol and the values are Mojang's content.** Nothing in this file is
//! extracted from a jar, because nothing in it is a value.
//!
//! # An id this build cannot name is not an error
//!
//! A data pack may add an enchantment, and then a stack carries an id past the
//! end of the vanilla table. Such a pair is dropped rather than refused: the
//! callers ask "is there silk touch on this", and an enchantment nobody here
//! can name is not silk touch. Refusing the click instead would take a
//! player's pickaxe away over a mod's enchantment that does not change what
//! the block drops.

/// The registry whose ids this component's first VarInt indexes.
const REGISTRY: &str = "minecraft:enchantment";

/// The most pairs read out of one component.
///
/// Vanilla's own limit is far lower — an item cannot carry more distinct
/// enchantments than the registry holds — and the component was already walked
/// and length-checked before it reached here. This exists so a hand-written
/// patch claiming several thousand pairs cannot make a caller allocate for
/// them, and it is above anything a real stack can be.
const MAX_PAIRS: usize = 64;

/// Read `(name, level)` out of a `minecraft:enchantments` payload.
///
/// `bytes` is exactly what
/// `dust_protocol::components::ComponentPatch::component` returns for that
/// name: a count, that many `(id, level)` VarInt pairs, then the
/// show-in-tooltip flag, which nothing here reads.
///
/// The names are `&'static str` out of the registry table, so a caller may
/// hold them without owning a string — which is what lets a loot roll take
/// `&[(&str, u32)]` and allocate nothing on the ordinary path of a stack with
/// no enchantments at all.
#[must_use]
pub fn parse(bytes: &[u8]) -> Vec<(&'static str, u32)> {
    let Some(registry) = crate::synced::by_name(REGISTRY) else {
        return Vec::new();
    };
    let mut at = 0usize;
    let Some(count) = var_int(bytes, &mut at) else {
        return Vec::new();
    };
    if count < 0 {
        return Vec::new();
    }
    let count = (count as usize).min(MAX_PAIRS);
    let mut out = Vec::new();
    for _ in 0..count {
        let (Some(id), Some(level)) = (var_int(bytes, &mut at), var_int(bytes, &mut at)) else {
            break;
        };
        if id < 0 || level <= 0 {
            continue;
        }
        // An id past the end of the table, or a level of zero, is dropped and
        // not guessed at. See this module's header.
        if let Some(name) = registry.entries.get(id as usize) {
            out.push((*name, level as u32));
        }
    }
    out
}

/// The level of `enchantment` in a parsed list, zero for absent.
///
/// Here rather than at each caller because there are three of them and they
/// are in three crates: the loot roll asks for silk touch and fortune, and the
/// break timer asks for efficiency.
#[must_use]
pub fn level(enchantments: &[(&str, u32)], enchantment: &str) -> u32 {
    enchantments
        .iter()
        .find(|(name, _)| *name == enchantment)
        .map_or(0, |(_, level)| *level)
}

/// One VarInt, advancing `at`. `None` at the end of the buffer or past five
/// bytes, which is a VarInt no 32-bit value needs.
fn var_int(bytes: &[u8], at: &mut usize) -> Option<i32> {
    let mut value: u32 = 0;
    for shift in 0..5 {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u32::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return Some(value as i32);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(mut value: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            if value < 0x80 {
                out.push(value as u8);
                return out;
            }
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
    }

    fn payload(pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut out = var(pairs.len() as u32);
        for (id, level) in pairs {
            out.extend(var(*id));
            out.extend(var(*level));
        }
        out.push(1);
        out
    }

    fn id(name: &str) -> u32 {
        crate::synced::by_name(REGISTRY)
            .expect("the enchantment registry")
            .id_of(name)
            .unwrap_or_else(|| panic!("{name} is in the table")) as u32
    }

    #[test]
    fn a_silk_touch_pickaxe_reads_back_as_silk_touch() {
        let bytes = payload(&[(id("minecraft:silk_touch"), 1)]);
        assert_eq!(parse(&bytes), vec![("minecraft:silk_touch", 1)]);
        assert_eq!(level(&parse(&bytes), "minecraft:silk_touch"), 1);
        assert_eq!(level(&parse(&bytes), "minecraft:fortune"), 0);
    }

    /// The three this project reads, on one stack, out of order. Ids come from
    /// the table rather than being written down here — a test that hardcoded
    /// `18` would pass on a build whose table had shifted underneath it, which
    /// is the failure the table's own capture test exists to catch.
    #[test]
    fn three_enchantments_on_one_stack() {
        let bytes = payload(&[
            (id("minecraft:fortune"), 3),
            (id("minecraft:efficiency"), 5),
            (id("minecraft:unbreaking"), 2),
        ]);
        let parsed = parse(&bytes);
        assert_eq!(parsed.len(), 3);
        assert_eq!(level(&parsed, "minecraft:fortune"), 3);
        assert_eq!(level(&parsed, "minecraft:efficiency"), 5);
        assert_eq!(level(&parsed, "minecraft:silk_touch"), 0);
    }

    /// An id a data pack added, past the end of the vanilla table. Dropped,
    /// and the pairs around it are kept — a stack with a modded enchantment
    /// and silk touch still has silk touch.
    #[test]
    fn an_unnameable_id_is_dropped_and_its_neighbours_are_not() {
        let bytes = payload(&[(9_000, 1), (id("minecraft:silk_touch"), 1)]);
        assert_eq!(parse(&bytes), vec![("minecraft:silk_touch", 1)]);
    }

    #[test]
    fn nothing_and_nonsense_read_as_no_enchantments() {
        assert!(parse(&[]).is_empty());
        assert!(parse(&payload(&[])).is_empty());
        // A count of two with one pair behind it. The pair that is there is
        // kept and the walk stops, rather than reading the tooltip flag as an
        // enchantment id.
        let mut truncated = var(2);
        truncated.extend(var(id("minecraft:silk_touch")));
        truncated.extend(var(1));
        assert_eq!(parse(&truncated), vec![("minecraft:silk_touch", 1)]);
    }

    /// Level zero is not an enchantment. Vanilla will not write one and a
    /// hand-made patch can, and a fortune of zero multiplying a drop by one is
    /// a subtler wrong answer than dropping it.
    #[test]
    fn a_level_of_zero_is_not_an_enchantment() {
        assert!(parse(&payload(&[(id("minecraft:fortune"), 0)])).is_empty());
    }
}
