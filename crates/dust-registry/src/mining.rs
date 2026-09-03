//! Which tool mines which block, and how fast.
//!
//! Since 1.20.5 that is one data component and nothing else. An item either
//! carries `minecraft:tool` — an ordered list of rules, each naming a set of
//! blocks and optionally a mining speed and optionally a verdict on whether
//! the block's drops are correct — or it does not, in which case it mines
//! everything at 1.0 and correctly drops nothing. There is no `PickaxeItem`
//! left to ask and no tier ladder written down anywhere: a wooden pickaxe is
//! refused diamond ore by a rule naming `#minecraft:incorrect_for_wooden_tool`,
//! and that rule is the only place the refusal exists.
//!
//! # Where the answer comes from
//!
//! [`crate::items`] already holds it. The component arrives in Mojang's own
//! item report and is generated into `ITEM_COMPONENTS` with its rules and its
//! tag names intact, so this module reads the crate rather than asking the jar
//! for a second copy. That matters more than it saves: two extractions of one
//! relation are two answers that can disagree, and nothing would say which was
//! right.
//!
//! Asking the *jar* to apply the rules was tried and does not work. A bare
//! `Bootstrap` leaves every block tag empty, so `Tool.getMiningSpeed` answers
//! for nine item-and-block pairs in the whole game — the two rules that name
//! their blocks outright — and the default for everything else. The tags are
//! in this crate; the rules meet them here.
//!
//! # Why it is a table and not a walk
//!
//! `#minecraft:mineable/axe` references fifteen other tags and some of those
//! reference more, so answering "is this block in that set" honestly means
//! walking a graph. That walk does not change between two breaks, and a block
//! break is on the interaction path, so it is done once: 33 tools times 1,060
//! blocks of one byte each, twice, built the first time anybody asks and read
//! by index afterwards.
//!
//! The byte is **which rule matched**, not the answer, so the two questions
//! stay separable — a rule can set a speed without granting the drops, which
//! is how shears cut leaves fast and still hand a player nothing they would
//! not have got by hand.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::items::ComponentValue;
use crate::tags::{self, TagRegistry};
use crate::{Block, Item};

/// The component an item carries when it is a tool.
const TOOL: &str = "minecraft:tool";

/// What a tool mines a block it has no rule for at.
///
/// Minecraft's own default for the component's `default_mining_speed`, which
/// no vanilla tool overrides — and which is also what an item with no tool
/// component at all answers, so a bare hand and a bowl are the same question.
const DEFAULT_SPEED: f32 = 1.0;

/// How much slower a block goes when the tool is not the right one.
///
/// Minecraft divides the progress by 100 rather than by 30. It is here rather
/// than in the caller because it is the other half of the same rule: the two
/// numbers are what "correct tool" *means* to a player, one of them being how
/// long it takes and the other being whether anything comes out.
pub const WRONG_TOOL_DIVISOR: f32 = 100.0;

/// And by 30 when it is.
pub const RIGHT_TOOL_DIVISOR: f32 = 30.0;

/// Every tool's rules, resolved against every block.
#[derive(Debug)]
struct Mining {
    /// One entry per item that carries a tool component, by item id.
    tools: BTreeMap<u32, Tool>,
}

#[derive(Debug)]
struct Tool {
    /// The speeds this tool's rules name, in rule order.
    speeds: Vec<Option<f32>>,
    /// The verdicts this tool's rules name, in rule order.
    verdicts: Vec<Option<bool>>,
    /// Per block, the first rule with a speed whose blocks hold it, plus one.
    /// Zero is "no rule names this block", which is the default speed.
    speed_of: Box<[u8]>,
    /// Per block, the first rule with a verdict whose blocks hold it, plus
    /// one. Zero is "no rule says", which Minecraft reads as *not* correct.
    verdict_of: Box<[u8]>,
}

/// Built once. Every input is generated static data, so there is nothing for a
/// second build to see differently and nothing for a caller to configure.
static TABLE: OnceLock<Mining> = OnceLock::new();

fn table() -> &'static Mining {
    TABLE.get_or_init(Mining::build)
}

/// How fast `item` mines `block`, as the multiplier Minecraft calls a mining
/// speed.
///
/// `None` is a bare hand, and answers [`DEFAULT_SPEED`] — the same as a bowl,
/// because an item with no tool component and no item at all are the same
/// question with the same answer.
#[must_use]
pub fn speed(item: Option<Item>, block: Block) -> f32 {
    let Some(tool) = item.and_then(|item| table().tools.get(&item.protocol_id())) else {
        return DEFAULT_SPEED;
    };
    let index = tool.speed_of[block.protocol_id() as usize];
    if index == 0 {
        return DEFAULT_SPEED;
    }
    tool.speeds[index as usize - 1].unwrap_or(DEFAULT_SPEED)
}

/// Whether `item` is a tool that makes `block` yield its drops.
///
/// **Not the same question as whether the block needs one.** This says the
/// tool is right; [`crate::BlockConstants`]'s `requires_tool` column says
/// whether the block cares. Dirt answers `false` here to a bare hand and
/// yields dirt anyway.
#[must_use]
pub fn correct_for_drops(item: Option<Item>, block: Block) -> bool {
    let Some(tool) = item.and_then(|item| table().tools.get(&item.protocol_id())) else {
        return false;
    };
    let index = tool.verdict_of[block.protocol_id() as usize];
    if index == 0 {
        return false;
    }
    tool.verdicts[index as usize - 1].unwrap_or(false)
}

/// How many items carry a tool component at all.
///
/// A boot line and a check in one: 33 on 1.21.1, and zero would be a build
/// whose item components lost the one this module is about.
#[must_use]
pub fn tools() -> usize {
    table().tools.len()
}

/// How many `(tool, block)` pairs this tool is the right one for.
///
/// The number that says the tag walk resolved: 4,523 on 1.21.1 over 33 tools,
/// and a fraction of that if the tag references are not followed.
#[must_use]
pub fn correct_pairs() -> usize {
    table()
        .tools
        .values()
        .map(|tool| {
            tool.verdict_of
                .iter()
                .filter(|index| **index > 0 && tool.verdicts[**index as usize - 1].unwrap_or(false))
                .count()
        })
        .sum()
}

impl Mining {
    fn build() -> Self {
        let count = Block::all().count();
        let mut resolved: BTreeMap<&'static str, Vec<bool>> = BTreeMap::new();
        let mut tools = BTreeMap::new();
        for item in Item::all() {
            let Some(component) = item.components().get(TOOL) else {
                continue;
            };
            let Some(rules) = component.get("rules").and_then(ComponentValue::as_list) else {
                continue;
            };
            let mut speeds = Vec::new();
            let mut verdicts = Vec::new();
            let mut speed_of = vec![0u8; count];
            let mut verdict_of = vec![0u8; count];
            for (at, rule) in rules.iter().enumerate() {
                let speed = rule.get("speed").and_then(ComponentValue::as_f32);
                let verdict = rule
                    .get("correct_for_drops")
                    .and_then(ComponentValue::as_bool);
                speeds.push(speed);
                verdicts.push(verdict);
                let Some(named) = rule.get("blocks") else {
                    continue;
                };
                let members = membership(named, &mut resolved, count);
                // First rule wins, so a slot already claimed by an earlier
                // rule is left alone. That is what makes the order of the
                // rules mean something: a pickaxe's refusal is rule 0 and its
                // speed is rule 1, and reading them the other way round would
                // hand a wooden pickaxe a diamond.
                let index = u8::try_from(at + 1).unwrap_or(u8::MAX);
                for block in 0..count {
                    if !members[block] {
                        continue;
                    }
                    if speed.is_some() && speed_of[block] == 0 {
                        speed_of[block] = index;
                    }
                    if verdict.is_some() && verdict_of[block] == 0 {
                        verdict_of[block] = index;
                    }
                }
            }
            tools.insert(
                item.protocol_id(),
                Tool {
                    speeds,
                    verdicts,
                    speed_of: speed_of.into_boxed_slice(),
                    verdict_of: verdict_of.into_boxed_slice(),
                },
            );
        }
        Self { tools }
    }
}

/// Which blocks a rule's `blocks` field names, as one bit per block.
///
/// Three spellings, all of which appear on 1.21.1: a tag (`#minecraft:leaves`),
/// one block by name (`minecraft:cobweb`) and a list of blocks. A spelling
/// this does not recognise names nothing, which is the direction that leaves a
/// player with a tool that is merely slow rather than one that eats blocks.
fn membership(
    named: ComponentValue,
    resolved: &mut BTreeMap<&'static str, Vec<bool>>,
    count: usize,
) -> Vec<bool> {
    match named {
        ComponentValue::Str(name) => one(name, resolved, count),
        ComponentValue::List(entries) => {
            let mut set = vec![false; count];
            for entry in entries {
                if let Some(name) = entry.as_str() {
                    for (block, member) in one(name, resolved, count).into_iter().enumerate() {
                        set[block] |= member;
                    }
                }
            }
            set
        }
        _ => vec![false; count],
    }
}

fn one(
    name: &'static str,
    resolved: &mut BTreeMap<&'static str, Vec<bool>>,
    count: usize,
) -> Vec<bool> {
    if let Some(tag) = name.strip_prefix('#') {
        if let Some(known) = resolved.get(tag) {
            return known.clone();
        }
        let mut set = vec![false; count];
        walk(tag, &mut set, 0);
        resolved.insert(tag, set.clone());
        return set;
    }
    let mut set = vec![false; count];
    if let Some(block) = Block::from_name(name) {
        set[block.protocol_id() as usize] = true;
    }
    set
}

/// How deep a tag may reference other tags before this stops following.
///
/// Vanilla's deepest chain is three. The limit is here for a data pack's sake
/// rather than vanilla's: a tag that referenced itself would otherwise be an
/// infinite walk at boot, and a server that hangs starting up is worse than
/// one whose axe is slow on one block.
const MAX_TAG_DEPTH: u32 = 16;

fn walk(tag: &str, set: &mut [bool], depth: u32) {
    if depth > MAX_TAG_DEPTH {
        return;
    }
    let Some(def) = tags::from_id(TagRegistry::Block, tag) else {
        return;
    };
    for member in def.members {
        if let Some(referenced) = member.strip_prefix('#') {
            walk(referenced, set, depth + 1);
        } else if let Some(block) = Block::from_name(member) {
            set[block.protocol_id() as usize] = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> Option<Item> {
        Item::from_name(name)
    }

    fn block(name: &str) -> Block {
        Block::from_name(name).expect("a vanilla block")
    }

    #[test]
    fn a_bare_hand_is_never_the_right_tool_and_is_never_faster() {
        assert!(!correct_for_drops(None, block("minecraft:stone")));
        assert_eq!(speed(None, block("minecraft:stone")), 1.0);
    }

    #[test]
    fn a_pickaxe_is_right_for_stone_and_a_shovel_is_not() {
        assert!(correct_for_drops(
            item("minecraft:wooden_pickaxe"),
            block("minecraft:stone")
        ));
        assert!(!correct_for_drops(
            item("minecraft:wooden_shovel"),
            block("minecraft:stone")
        ));
    }

    /// The case the rule order exists for. A wooden pickaxe is *faster* on
    /// diamond ore and gets nothing out of it, because rule 0 refuses the
    /// drops and rule 1 sets the speed, and both match.
    #[test]
    fn a_wooden_pickaxe_is_fast_on_diamond_ore_and_gets_nothing() {
        let ore = block("minecraft:diamond_ore");
        assert!(speed(item("minecraft:wooden_pickaxe"), ore) > 1.0);
        assert!(!correct_for_drops(item("minecraft:wooden_pickaxe"), ore));
        assert!(correct_for_drops(item("minecraft:iron_pickaxe"), ore));
    }

    /// The tag walk, stated as the thing it is for: `deepslate_diamond_ore` is
    /// in `#minecraft:mineable/pickaxe` directly, and an anvil is in it only
    /// through `#minecraft:anvil`. A walk that did not follow references would
    /// answer this one wrong and the other right.
    #[test]
    fn a_tag_reference_is_followed() {
        assert!(correct_for_drops(
            item("minecraft:iron_pickaxe"),
            block("minecraft:anvil")
        ));
        assert!(speed(item("minecraft:iron_pickaxe"), block("minecraft:anvil")) > 1.0);
    }

    /// Shears cut leaves fifteen times faster and are still not the right tool
    /// for them — the rule sets a speed and gives no verdict. Leaves do not
    /// need a correct tool, so a player still gets the sapling; what this
    /// pins is that the two questions are answered separately.
    #[test]
    fn a_rule_can_set_a_speed_without_granting_the_drops() {
        let leaves = block("minecraft:oak_leaves");
        assert_eq!(speed(item("minecraft:shears"), leaves), 15.0);
        assert!(!correct_for_drops(item("minecraft:shears"), leaves));
    }

    #[test]
    fn every_tier_refuses_something_the_next_one_up_allows() {
        let obsidian = block("minecraft:obsidian");
        assert!(!correct_for_drops(item("minecraft:iron_pickaxe"), obsidian));
        assert!(correct_for_drops(
            item("minecraft:diamond_pickaxe"),
            obsidian
        ));
    }

    #[test]
    fn the_table_describes_every_tool_and_a_lot_of_pairs() {
        assert!(tools() >= 30, "{} tools", tools());
        assert!(correct_pairs() > 1_000, "{} pairs", correct_pairs());
    }
}
