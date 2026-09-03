//! What a broken block yields, read out of the operator's own loot tables.
//!
//! # This is data, and it is not this crate's data
//!
//! What a block drops is not a rule anyone can state. Stone yields
//! cobblestone, wheat yields wheat only when it is fully grown and seeds
//! otherwise, an ore yields a variable count, a silk-touch tool changes all of
//! it, and `minecraft:oak_leaves` yields nothing on nearly every break. Every
//! one of those is a **file**: `loot_table/blocks/<block>.json`, in the data
//! pack, in the operator's own `[data] path` — the directory decision record
//! 0007 already asks them to produce and decision record 0008 already reads a
//! table out of.
//!
//! So nothing here is a table of Mojang's values. This module is a *compiler*
//! and an *evaluator* for a language, and the sentences are the operator's.
//! A server whose data pack changes what stone drops drops the changed thing,
//! because there was never a second copy of the answer to disagree with it.
//!
//! # The language is small enough to speak exactly
//!
//! That is the measurement this module rests on, and it is the reason a rule
//! was not written instead. Over the 982 block tables vanilla 1.21.1 ships:
//!
//! ```text
//!   pools whose `rolls` is anything but 1.0            0 of 1,022
//!   pools whose `bonus_rolls` is anything but 0.0      0 of 1,022
//!   entry types                                        3   item, alternatives, dynamic
//!   condition types                                    9
//!   function types                                     7
//!   deepest nesting of `alternatives`                  2
//! ```
//!
//! Nine conditions and seven functions is a vocabulary that can be implemented
//! rather than approximated, and every one of them is below. The general loot
//! language is much larger — the full 1,178 tables use more, and entity and
//! chest loot uses far more — but **block** drops do not, and block drops are
//! what a player mining feels.
//!
//! # What it refuses, and why it says so
//!
//! A condition this compiler has never heard of could mean anything, so an
//! entry carrying one is **refused**: it yields nothing and is counted, and
//! [`Table::refused`] says how many. It is never guessed at, and it is never
//! silently treated as false — a condition quietly read as false is a drop
//! quietly deleted, which is the failure this project rules out everywhere
//! else.
//!
//! A *function* is different: it modifies a stack that is dropping either way.
//! `minecraft:copy_components` on a chest copies the chest's custom name, and
//! Dust has no block entities yet, so the chest still drops and still is a
//! chest. Those are counted apart, as [`Table::needs_block_entity`], because
//! the drop is right and the trimming is missing.
//!
//! # A count is not a stack
//!
//! [`Drop::count`] is what the table said, which for a fortune-enchanted ore
//! or a `set_count` of eight is not bounded by a stack size and for
//! `minecraft:sea_lantern` is bounded by a `limit_count` that has nothing to
//! do with one. Splitting a count into stacks is the caller's job, because
//! only the caller knows what a stack is.

use dust_registry::tags::{self, TagRegistry};
use dust_registry::{Block, BlockState, Item};

/// One thing a break yielded: what item, and how many of it.
///
/// The count is the loot table's own number and is deliberately not clamped to
/// a stack: `minecraft:glowstone` with fortune says four, `minecraft:snow`
/// with eight layers says eight, and both are one `Drop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drop {
    pub item: Item,
    pub count: u32,
}

/// The tool the break was made with, as the loot tables ask about it.
///
/// Enchantments are a borrowed slice rather than a map because the question is
/// asked at most a handful of times per break over a list that is at most a
/// handful long, and a `HashMap` allocated per break to answer it twice would
/// cost more than the scan it replaced.
///
/// **The server reads this off the held stack**, which it could not do for the
/// life of these tables — every silk-touch and fortune branch was compiled and
/// unreachable, and a player mining ore with a fortune pickaxe got one drop.
/// The day the stack knew, every one of those branches started working with no
/// change here at all. Decision record 0028 has the 27 rows a real 1.21.1
/// server was measured giving and the 12 that go wrong when this is emptied.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tool<'a> {
    /// The item in the breaking hand, or `None` for a bare hand.
    pub item: Option<Item>,
    /// `(enchantment id, level)`, ids spelled `minecraft:silk_touch`.
    ///
    /// Filled from the held stack's `minecraft:enchantments` component; see
    /// `dust_registry::enchantments`. It was empty for the life of the drop
    /// tables and every silk-touch and fortune branch took its unenchanted
    /// side; decision record 0028 has the 27 rows a real server was measured
    /// giving, and the 12 of them that go wrong when this is emptied again.
    pub enchantments: &'a [(&'a str, u32)],
}

impl Tool<'_> {
    fn level(&self, enchantment: &str) -> u32 {
        self.enchantments
            .iter()
            .find(|(name, _)| *name == enchantment)
            .map_or(0, |(_, level)| *level)
    }
}

/// Everything a table may ask about one break that is not the table itself.
#[derive(Debug, Clone, Copy)]
pub struct Break<'a> {
    /// The state that was broken. Conditions read its properties.
    pub state: BlockState,
    /// What it was broken with.
    pub tool: Tool<'a>,
    /// Whether an entity broke it. A block that fell, was pushed or was
    /// dissolved by a piston has no `this` entity, and two tables ask.
    pub broken_by_entity: bool,
    /// Whether this state yields nothing to the wrong tool — Minecraft's
    /// `requiresCorrectToolForDrops`, which is a Java constant and reaches
    /// here out of `dust-constants.tsv`'s `requires_tool` column.
    ///
    /// **The caller supplies whether the block cares; this module works out
    /// whether the tool is right.** The two are different questions with
    /// different sources: the first is a property of the block state and the
    /// second is the held item's `minecraft:tool` component read against the
    /// block, and a server that conflated them would either hand a
    /// bare-handed player cobblestone or refuse a shovel its dirt.
    ///
    /// `false` for a table with no such column, which is what a server
    /// extracted before the column existed does — and it is the direction that
    /// is generous rather than the one that quietly stops a player mining.
    pub requires_tool: bool,
    /// The states of cells around the broken one, as `(y offset, state)`.
    /// Two tables read them — the two double-tall plants, which check the half
    /// above or below to decide which of the pair is the one that drops.
    /// An offset the caller did not supply is a question with no answer, and
    /// the entry that asked it is refused rather than guessed.
    pub neighbours: &'a [(i8, BlockState)],
}

/// A deterministic source of randomness for one break.
///
/// xorshift64\*, which is eight bytes of state and three shifts per number.
/// A loot roll needs a stream that is cheap and reproducible, not one that is
/// unpredictable to an adversary, and a seedable eight-byte struct is what
/// lets `tests/drops.rs` state that a given seed yields a given drop.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u64);

impl Rng {
    /// A stream from a seed. Zero is replaced, because xorshift cannot leave
    /// it.
    pub fn from_seed(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A float in `[0, 1)`, from the top 53 bits.
    fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }

    /// A whole number in `low..=high`, or `low` when the range is empty.
    fn between(&mut self, low: i32, high: i32) -> i32 {
        if high <= low {
            return low;
        }
        let span = (high - low) as u64 + 1;
        low + (self.next_u64() % span) as i32
    }
}

/// A number a table asks for: constant, uniform over a range, or binomial.
///
/// These are Minecraft's `NumberProvider`s, and the three the block tables use
/// are all of them. Stored as the floats the file spells so the compile step
/// is a read and not an interpretation; rounding happens where the number is
/// wanted, which is what Minecraft does too.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Number {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Binomial { n: f32, p: f32 },
}

impl Number {
    fn roll(self, rng: &mut Rng) -> i32 {
        match self {
            Self::Constant(v) => v.round() as i32,
            Self::Uniform { min, max } => rng.between(min.round() as i32, max.round() as i32),
            Self::Binomial { n, p } => {
                let trials = n.round().max(0.0) as u32;
                let mut hits = 0;
                for _ in 0..trials {
                    if rng.next_f32() < p {
                        hits += 1;
                    }
                }
                hits
            }
        }
    }
}

/// One test a pool or an entry has to pass.
#[derive(Debug, Clone, PartialEq)]
enum Cond {
    /// The drop survives being blown up. A break is not an explosion, so this
    /// is true — and it is kept rather than dropped at compile time because
    /// the day Dust has TNT it stops being true, and a condition that was
    /// optimised away is one nobody can find.
    SurvivesExplosion,
    /// The tool holds this enchantment at this level or higher.
    ToolEnchanted {
        enchantment: Box<str>,
        min_level: u32,
    },
    /// The tool is exactly this item.
    ToolIs(Item),
    /// The tool is in this item tag, `#` stripped.
    ToolIn(Box<str>),
    /// The broken state has this property set to this value.
    StateIs {
        property: Box<str>,
        value: Box<str>,
    },
    /// Fortune's own chance ladder: index by the level, capped at the end.
    TableBonus {
        enchantment: Box<str>,
        chances: Box<[f32]>,
    },
    /// A flat chance.
    RandomChance(f32),
    /// The cell this far up or down holds this block, with these properties.
    NeighbourIs {
        offset: i8,
        block: Block,
        state: Box<[(Box<str>, Box<str>)]>,
    },
    /// There is a `this` entity — something broke it rather than it falling.
    BrokenByEntity,
    AnyOf(Box<[Cond]>),
    AllOf(Box<[Cond]>),
    Inverted(Box<Cond>),
}

/// One change a table makes to a stack that is already dropping.
#[derive(Debug, Clone, PartialEq)]
enum Func {
    SetCount {
        count: Number,
        add: bool,
        conditions: Box<[Cond]>,
    },
    ApplyBonus {
        enchantment: Box<str>,
        formula: Formula,
        conditions: Box<[Cond]>,
    },
    LimitCount {
        min: Option<f32>,
        max: Option<f32>,
    },
    /// Halves the count once per level of an explosion's radius. A break is
    /// not an explosion, so this does nothing — kept for the same reason
    /// [`Cond::SurvivesExplosion`] is.
    ExplosionDecay,
    /// Known, and needs a block entity Dust does not have yet. The item drops;
    /// what it would have carried does not.
    NeedsBlockEntity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Formula {
    /// `count + rng(0..=level)`, applied `bonus_multiplier` times.
    UniformBonusCount { multiplier: i32 },
    /// Minecraft's ore formula: with fortune n, a `1..=n+1` multiplier, and
    /// only a multiplier of 2 or more does anything.
    OreDrops,
    /// A binomial with `extra + level` trials at a fixed probability.
    BinomialWithBonusCount { extra: i32, probability: f32 },
}

/// One entry in a pool: an item, or a first-that-passes list of them.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Item {
        item: Item,
        conditions: Box<[Cond]>,
        functions: Box<[Func]>,
    },
    Alternatives {
        conditions: Box<[Cond]>,
        functions: Box<[Func]>,
        children: Box<[Entry]>,
    },
    /// Something this compiler could not read. It yields nothing and is
    /// counted; it is never treated as an empty list of items that happened to
    /// pass no conditions, because those two look the same in a drop and mean
    /// opposite things.
    Refused,
}

#[derive(Debug, Clone, PartialEq)]
struct Pool {
    conditions: Box<[Cond]>,
    functions: Box<[Func]>,
    entries: Box<[Entry]>,
}

/// One compiled `loot_table/blocks/<block>.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    functions: Box<[Func]>,
    pools: Box<[Pool]>,
    refused: u32,
    needs_block_entity: u32,
}

impl Table {
    /// How many entries this table has that the compiler refused to read.
    /// Zero is the answer for every vanilla 1.21.1 block table but one.
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// How many functions in this table want a block entity Dust has not built
    /// yet. The items still drop; their copied name, contents or state do not.
    pub fn needs_block_entity(&self) -> u32 {
        self.needs_block_entity
    }

    /// Roll this table for one break, appending what it yielded.
    ///
    /// Appends rather than returns so a caller breaking many blocks reuses one
    /// buffer: a `Vec` per break is an allocation per break, and this runs on
    /// the tick loop.
    ///
    /// # The tool gate comes first, and it is not a condition
    ///
    /// A block that wants a correct tool and did not get one yields nothing at
    /// all, and it yields nothing *outside* the table: Minecraft never calls
    /// `playerDestroy`, so no pool is rolled, no function runs and no
    /// `survives_explosion` is asked. Writing it as one more condition would
    /// give the same answer for every vanilla table today and a different one
    /// the day a table has a pool nobody expected to be reachable.
    ///
    /// It is here rather than in the caller so that everything asking this
    /// module what a break yields is asked the same question — the server, the
    /// tests and `cargo xtask harness drops`, which scores Dust against a real
    /// vanilla server and would otherwise be scoring a rule the server has and
    /// it does not.
    pub fn roll(&self, ctx: &Break<'_>, rng: &mut Rng, out: &mut Vec<Drop>) {
        if !harvestable(ctx) {
            return;
        }
        let start = out.len();
        for pool in &self.pools {
            if !all_pass(&pool.conditions, ctx, rng) {
                continue;
            }
            // Every vanilla block pool rolls exactly once and has no bonus
            // rolls, which the module documentation states as a measurement.
            // A pool that said otherwise would have been refused at compile
            // time rather than rolled the wrong number of times here.
            for entry in &pool.entries {
                self.take(entry, &pool.functions, ctx, rng, out);
            }
        }
        // The table's own functions apply to everything that came out of it.
        for drop in &mut out[start..] {
            for function in &self.functions {
                apply(function, drop, ctx, rng);
            }
        }
    }

    fn take(
        &self,
        entry: &Entry,
        pool_functions: &[Func],
        ctx: &Break<'_>,
        rng: &mut Rng,
        out: &mut Vec<Drop>,
    ) -> bool {
        match entry {
            Entry::Refused => false,
            Entry::Item {
                item,
                conditions,
                functions,
            } => {
                if !all_pass(conditions, ctx, rng) {
                    return false;
                }
                let mut drop = Drop {
                    item: *item,
                    count: 1,
                };
                for function in functions.iter().chain(pool_functions) {
                    apply(function, &mut drop, ctx, rng);
                }
                if drop.count > 0 {
                    out.push(drop);
                }
                true
            }
            Entry::Alternatives {
                conditions,
                functions,
                children,
            } => {
                if !all_pass(conditions, ctx, rng) {
                    return false;
                }
                // First child that passes wins, and the rest are not asked.
                // That is what makes stone's table one item and not two.
                let before = out.len();
                for child in children.iter() {
                    if self.take(child, pool_functions, ctx, rng, out) {
                        for drop in &mut out[before..] {
                            for function in functions.iter() {
                                apply(function, drop, ctx, rng);
                            }
                        }
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// Whether the generated item tag holds this item.
///
/// Exactly one vanilla block table asks — `minecraft:amethyst_cluster`, whose
/// `#minecraft:cluster_max_harvestables` decides four shards from two. The tag
/// itself is checked at *compile* time: a `match_tool` naming a tag this build
/// has never heard of refuses its entry there, so by the time this runs the
/// only question left is whether the tool is in it.
fn tool_in_tag(tag: &str, item: Item) -> bool {
    tags::from_id(TagRegistry::Item, tag).is_some_and(|def| def.contains(item.name()))
}

/// Whether this break gets anything at all.
///
/// Minecraft's `Player.hasCorrectToolForDrops`: a state that does not require
/// a correct tool always yields, and one that does yields only to an item
/// whose `minecraft:tool` component says so. A wooden pickaxe on diamond ore
/// is the case the whole rule exists for — it breaks the block, faster than a
/// bare hand, and hands the player nothing.
///
/// **The block still breaks.** Nothing here refuses the break; a server that
/// left the block standing because the tool was wrong would feel broken rather
/// than strict, and it is not what vanilla does.
fn harvestable(ctx: &Break<'_>) -> bool {
    !ctx.requires_tool || dust_registry::mining::correct_for_drops(ctx.tool.item, ctx.state.block())
}

fn all_pass(conditions: &[Cond], ctx: &Break<'_>, rng: &mut Rng) -> bool {
    conditions
        .iter()
        .all(|condition| passes(condition, ctx, rng))
}

fn passes(condition: &Cond, ctx: &Break<'_>, rng: &mut Rng) -> bool {
    match condition {
        Cond::SurvivesExplosion => true,
        Cond::BrokenByEntity => ctx.broken_by_entity,
        Cond::ToolEnchanted {
            enchantment,
            min_level,
        } => ctx.tool.level(enchantment) >= *min_level,
        Cond::ToolIs(item) => ctx.tool.item == Some(*item),
        Cond::ToolIn(tag) => ctx.tool.item.is_some_and(|item| tool_in_tag(tag, item)),
        Cond::StateIs { property, value } => ctx.state.property(property) == Some(value.as_ref()),
        Cond::TableBonus {
            enchantment,
            chances,
        } => {
            let level = ctx.tool.level(enchantment) as usize;
            let chance = chances
                .get(level)
                .or_else(|| chances.last())
                .copied()
                .unwrap_or(0.0);
            rng.next_f32() < chance
        }
        Cond::RandomChance(chance) => rng.next_f32() < *chance,
        Cond::NeighbourIs {
            offset,
            block,
            state,
        } => {
            let Some((_, found)) = ctx.neighbours.iter().find(|(dy, _)| dy == offset) else {
                return false;
            };
            found.block() == *block
                && state
                    .iter()
                    .all(|(name, want)| found.property(name) == Some(want.as_ref()))
        }
        Cond::AnyOf(terms) => terms.iter().any(|term| passes(term, ctx, rng)),
        Cond::AllOf(terms) => terms.iter().all(|term| passes(term, ctx, rng)),
        Cond::Inverted(term) => !passes(term, ctx, rng),
    }
}

fn apply(function: &Func, drop: &mut Drop, ctx: &Break<'_>, rng: &mut Rng) {
    match function {
        Func::ExplosionDecay | Func::NeedsBlockEntity => {}
        Func::SetCount {
            count,
            add,
            conditions,
        } => {
            if !all_pass(conditions, ctx, rng) {
                return;
            }
            let rolled = count.roll(rng);
            let next = if *add {
                drop.count as i64 + rolled as i64
            } else {
                rolled as i64
            };
            drop.count = next.clamp(0, u32::MAX as i64) as u32;
        }
        Func::ApplyBonus {
            enchantment,
            formula,
            conditions,
        } => {
            if !all_pass(conditions, ctx, rng) {
                return;
            }
            let level = ctx.tool.level(enchantment) as i32;
            let count = drop.count as i32;
            let next = match formula {
                Formula::UniformBonusCount { multiplier } => {
                    count + rng.between(0, level * multiplier)
                }
                Formula::OreDrops => {
                    if level <= 0 {
                        count
                    } else {
                        // Minecraft rolls `-1..=level`, and a roll below one
                        // leaves the count alone. That is why fortune I on an
                        // ore is worth a third of a drop and not a whole one.
                        let multiplier = rng.between(-1, level).max(0) + 1;
                        count * multiplier
                    }
                }
                Formula::BinomialWithBonusCount { extra, probability } => {
                    let trials = extra + level;
                    let mut hits = 0;
                    for _ in 0..trials.max(0) {
                        if rng.next_f32() < *probability {
                            hits += 1;
                        }
                    }
                    count + hits
                }
            };
            drop.count = next.max(0) as u32;
        }
        Func::LimitCount { min, max } => {
            let mut count = drop.count as i64;
            if let Some(min) = min {
                count = count.max(min.round() as i64);
            }
            if let Some(max) = max {
                count = count.min(max.round() as i64);
            }
            drop.count = count.clamp(0, u32::MAX as i64) as u32;
        }
    }
}

/// Every block table an operator's data has, keyed by the block it belongs to.
///
/// A flat vector indexed by the block's own protocol id rather than a map:
/// there are about a thousand blocks, the index is already in hand at every
/// call site, and a break should not hash a string to find out what it yielded.
#[derive(Debug, Default)]
pub struct Tables {
    /// Every table read, in the order the files were offered.
    compiled: Vec<Table>,
    /// Per block, which of them it draws from.
    ///
    /// An index and not a table, because a table is shared: about sixty blocks
    /// on 1.21.1 draw from another block's file, and `blocks/oak_sign.json`
    /// serves `oak_sign` and `oak_wall_sign` both. Compiling it twice would be
    /// two answers to one question and twice the memory for them.
    by_block: Vec<Option<u32>>,
    files: u32,
    refused_files: u32,
}

impl Tables {
    /// Compile one file, named by the block whose table it is.
    ///
    /// The name is the file's own path stem — `blocks/stone.json` is
    /// `minecraft:stone` — which is the convention every vanilla block table
    /// follows. See [`Tables::table`] for what this does *not* claim.
    pub fn insert(&mut self, block: Block, json: &str) -> Result<(), CompileError> {
        self.insert_for(std::slice::from_ref(&block), json)
    }

    /// Compile one file **once** and point every block that draws from it at
    /// the result.
    ///
    /// Which blocks those are is `dust-blocks.tsv`'s answer and not this
    /// module's: `Block.getLootTable` is a Java constant, `blocks/oak_sign.json`
    /// serves two blocks and `blocks/bamboo.json` serves one, and there is no
    /// rule about file names that says which. See
    /// [`dust_registry::loot::BlockLoot`].
    pub fn insert_for(&mut self, blocks: &[Block], json: &str) -> Result<(), CompileError> {
        self.files += 1;
        let table = compile(json)?;
        let at = self.compiled.len() as u32;
        self.compiled.push(table);
        for block in blocks {
            let index = block.protocol_id() as usize;
            if self.by_block.len() <= index {
                self.by_block.resize_with(index + 1, || None);
            }
            self.by_block[index] = Some(at);
        }
        Ok(())
    }

    /// Record that a file could not be compiled, so the count of what is here
    /// and the count of what was read stay comparable.
    pub fn refuse(&mut self) {
        self.files += 1;
        self.refused_files += 1;
    }

    /// This block's table, or `None` when the data holds no table under this
    /// block's name.
    ///
    /// **`None` is not "drops nothing".** It is "nobody here knows", and the
    /// two are different: `minecraft:bedrock` has no table because it yields
    /// nothing, and `minecraft:oak_wall_sign` has no table under its own name
    /// because Minecraft points it at `minecraft:oak_sign`'s. A caller that
    /// read `None` as an empty drop would be right about the first and wrong
    /// about the second, which is why this answers the question it was asked
    /// and leaves the reading to the reader.
    pub fn table(&self, block: Block) -> Option<&Table> {
        let at = (*self.by_block.get(block.protocol_id() as usize)?)?;
        self.compiled.get(at as usize)
    }

    /// How many blocks have a table here.
    pub fn len(&self) -> usize {
        self.by_block.iter().filter(|slot| slot.is_some()).count()
    }

    /// How many distinct tables were compiled.
    ///
    /// Not the same number as [`Tables::len`] once a file can serve several
    /// blocks, and the gap between them is what says the wall forms found
    /// theirs: 982 files covering 1,042 blocks on 1.21.1.
    pub fn distinct(&self) -> usize {
        self.compiled.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many files were offered, compiled or not.
    pub fn files(&self) -> u32 {
        self.files
    }

    /// How many files could not be compiled at all.
    pub fn refused_files(&self) -> u32 {
        self.refused_files
    }

    /// Entries refused across every table here.
    pub fn refused_entries(&self) -> u32 {
        self.compiled.iter().map(Table::refused).sum()
    }

    /// Functions across every table here that want a block entity.
    pub fn needs_block_entity(&self) -> u32 {
        self.compiled.iter().map(Table::needs_block_entity).sum()
    }
}

/// Why a loot table file could not be compiled at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The bytes are not JSON.
    NotJson(String),
    /// The top level is not an object.
    NotAnObject,
    /// `type` is not `minecraft:block`. A chest or entity table read as a
    /// block table would answer questions it was never asked.
    NotABlockTable(String),
    /// A pool rolls a number of times this compiler cannot express. No vanilla
    /// block table does; a data pack's might, and it is refused whole rather
    /// than rolled once and hoped over.
    UnsupportedRolls(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(why) => write!(f, "not JSON: {why}"),
            Self::NotAnObject => write!(f, "the top level is not a JSON object"),
            Self::NotABlockTable(kind) => {
                write!(
                    f,
                    "type is {kind}, and a block table's type is minecraft:block"
                )
            }
            Self::UnsupportedRolls(spelling) => write!(
                f,
                "a pool rolls {spelling}, and every vanilla block pool rolls exactly once"
            ),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile one loot table file.
pub fn compile(json: &str) -> Result<Table, CompileError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CompileError::NotJson(e.to_string()))?;
    let object = value.as_object().ok_or(CompileError::NotAnObject)?;
    match object.get("type").and_then(|v| v.as_str()) {
        Some("minecraft:block") => {}
        Some(other) => return Err(CompileError::NotABlockTable(other.to_owned())),
        None => return Err(CompileError::NotABlockTable("absent".to_owned())),
    }

    let mut counts = Counts::default();
    let functions = functions_or_refuse(object.get("functions"), &mut counts);
    let mut pools = Vec::new();
    for pool in object
        .get("pools")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(pool) = pool.as_object() else {
            counts.refused += 1;
            continue;
        };
        let rolls = pool.get("rolls");
        if rolls.and_then(serde_json::Value::as_f64) != Some(1.0) {
            return Err(CompileError::UnsupportedRolls(
                rolls.map_or_else(|| "nothing".to_owned(), ToString::to_string),
            ));
        }
        if pool
            .get("bonus_rolls")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            != 0.0
        {
            return Err(CompileError::UnsupportedRolls(
                "a non-zero bonus_rolls".to_owned(),
            ));
        }
        let entries = pool
            .get("entries")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|entry| entry_of(entry, &mut counts))
            .collect();
        pools.push(Pool {
            conditions: conditions(pool.get("conditions"), &mut counts),
            functions: functions_or_refuse(pool.get("functions"), &mut counts),
            entries,
        });
    }

    Ok(Table {
        functions,
        pools: pools.into_boxed_slice(),
        refused: counts.refused,
        needs_block_entity: counts.needs_block_entity,
    })
}

#[derive(Default)]
struct Counts {
    refused: u32,
    needs_block_entity: u32,
}

fn entry_of(value: &serde_json::Value, counts: &mut Counts) -> Entry {
    let Some(object) = value.as_object() else {
        counts.refused += 1;
        return Entry::Refused;
    };
    let kind = object.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let conditions = conditions_or_refuse(object.get("conditions"), counts);
    let Some(conditions) = conditions else {
        counts.refused += 1;
        return Entry::Refused;
    };
    let functions = functions_or_refuse_entry(object.get("functions"), counts);
    let Some(functions) = functions else {
        counts.refused += 1;
        return Entry::Refused;
    };
    match kind {
        "minecraft:item" => {
            let name = object.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match Item::from_name(name) {
                Some(item) => Entry::Item {
                    item,
                    conditions,
                    functions,
                },
                None => {
                    counts.refused += 1;
                    Entry::Refused
                }
            }
        }
        "minecraft:alternatives" => {
            let children = object
                .get("children")
                .and_then(|v| v.as_array())
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|child| entry_of(child, counts))
                .collect();
            Entry::Alternatives {
                conditions,
                functions,
                children,
            }
        }
        _ => {
            counts.refused += 1;
            Entry::Refused
        }
    }
}

/// Conditions on a pool: an unreadable one refuses the whole pool by making it
/// impossible to pass, which is what `Cond::RandomChance(0.0)` says.
fn conditions(value: Option<&serde_json::Value>, counts: &mut Counts) -> Box<[Cond]> {
    match conditions_or_refuse(value, counts) {
        Some(conditions) => conditions,
        None => {
            counts.refused += 1;
            Box::new([Cond::RandomChance(0.0)])
        }
    }
}

fn conditions_or_refuse(
    value: Option<&serde_json::Value>,
    counts: &mut Counts,
) -> Option<Box<[Cond]>> {
    let Some(array) = value else {
        return Some(Box::new([]));
    };
    let array = array.as_array()?;
    let mut out = Vec::with_capacity(array.len());
    for term in array {
        out.push(condition(term, counts)?);
    }
    Some(out.into_boxed_slice())
}

fn condition(value: &serde_json::Value, counts: &mut Counts) -> Option<Cond> {
    let object = value.as_object()?;
    match object.get("condition")?.as_str()? {
        "minecraft:survives_explosion" => Some(Cond::SurvivesExplosion),
        "minecraft:entity_properties" => {
            // Every vanilla use asks for `this` with an empty predicate, which
            // is the question "did something break this". A predicate with
            // anything in it asks about the breaker and is refused.
            let empty = object
                .get("predicate")
                .and_then(|v| v.as_object())
                .is_some_and(serde_json::Map::is_empty);
            (empty && object.get("entity").and_then(|v| v.as_str()) == Some("this"))
                .then_some(Cond::BrokenByEntity)
        }
        "minecraft:match_tool" => match_tool(object.get("predicate")?),
        "minecraft:block_state_property" => {
            let properties = object.get("properties")?.as_object()?;
            let mut terms: Vec<Cond> = Vec::with_capacity(properties.len());
            for (property, want) in properties {
                terms.push(Cond::StateIs {
                    property: property.as_str().into(),
                    value: want.as_str()?.into(),
                });
            }
            Some(if terms.len() == 1 {
                terms.pop()?
            } else {
                Cond::AllOf(terms.into_boxed_slice())
            })
        }
        "minecraft:table_bonus" => {
            let chances: Option<Vec<f32>> = object
                .get("chances")?
                .as_array()?
                .iter()
                .map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            Some(Cond::TableBonus {
                enchantment: object.get("enchantment")?.as_str()?.into(),
                chances: chances?.into_boxed_slice(),
            })
        }
        "minecraft:random_chance" => {
            Some(Cond::RandomChance(object.get("chance")?.as_f64()? as f32))
        }
        "minecraft:location_check" => {
            // The only shape any vanilla block table uses: a Y offset and a
            // block predicate naming one block and some of its properties.
            if object.get("offsetX").is_some() || object.get("offsetZ").is_some() {
                return None;
            }
            let offset = i8::try_from(object.get("offsetY")?.as_i64()?).ok()?;
            let block_predicate = object.get("predicate")?.as_object()?.get("block")?;
            let block = Block::from_name(block_predicate.get("blocks")?.as_str()?)?;
            let mut state = Vec::new();
            if let Some(want) = block_predicate.get("state").and_then(|v| v.as_object()) {
                for (property, value) in want {
                    state.push((property.as_str().into(), value.as_str()?.into()));
                }
            }
            Some(Cond::NeighbourIs {
                offset,
                block,
                state: state.into_boxed_slice(),
            })
        }
        "minecraft:any_of" => Some(Cond::AnyOf(terms(object.get("terms")?, counts)?)),
        "minecraft:all_of" => Some(Cond::AllOf(terms(object.get("terms")?, counts)?)),
        "minecraft:inverted" => Some(Cond::Inverted(Box::new(condition(
            object.get("term")?,
            counts,
        )?))),
        _ => None,
    }
}

fn terms(value: &serde_json::Value, counts: &mut Counts) -> Option<Box<[Cond]>> {
    let array = value.as_array()?;
    let mut out = Vec::with_capacity(array.len());
    for term in array {
        out.push(condition(term, counts)?);
    }
    Some(out.into_boxed_slice())
}

/// `match_tool`'s predicate, which is an item predicate and not a tool one.
///
/// Two shapes appear in the block tables and both are here: `items`, which
/// names an item or an item tag, and `predicates` carrying
/// `minecraft:enchantments`. Anything else about a tool — its damage, its
/// components — is refused, because a predicate read as "no constraint" is a
/// silk-touch branch that fires for a bare hand.
fn match_tool(predicate: &serde_json::Value) -> Option<Cond> {
    let object = predicate.as_object()?;
    let mut terms: Vec<Cond> = Vec::new();
    for (key, value) in object {
        match key.as_str() {
            "items" => {
                let spelling = value.as_str()?;
                terms.push(match spelling.strip_prefix('#') {
                    // Refused here rather than answered false at run time: a
                    // tag nothing has heard of is not an empty tag.
                    Some(tag) => {
                        tags::from_id(TagRegistry::Item, tag)?;
                        Cond::ToolIn(tag.into())
                    }
                    None => Cond::ToolIs(Item::from_name(spelling)?),
                });
            }
            "predicates" => {
                let inner = value.as_object()?;
                for (kind, argument) in inner {
                    if kind != "minecraft:enchantments" {
                        return None;
                    }
                    for wanted in argument.as_array()? {
                        let wanted = wanted.as_object()?;
                        let levels = wanted.get("levels");
                        let min = match levels {
                            None => 1,
                            Some(serde_json::Value::Number(n)) => n.as_u64()? as u32,
                            Some(serde_json::Value::Object(range)) => {
                                range.get("min")?.as_u64()? as u32
                            }
                            Some(_) => return None,
                        };
                        terms.push(Cond::ToolEnchanted {
                            enchantment: wanted.get("enchantments")?.as_str()?.into(),
                            min_level: min,
                        });
                    }
                }
            }
            _ => return None,
        }
    }
    match terms.len() {
        0 => None,
        1 => terms.pop(),
        _ => Some(Cond::AllOf(terms.into_boxed_slice())),
    }
}

fn functions_or_refuse(value: Option<&serde_json::Value>, counts: &mut Counts) -> Box<[Func]> {
    functions_or_refuse_entry(value, counts).unwrap_or_else(|| {
        counts.refused += 1;
        Box::new([])
    })
}

fn functions_or_refuse_entry(
    value: Option<&serde_json::Value>,
    counts: &mut Counts,
) -> Option<Box<[Func]>> {
    let Some(array) = value else {
        return Some(Box::new([]));
    };
    let array = array.as_array()?;
    let mut out = Vec::with_capacity(array.len());
    for entry in array {
        out.push(function(entry, counts)?);
    }
    Some(out.into_boxed_slice())
}

fn function(value: &serde_json::Value, counts: &mut Counts) -> Option<Func> {
    let object = value.as_object()?;
    let conditions = conditions_or_refuse(object.get("conditions"), counts)?;
    match object.get("function")?.as_str()? {
        "minecraft:explosion_decay" => Some(Func::ExplosionDecay),
        "minecraft:copy_components" | "minecraft:copy_state" | "minecraft:copy_custom_data" => {
            counts.needs_block_entity += 1;
            Some(Func::NeedsBlockEntity)
        }
        "minecraft:set_count" => Some(Func::SetCount {
            count: number(object.get("count")?)?,
            add: object
                .get("add")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            conditions,
        }),
        "minecraft:limit_count" => {
            let limit = object.get("limit")?;
            let (min, max) = match limit {
                serde_json::Value::Number(n) => {
                    let v = n.as_f64()? as f32;
                    (Some(v), Some(v))
                }
                serde_json::Value::Object(range) => (
                    range.get("min").and_then(|v| v.as_f64()).map(|v| v as f32),
                    range.get("max").and_then(|v| v.as_f64()).map(|v| v as f32),
                ),
                _ => return None,
            };
            Some(Func::LimitCount { min, max })
        }
        "minecraft:apply_bonus" => {
            let parameters = object.get("parameters").and_then(|v| v.as_object());
            let formula = match object.get("formula")?.as_str()? {
                "minecraft:ore_drops" => Formula::OreDrops,
                "minecraft:uniform_bonus_count" => Formula::UniformBonusCount {
                    multiplier: parameters
                        .and_then(|p| p.get("bonusMultiplier"))
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(1) as i32,
                },
                "minecraft:binomial_with_bonus_count" => {
                    let parameters = parameters?;
                    Formula::BinomialWithBonusCount {
                        extra: parameters.get("extra")?.as_i64()? as i32,
                        probability: parameters.get("probability")?.as_f64()? as f32,
                    }
                }
                _ => return None,
            };
            Some(Func::ApplyBonus {
                enchantment: object.get("enchantment")?.as_str()?.into(),
                formula,
                conditions,
            })
        }
        _ => None,
    }
}

fn number(value: &serde_json::Value) -> Option<Number> {
    match value {
        serde_json::Value::Number(n) => Some(Number::Constant(n.as_f64()? as f32)),
        serde_json::Value::Object(object) => match object.get("type").and_then(|v| v.as_str()) {
            Some("minecraft:uniform") => Some(Number::Uniform {
                min: object.get("min")?.as_f64()? as f32,
                max: object.get("max")?.as_f64()? as f32,
            }),
            Some("minecraft:binomial") => Some(Number::Binomial {
                n: object.get("n")?.as_f64()? as f32,
                p: object.get("p")?.as_f64()? as f32,
            }),
            Some("minecraft:constant") => {
                Some(Number::Constant(object.get("value")?.as_f64()? as f32))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The block a `blocks/<name>.json` file belongs to, from its path stem.
///
/// Every vanilla block table is named after its block, which is a fact about
/// vanilla's data and not a rule this crate imposes: a file named after
/// nothing is `None`, and the caller counts it rather than inventing a block
/// for it.
pub fn block_of_file(namespace: &str, stem: &str) -> Option<Block> {
    let mut name = String::with_capacity(namespace.len() + stem.len() + 1);
    name.push_str(namespace);
    name.push(':');
    name.push_str(stem);
    Block::from_name(&name)
}

/// Which blocks a set of tables says nothing about, in name order.
///
/// A caller reports this rather than asserting on it: on vanilla 1.21.1, 78 of
/// the 1,060 blocks have no table of their own name, and they are two very
/// different groups. See [`Tables::table`].
pub fn blocks_without_a_table(tables: &Tables) -> Vec<Block> {
    Block::all()
        .filter(|block| tables.table(*block).is_none())
        .collect()
}
