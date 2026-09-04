//! What a fire turns one item into: smelting, blasting, smoking, a campfire.
//!
//! # Why this is not [`crafting`](crate::crafting)
//!
//! A grid recipe is a *pattern* over up to nine slots and the interesting work
//! is matching it. A cooking recipe has exactly one ingredient, so there is no
//! matching at all — the question is only "given this item and this fire, what
//! comes out, how long does it take and what is it worth". That makes the
//! whole table a lookup keyed by `(fire, item)`, which is an array index and
//! not a search.
//!
//! # The four fires are four tables, not one with a filter
//!
//! A furnace reads `minecraft:smelting`, a blast furnace `minecraft:blasting`,
//! a smoker `minecraft:smoking` and a campfire `minecraft:campfire_cooking`.
//! They overlap — raw beef is in three of them — and they disagree about the
//! time: 200 ticks in a furnace, 100 in a smoker, 600 on a campfire. Reading
//! one table and halving it for a smoker would be a rule where the data has an
//! answer, and it would be wrong for every recipe that is not in both.
//!
//! # What it costs
//!
//! One `u32` per (fire, item) pair: four arrays the length of the item
//! registry, one allocation, about 21 kB on 1.21.1's 1,333 items. A lookup is
//! one bounds check and one load. That is the same shape as
//! [`crafting`](crate::crafting)'s index and for the same reason — this is
//! asked every time a furnace's input slot changes and once per completed
//! smelt, which on a world full of furnaces is often.
//!
//! # What is not here
//!
//! How long a *fuel* burns for. That is `dust-items.tsv`'s `burn` column, out
//! of the operator's own jar, because unlike everything on this page it is not
//! in any recipe file. See `dust_registry::placement::ItemBlocks::burn`.

use dust_registry::Item;

use crate::crafting::{one_into, result_stack, ItemTags, Refusal};

/// The four things that cook, and the recipe type each of them reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fire {
    /// `minecraft:furnace`, reading `minecraft:smelting`.
    Furnace,
    /// `minecraft:blast_furnace`, reading `minecraft:blasting`.
    BlastFurnace,
    /// `minecraft:smoker`, reading `minecraft:smoking`.
    Smoker,
    /// `minecraft:campfire`, reading `minecraft:campfire_cooking`.
    Campfire,
}

/// Every fire, in the order their tables are laid out.
pub const FIRES: [Fire; 4] = [
    Fire::Furnace,
    Fire::BlastFurnace,
    Fire::Smoker,
    Fire::Campfire,
];

impl Fire {
    /// The `type` of the recipe files this fire reads.
    #[must_use]
    pub fn recipe_type(self) -> &'static str {
        match self {
            Self::Furnace => "minecraft:smelting",
            Self::BlastFurnace => "minecraft:blasting",
            Self::Smoker => "minecraft:smoking",
            Self::Campfire => "minecraft:campfire_cooking",
        }
    }

    /// The block that is this fire.
    #[must_use]
    pub fn block(self) -> &'static str {
        match self {
            Self::Furnace => "minecraft:furnace",
            Self::BlastFurnace => "minecraft:blast_furnace",
            Self::Smoker => "minecraft:smoker",
            Self::Campfire => "minecraft:campfire",
        }
    }

    /// The `minecraft:menu` entry this fire's screen is.
    ///
    /// A campfire has none — it is cooked at by right-clicking food onto it
    /// and has no screen at all — which is why this is an `Option` and why a
    /// caller must not assume every fire opens.
    #[must_use]
    pub fn menu(self) -> Option<&'static str> {
        match self {
            Self::Furnace => Some("minecraft:furnace"),
            Self::BlastFurnace => Some("minecraft:blast_furnace"),
            Self::Smoker => Some("minecraft:smoker"),
            Self::Campfire => None,
        }
    }

    /// Which fire reads this recipe `type`, if any.
    #[must_use]
    pub fn from_recipe_type(kind: &str) -> Option<Self> {
        FIRES.into_iter().find(|fire| fire.recipe_type() == kind)
    }

    /// Which fire this block is, if it is one.
    #[must_use]
    pub fn from_block(name: &str) -> Option<Self> {
        FIRES.into_iter().find(|fire| fire.block() == name)
    }

    fn slot(self) -> usize {
        match self {
            Self::Furnace => 0,
            Self::BlastFurnace => 1,
            Self::Smoker => 2,
            Self::Campfire => 3,
        }
    }
}

/// What one item becomes in one fire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cooked {
    result: Item,
    count: u8,
    ticks: u16,
    experience: f32,
}

impl Cooked {
    /// The item that comes out, and how many. One, for every vanilla recipe.
    #[must_use]
    pub fn result(&self) -> (Item, u8) {
        (self.result, self.count)
    }

    /// How many ticks one of these takes. 200 in a furnace, 100 in a blast
    /// furnace or a smoker, 600 on a campfire — read from the file, never
    /// derived from another fire's number.
    #[must_use]
    pub fn ticks(&self) -> u16 {
        self.ticks
    }

    /// The experience one of these is worth, as the file writes it.
    ///
    /// A float and not a count, because the file says `0.7`. What a player
    /// actually receives is the accumulated total of many of these, rounded
    /// once at the moment they take it out — see the furnace. Rounding here
    /// would turn every iron ingot into nothing.
    #[must_use]
    pub fn experience(&self) -> f32 {
        self.experience
    }
}

/// Everything the four fires can cook.
#[derive(Debug, Default)]
pub struct Cooking {
    recipes: Vec<Cooked>,
    /// `FIRES.len() * items` entries, `u32::MAX` for "this fire does not cook
    /// this item". One allocation, built once at boot.
    index: Box<[u32]>,
    /// Pairs that were already claimed by an earlier file. Counted rather than
    /// refused: two recipes for one input in one fire is a data pack saying
    /// two things, and the first file read wins — but silence about it would
    /// hide a data pack that half works.
    collisions: usize,
}

/// The value in [`Cooking::index`] meaning "nothing".
const NOTHING: u32 = u32::MAX;

impl Cooking {
    /// An empty table sized for this build's item registry.
    #[must_use]
    pub fn new() -> Self {
        let items = Item::registry().entry_count();
        Self {
            recipes: Vec::new(),
            index: vec![NOTHING; FIRES.len() * items].into_boxed_slice(),
            collisions: 0,
        }
    }

    /// Compile one recipe file, if it is a cooking recipe.
    ///
    /// Returns [`Refusal::NotAGrid`] for anything that is not one, which is
    /// the same answer [`Recipes::add`](crate::crafting::Recipes::add) gives —
    /// so a loader can try both and count a file only both refused.
    ///
    /// # Errors
    ///
    /// [`Refusal`], naming what about the file could not be read.
    pub fn add(&mut self, value: &serde_json::Value, tags: &ItemTags) -> Result<(), Refusal> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(Refusal::NoType)?;
        let fire =
            Fire::from_recipe_type(kind).ok_or_else(|| Refusal::NotAGrid(kind.to_owned()))?;

        let (result, count) = result_stack(value)?;
        // **Read, never assumed.** 200 is the furnace's number for every
        // vanilla recipe and a data pack may say anything; a default here
        // would be a rule that is right until somebody writes a recipe.
        let ticks = value
            .get("cookingtime")
            .and_then(serde_json::Value::as_u64)
            .ok_or(Refusal::Malformed("`cookingtime` is not a number"))?;
        let ticks = u16::try_from(ticks)
            .map_err(|_| Refusal::Malformed("`cookingtime` is longer than 65,535 ticks"))?;
        if ticks == 0 {
            return Err(Refusal::Malformed("`cookingtime` is zero"));
        }
        // Absent means none. Vanilla writes `experience` on every cooking
        // recipe that is worth anything and leaves it off the rest, so this is
        // the one key where absence really is a value.
        let experience = match value.get("experience") {
            None => 0.0,
            Some(value) => value
                .as_f64()
                .ok_or(Refusal::Malformed("`experience` is not a number"))?
                as f32,
        };
        if !experience.is_finite() || experience < 0.0 {
            return Err(Refusal::Malformed("`experience` is not a positive number"));
        }

        let mut accepts = Vec::new();
        match value.get("ingredient") {
            Some(serde_json::Value::Array(list)) => {
                for one in list {
                    one_into(one, tags, &mut accepts)?;
                }
            }
            Some(one @ serde_json::Value::Object(_)) => one_into(one, tags, &mut accepts)?,
            _ => return Err(Refusal::Malformed("`ingredient` is not an object or list")),
        }
        if accepts.is_empty() {
            return Err(Refusal::Malformed("an ingredient accepts nothing"));
        }

        let at = u32::try_from(self.recipes.len())
            .map_err(|_| Refusal::Malformed("more recipes than a u32 can number"))?;
        self.recipes.push(Cooked {
            result,
            count,
            ticks,
            experience,
        });
        let items = Item::registry().entry_count();
        for id in accepts {
            let Some(cell) = self.index.get_mut(fire.slot() * items + usize::from(id)) else {
                continue;
            };
            if *cell != NOTHING {
                self.collisions += 1;
                continue;
            }
            *cell = at;
        }
        Ok(())
    }

    /// What `input` becomes in `fire`, or `None` if that fire does not cook it.
    #[must_use]
    pub fn find(&self, fire: Fire, input: Item) -> Option<&Cooked> {
        let items = Item::registry().entry_count();
        let at = *self
            .index
            .get(fire.slot() * items + input.protocol_id() as usize)?;
        (at != NOTHING).then(|| &self.recipes[at as usize])
    }

    /// How many recipes compiled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// Whether none did.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// How many `(fire, item)` pairs cook. Larger than [`Cooking::len`]
    /// wherever an ingredient is a tag or a list.
    #[must_use]
    pub fn pairs(&self) -> usize {
        self.index.iter().filter(|at| **at != NOTHING).count()
    }

    /// How many pairs a later file wanted and an earlier one already held.
    #[must_use]
    pub fn collisions(&self) -> usize {
        self.collisions
    }

    /// How many pairs one fire cooks.
    #[must_use]
    pub fn pairs_in(&self, fire: Fire) -> usize {
        let items = Item::registry().entry_count();
        self.index[fire.slot() * items..(fire.slot() + 1) * items]
            .iter()
            .filter(|at| **at != NOTHING)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the fixture is JSON")
    }

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("this build has it")
    }

    const IRON: &str = r#"{
        "type": "minecraft:smelting",
        "cookingtime": 200,
        "experience": 0.7,
        "ingredient": { "item": "minecraft:raw_iron" },
        "result": { "id": "minecraft:iron_ingot" }
    }"#;

    #[test]
    fn a_smelting_recipe_is_found_by_its_input_in_a_furnace_and_nowhere_else() {
        let mut cooking = Cooking::new();
        cooking
            .add(&json(IRON), &ItemTags::new())
            .expect("compiles");
        let raw = item("minecraft:raw_iron");
        let found = cooking
            .find(Fire::Furnace, raw)
            .expect("a furnace cooks it");
        assert_eq!(found.result(), (item("minecraft:iron_ingot"), 1));
        assert_eq!(found.ticks(), 200);
        assert!((found.experience() - 0.7).abs() < 1e-6);
        // The same item in a fire whose table does not hold it. A furnace
        // recipe is not a smoker recipe, and a table that answered here would
        // let a smoker melt iron.
        assert!(cooking.find(Fire::Smoker, raw).is_none());
        assert!(cooking.find(Fire::BlastFurnace, raw).is_none());
        assert_eq!(cooking.pairs(), 1);
        assert_eq!(cooking.pairs_in(Fire::Furnace), 1);
    }

    #[test]
    fn the_same_food_in_two_fires_keeps_each_fires_own_time() {
        // The case a "halve the furnace's number for a smoker" rule gets
        // wrong the moment the two files disagree about anything else.
        let mut cooking = Cooking::new();
        for text in [
            r#"{"type":"minecraft:smelting","cookingtime":200,"experience":0.35,
                "ingredient":{"item":"minecraft:beef"},"result":{"id":"minecraft:cooked_beef"}}"#,
            r#"{"type":"minecraft:smoking","cookingtime":100,"experience":0.35,
                "ingredient":{"item":"minecraft:beef"},"result":{"id":"minecraft:cooked_beef"}}"#,
            r#"{"type":"minecraft:campfire_cooking","cookingtime":600,"experience":0.35,
                "ingredient":{"item":"minecraft:beef"},"result":{"id":"minecraft:cooked_beef"}}"#,
        ] {
            cooking
                .add(&json(text), &ItemTags::new())
                .expect("compiles");
        }
        let beef = item("minecraft:beef");
        assert_eq!(
            cooking.find(Fire::Furnace, beef).map(Cooked::ticks),
            Some(200)
        );
        assert_eq!(
            cooking.find(Fire::Smoker, beef).map(Cooked::ticks),
            Some(100)
        );
        assert_eq!(
            cooking.find(Fire::Campfire, beef).map(Cooked::ticks),
            Some(600)
        );
        assert_eq!(cooking.find(Fire::BlastFurnace, beef), None);
    }

    #[test]
    fn a_tag_ingredient_cooks_every_member_of_the_tag() {
        let mut tags = ItemTags::new();
        tags.insert(
            "minecraft:logs_that_burn".to_owned(),
            vec![item("minecraft:oak_log"), item("minecraft:birch_log")],
        );
        let mut cooking = Cooking::new();
        cooking
            .add(
                &json(
                    r#"{"type":"minecraft:smelting","cookingtime":100,"experience":0.15,
                        "ingredient":{"tag":"minecraft:logs_that_burn"},
                        "result":{"id":"minecraft:charcoal"}}"#,
                ),
                &tags,
            )
            .expect("compiles");
        assert_eq!(cooking.len(), 1, "one recipe");
        assert_eq!(cooking.pairs(), 2, "two items reach it");
        for log in ["minecraft:oak_log", "minecraft:birch_log"] {
            assert_eq!(
                cooking.find(Fire::Furnace, item(log)).map(|c| c.result().0),
                Some(item("minecraft:charcoal"))
            );
        }
    }

    #[test]
    fn a_list_ingredient_cooks_every_item_in_the_list() {
        let mut cooking = Cooking::new();
        cooking
            .add(
                &json(
                    r#"{"type":"minecraft:blasting","cookingtime":100,"experience":0.1,
                        "ingredient":[{"item":"minecraft:golden_sword"},
                                      {"item":"minecraft:golden_pickaxe"}],
                        "result":{"id":"minecraft:gold_nugget"}}"#,
                ),
                &ItemTags::new(),
            )
            .expect("compiles");
        assert_eq!(cooking.pairs_in(Fire::BlastFurnace), 2);
    }

    #[test]
    fn a_grid_recipe_is_refused_the_way_a_grid_refuses_a_furnace_one() {
        let mut cooking = Cooking::new();
        let refused = cooking.add(
            &json(r#"{"type":"minecraft:crafting_shapeless","ingredients":[],"result":{}}"#),
            &ItemTags::new(),
        );
        assert!(matches!(refused, Err(Refusal::NotAGrid(_))), "{refused:?}");
    }

    #[test]
    fn a_recipe_with_no_cooking_time_is_refused_rather_than_given_a_default() {
        // A default of 200 would be right for every vanilla furnace recipe and
        // would silently make a data pack's ten-second smelt take ten seconds
        // more. A furnace is a timer a player watches.
        let mut cooking = Cooking::new();
        let refused = cooking.add(
            &json(
                r#"{"type":"minecraft:smelting","ingredient":{"item":"minecraft:raw_iron"},
                    "result":{"id":"minecraft:iron_ingot"}}"#,
            ),
            &ItemTags::new(),
        );
        assert!(matches!(refused, Err(Refusal::Malformed(_))), "{refused:?}");
    }

    #[test]
    fn a_second_recipe_for_one_input_is_counted_and_the_first_one_read_wins() {
        let mut cooking = Cooking::new();
        cooking
            .add(&json(IRON), &ItemTags::new())
            .expect("compiles");
        cooking
            .add(
                &json(
                    r#"{"type":"minecraft:smelting","cookingtime":20,"experience":0.0,
                        "ingredient":{"item":"minecraft:raw_iron"},
                        "result":{"id":"minecraft:gold_ingot"}}"#,
                ),
                &ItemTags::new(),
            )
            .expect("compiles");
        assert_eq!(cooking.collisions(), 1);
        assert_eq!(
            cooking
                .find(Fire::Furnace, item("minecraft:raw_iron"))
                .map(|c| c.result().0),
            Some(item("minecraft:iron_ingot"))
        );
    }

    #[test]
    fn every_fire_has_its_own_recipe_type_and_block_and_three_of_them_open() {
        let types: Vec<&str> = FIRES.iter().map(|f| f.recipe_type()).collect();
        let blocks: Vec<&str> = FIRES.iter().map(|f| f.block()).collect();
        assert_eq!(types.len(), 4);
        for (fire, kind) in FIRES.into_iter().zip(types) {
            assert_eq!(Fire::from_recipe_type(kind), Some(fire));
        }
        for (fire, block) in FIRES.into_iter().zip(blocks) {
            assert_eq!(Fire::from_block(block), Some(fire));
        }
        assert_eq!(FIRES.iter().filter(|f| f.menu().is_some()).count(), 3);
        assert_eq!(Fire::Campfire.menu(), None);
    }
}
