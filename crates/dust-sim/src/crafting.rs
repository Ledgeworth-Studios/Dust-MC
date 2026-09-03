//! What a grid of items makes, out of the operator's own recipe files.
//!
//! `<[data] path>/<namespace>/recipe/<name>.json` — the same directory
//! [`crate::drops`] reads loot tables from, put there by Minecraft's own
//! `--server` data generator. **No new file, no new extraction step and
//! nothing of Mojang's committed**, for exactly the reasons decision record
//! 0022 gives: a recipe is data pack content, an operator already holds it,
//! and asking them to run an extractor over a directory they are already
//! holding would be asking twice. A data pack that changes what a log makes
//! changes what Dust makes, because there was never a second copy of the
//! answer to disagree with it.
//!
//! # What the data is, measured before any of this was written
//!
//! All 1,290 files vanilla 1.21.1 ships, counted:
//!
//! ```text
//!   crafting_shaped                            634
//!   crafting_shapeless                         253
//!   stonecutting                               250
//!   smelting / blasting / smoking / campfire   112
//!   smithing_transform / smithing_trim          28
//!   crafting_special_* and crafting_decorated_pot 13
//! ```
//!
//! Of the 887 that are grid recipes:
//!
//! ```text
//!   ingredient shapes         3   {"item": id}, {"tag": id}, and a list of the first
//!   result keys               2   `id` and `count`, on every one of the 887
//!   distinct item tags used  19
//!   pattern rows              1..=3, and rows are 1..=3 wide
//!   shapeless ingredients     1..=9
//! ```
//!
//! Three ingredient shapes and two result keys is a language that can be
//! *implemented* rather than approximated, which is the same finding decision
//! record 0022 made about block loot and the same reason there is no rule here
//! and no name matching anywhere in this file.
//!
//! # The thirteen that are code, not data
//!
//! `crafting_special_firework_rocket`, `crafting_special_shulkerboxcoloring`,
//! `crafting_special_armordye` and the ten beside them are **marker files**:
//! on 1.21.1 they carry `type` and `category` and nothing else, because the
//! recipe is a Java class. There is nothing in the file to compile, so this
//! compiler refuses them by their declared type and **counts** them — it never
//! guesses at what a name implies. A player cannot dye leather armour or make
//! a firework on this server yet, and the boot line says so in a number.
//!
//! # Refusal is counted, never guessed
//!
//! An ingredient shape, a result key or a recipe type this compiler has not
//! heard of makes the recipe refuse to compile and say so. The alternative —
//! reading an unknown key as absent — is a recipe that quietly makes the wrong
//! thing, and crafting is where a wrong answer costs the player the
//! ingredients as well as the result.
//!
//! # The index, and what it costs
//!
//! A recipe lookup runs on **every grid change**, which is every click a
//! player makes while arranging ingredients. Scanning 887 recipes per click
//! would be 887 pattern matches to answer a question that usually has one
//! candidate, so the recipes are indexed by ingredient item: for each item id,
//! the recipes that can use it, as a sorted flat `u32` array with a range per
//! item. A lookup takes the grid's rarest item and tests only that item's
//! candidates.
//!
//! Both halves are flat arrays rather than maps, and the whole index is built
//! once at boot. See [`Recipes::index_len`] for what it holds on real data.

use std::collections::BTreeMap;

use dust_registry::Item;

/// The widest grid this matcher accepts, which is a crafting table's.
pub const MAX_GRID: usize = 3;

/// One ingredient: the items that satisfy it.
///
/// A range into [`Recipes::choices`] rather than a `Vec` per ingredient. The
/// vanilla data has 1,521 ingredients across 887 recipes and 61 of them are
/// lists; a `Vec` each would be 887 allocations to hold what one flat array
/// holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Choice {
    start: u32,
    len: u16,
}

/// What a compiled recipe is made of.
#[derive(Debug, Clone)]
enum Kind {
    /// A pattern, trimmed of empty edge rows and columns exactly as
    /// `ShapedRecipePattern.shrink` trims Minecraft's own.
    Shaped {
        width: u8,
        height: u8,
        /// `width * height` cells, row-major. `None` is a hole in the pattern
        /// and a hole must be empty in the grid.
        cells: Box<[Option<Choice>]>,
    },
    /// An unordered bag. The grid must hold exactly this many stacks and there
    /// must be a one-to-one assignment between them and these ingredients.
    Shapeless { ingredients: Box<[Choice]> },
}

/// One recipe this server can make.
#[derive(Debug, Clone)]
pub struct Recipe {
    /// The file's own namespaced id, for the log and for a report. Not used in
    /// matching.
    id: Box<str>,
    kind: Kind,
    result: Item,
    count: u8,
}

impl Recipe {
    /// The recipe's namespaced id — `minecraft:stick`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What one craft yields: the item and how many.
    #[must_use]
    pub fn result(&self) -> (Item, u8) {
        (self.result, self.count)
    }

    /// How many grid cells one craft consumes. Every ingredient takes exactly
    /// one item from one cell, which is true of every recipe in this language.
    #[must_use]
    pub fn cells(&self) -> usize {
        match &self.kind {
            Kind::Shaped { cells, .. } => cells.iter().filter(|cell| cell.is_some()).count(),
            Kind::Shapeless { ingredients } => ingredients.len(),
        }
    }
}

/// The items whose crafting remainder is not an item at all, which is to say
/// every item: a **crafting remainder is a Java constant**, `Item.Properties.
/// craftRemainder`, and it is in no report, no data pack and no registry — the
/// same shape as `Block.getLootTable` in decision record 0022 and
/// `Mob.getEquipmentSlotForItem` in 0016.
///
/// So it is written here, as pairs, and it is the only Minecraft-authored
/// relation in this file. Twelve items on 1.21.1: the eleven filled buckets
/// give a bucket back and a honey bottle gives a glass bottle back. Three of
/// the 887 vanilla recipes touch one — `cake`, `honey_block` and
/// `sugar_from_honey_bottle` — and a server that consumed the container
/// instead of returning it would be eating three buckets to make one cake,
/// which is the loss a player notices most and forgives least.
///
/// An item that is *not* in this table is assumed to leave nothing behind,
/// which is right for the other 1,321 items 1.21.1 ships and is the assumption
/// vanilla itself makes. Being wrong here in the other direction would destroy
/// an item, which is why the list is written out rather than derived from a
/// name: `minecraft:water_bucket` and `minecraft:bucket` differ by a rule, and
/// `minecraft:milk_bucket` and `minecraft:powder_snow_bucket` do not follow it.
const REMAINDERS: [(&str, &str); 12] = [
    ("minecraft:water_bucket", "minecraft:bucket"),
    ("minecraft:lava_bucket", "minecraft:bucket"),
    ("minecraft:milk_bucket", "minecraft:bucket"),
    ("minecraft:powder_snow_bucket", "minecraft:bucket"),
    ("minecraft:cod_bucket", "minecraft:bucket"),
    ("minecraft:salmon_bucket", "minecraft:bucket"),
    ("minecraft:pufferfish_bucket", "minecraft:bucket"),
    ("minecraft:tropical_fish_bucket", "minecraft:bucket"),
    ("minecraft:axolotl_bucket", "minecraft:bucket"),
    ("minecraft:tadpole_bucket", "minecraft:bucket"),
    ("minecraft:honey_bottle", "minecraft:glass_bottle"),
    ("minecraft:dragon_breath", "minecraft:glass_bottle"),
];

/// What one of `item` leaves behind when a craft consumes it.
///
/// The bucket you get back from a cake. See [`REMAINDERS`] for why this is a
/// written list and not a rule.
#[must_use]
pub fn remainder(item: Item) -> Option<Item> {
    static TABLE: std::sync::OnceLock<Box<[u16]>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        // Indexed by protocol id, holding the protocol id of the remainder
        // plus one so that zero can mean "nothing left behind" without an
        // `Option` per row. There are 1,333 items on 1.21.1, so this is 2.6 kB
        // built once.
        let mut table = vec![0u16; Item::registry().entry_count()];
        for (from, to) in REMAINDERS {
            if let (Some(from), Some(to)) = (Item::from_name(from), Item::from_name(to)) {
                table[from.protocol_id() as usize] = to.protocol_id() as u16 + 1;
            }
        }
        table.into_boxed_slice()
    });
    match table.get(item.protocol_id() as usize).copied() {
        None | Some(0) => None,
        Some(id) => Item::from_protocol_id(u32::from(id - 1)),
    }
}

/// Everything a grid can make.
#[derive(Debug, Default)]
pub struct Recipes {
    recipes: Vec<Recipe>,
    /// Every ingredient's accepted items, back to back. Item protocol ids.
    choices: Vec<u16>,
    /// Indexed by item protocol id: where this item's candidates start and how
    /// many there are, in `candidates`.
    by_item: Vec<(u32, u32)>,
    /// Recipe indices, grouped by item and sorted within a group.
    candidates: Vec<u32>,
}

/// Why one recipe file did not compile.
///
/// Every variant is something the compiler *knows* it does not handle, which
/// is the point: a refusal is counted and named, never read as an absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The `type` is not a crafting-grid recipe — smelting, stonecutting,
    /// smithing. Those belong to blocks this server does not open yet.
    NotAGrid(String),
    /// One of the thirteen `crafting_special_*` shapes, which is a Java class
    /// rather than a description of a grid.
    Special(String),
    /// The `type` key is missing or is not a string.
    NoType,
    /// A key whose value is not the shape the language allows.
    Malformed(&'static str),
    /// An item, tag or result name nothing in this build answers to.
    Unknown(String),
    /// A tag with no members, after resolution. An ingredient nothing can
    /// satisfy would make a recipe that can never match, and saying so is more
    /// use than holding it.
    EmptyTag(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGrid(kind) => write!(f, "{kind} is not made in a crafting grid"),
            Self::Special(kind) => write!(f, "{kind} is a code recipe, not a described one"),
            Self::NoType => write!(f, "no `type`"),
            Self::Malformed(what) => write!(f, "{what}"),
            Self::Unknown(name) => write!(f, "no such item or tag: {name}"),
            Self::EmptyTag(name) => write!(f, "tag {name} has no members"),
        }
    }
}

/// The item tags an ingredient may name, already resolved to items.
///
/// A `#minecraft:planks` in a recipe is a membership list in the operator's
/// own `tags/item/` directory, and it is resolved *there* rather than out of
/// the table this crate ships, because a data pack that adds a wood adds it to
/// that tag and crafting is the first place a player would notice it missing.
pub type ItemTags = BTreeMap<String, Vec<Item>>;

impl Recipes {
    /// Compile one recipe file.
    ///
    /// `id` is the recipe's namespaced name, `value` its parsed JSON, `tags`
    /// the resolved item tags. Returns the refusal rather than logging it, so
    /// the caller decides what a broken data pack does to the boot line.
    pub fn add(
        &mut self,
        id: &str,
        value: &serde_json::Value,
        tags: &ItemTags,
    ) -> Result<(), Refusal> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(Refusal::NoType)?;
        match kind {
            "minecraft:crafting_shaped" => self.add_shaped(id, value, tags),
            "minecraft:crafting_shapeless" => self.add_shapeless(id, value, tags),
            // The marker files. Named apart from the rest because "this server
            // cannot make a firework yet" and "this data pack has a recipe I
            // cannot read" are different sentences to an operator.
            other if other.starts_with("minecraft:crafting_") => {
                Err(Refusal::Special(other.to_owned()))
            }
            other => Err(Refusal::NotAGrid(other.to_owned())),
        }
    }

    fn add_shaped(
        &mut self,
        id: &str,
        value: &serde_json::Value,
        tags: &ItemTags,
    ) -> Result<(), Refusal> {
        let pattern = value
            .get("pattern")
            .and_then(serde_json::Value::as_array)
            .ok_or(Refusal::Malformed("`pattern` is not a list"))?;
        let keys = value
            .get("key")
            .and_then(serde_json::Value::as_object)
            .ok_or(Refusal::Malformed("`key` is not an object"))?;
        if pattern.is_empty() || pattern.len() > MAX_GRID {
            return Err(Refusal::Malformed("`pattern` is not one to three rows"));
        }
        let mut rows: Vec<&str> = Vec::with_capacity(pattern.len());
        for row in pattern {
            let row = row
                .as_str()
                .ok_or(Refusal::Malformed("a `pattern` row is not a string"))?;
            if row.chars().count() > MAX_GRID {
                return Err(Refusal::Malformed("a `pattern` row is wider than three"));
            }
            rows.push(row);
        }
        let width = rows.iter().map(|row| row.chars().count()).max().unwrap_or(0);

        // Rows are padded to the widest, exactly as Minecraft pads a short row
        // with spaces: `["#", "##"]` is a two-wide pattern whose first row has
        // a hole on the right, not a one-wide pattern.
        let mut cells: Vec<Option<Choice>> = Vec::with_capacity(width * rows.len());
        for row in &rows {
            let mut chars = row.chars();
            for _ in 0..width {
                match chars.next() {
                    None | Some(' ') => cells.push(None),
                    Some(symbol) => {
                        let mut buffer = [0u8; 4];
                        let name = symbol.encode_utf8(&mut buffer);
                        let ingredient = keys
                            .get(name)
                            .ok_or(Refusal::Malformed("a `pattern` symbol has no `key`"))?;
                        cells.push(Some(self.choice(ingredient, tags)?));
                    }
                }
            }
        }

        let (width, height, cells) = shrink(width, rows.len(), cells);
        if cells.iter().all(Option::is_none) {
            return Err(Refusal::Malformed("`pattern` is empty"));
        }
        let (result, count) = self.result(value)?;
        self.push(Recipe {
            id: id.into(),
            kind: Kind::Shaped {
                width: width as u8,
                height: height as u8,
                cells: cells.into_boxed_slice(),
            },
            result,
            count,
        });
        Ok(())
    }

    fn add_shapeless(
        &mut self,
        id: &str,
        value: &serde_json::Value,
        tags: &ItemTags,
    ) -> Result<(), Refusal> {
        let list = value
            .get("ingredients")
            .and_then(serde_json::Value::as_array)
            .ok_or(Refusal::Malformed("`ingredients` is not a list"))?;
        if list.is_empty() || list.len() > MAX_GRID * MAX_GRID {
            return Err(Refusal::Malformed("`ingredients` is not one to nine"));
        }
        let mut ingredients = Vec::with_capacity(list.len());
        for ingredient in list {
            ingredients.push(self.choice(ingredient, tags)?);
        }
        let (result, count) = self.result(value)?;
        self.push(Recipe {
            id: id.into(),
            kind: Kind::Shapeless {
                ingredients: ingredients.into_boxed_slice(),
            },
            result,
            count,
        });
        Ok(())
    }

    /// The result stack. Two keys and no others.
    ///
    /// A `components` key on a result — which a data pack may write and which
    /// 1.21.2 puts on vanilla's own — makes the recipe refuse rather than
    /// produce a plain item where a named or enchanted one was described.
    /// Handing a player a sword without the enchantment they crafted it for is
    /// the same class of loss as eating the ingredients.
    fn result(&self, value: &serde_json::Value) -> Result<(Item, u8), Refusal> {
        let result = value
            .get("result")
            .and_then(serde_json::Value::as_object)
            .ok_or(Refusal::Malformed("`result` is not an object"))?;
        for key in result.keys() {
            if key != "id" && key != "count" {
                return Err(Refusal::Malformed("`result` carries more than a stack"));
            }
        }
        let name = result
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or(Refusal::Malformed("`result` has no `id`"))?;
        let item = Item::from_name(name).ok_or_else(|| Refusal::Unknown(name.to_owned()))?;
        let count = match result.get("count") {
            None => 1,
            Some(count) => u8::try_from(
                count
                    .as_u64()
                    .ok_or(Refusal::Malformed("`result`'s `count` is not a number"))?,
            )
            .map_err(|_| Refusal::Malformed("`result`'s `count` does not fit a stack"))?,
        };
        if count == 0 || count > item.max_stack_size() {
            return Err(Refusal::Malformed("`result`'s `count` does not fit a stack"));
        }
        Ok((item, count))
    }

    /// One ingredient's accepted items, appended to the flat pool.
    fn choice(&mut self, value: &serde_json::Value, tags: &ItemTags) -> Result<Choice, Refusal> {
        let start = self.choices.len();
        match value {
            serde_json::Value::Object(_) => self.one(value, tags)?,
            serde_json::Value::Array(list) => {
                for one in list {
                    self.one(one, tags)?;
                }
            }
            _ => return Err(Refusal::Malformed("an ingredient is not an object or list")),
        }
        // Sorted and deduplicated so that a lookup can binary-search a choice
        // and so that `#planks` listed twice costs one entry.
        self.choices[start..].sort_unstable();
        let end = start + dedup(&mut self.choices[start..]);
        self.choices.truncate(end);
        let len = end - start;
        if len == 0 {
            return Err(Refusal::Malformed("an ingredient accepts nothing"));
        }
        Ok(Choice {
            start: start as u32,
            len: u16::try_from(len).map_err(|_| Refusal::Malformed("an ingredient is enormous"))?,
        })
    }

    /// One `{"item": …}` or `{"tag": …}`, pushed onto the pool.
    fn one(&mut self, value: &serde_json::Value, tags: &ItemTags) -> Result<(), Refusal> {
        let object = value
            .as_object()
            .ok_or(Refusal::Malformed("an ingredient is not an object"))?;
        if object.len() != 1 {
            return Err(Refusal::Malformed("an ingredient is not one of item or tag"));
        }
        if let Some(name) = object.get("item").and_then(serde_json::Value::as_str) {
            let item = Item::from_name(name).ok_or_else(|| Refusal::Unknown(name.to_owned()))?;
            self.choices.push(item.protocol_id() as u16);
            return Ok(());
        }
        if let Some(name) = object.get("tag").and_then(serde_json::Value::as_str) {
            let members = tags
                .get(name)
                .ok_or_else(|| Refusal::Unknown(format!("#{name}")))?;
            if members.is_empty() {
                return Err(Refusal::EmptyTag(name.to_owned()));
            }
            self.choices
                .extend(members.iter().map(|item| item.protocol_id() as u16));
            return Ok(());
        }
        Err(Refusal::Malformed("an ingredient is not one of item or tag"))
    }

    fn push(&mut self, recipe: Recipe) {
        self.recipes.push(recipe);
    }

    /// Build the item-to-recipe index. Call once, after the last [`add`].
    ///
    /// [`add`]: Recipes::add
    pub fn index(&mut self) {
        let items = Item::registry().entry_count();
        let mut counts = vec![0u32; items];
        let mut seen = vec![u32::MAX; items];
        // Two passes and no allocation per recipe: count, then fill. `seen`
        // holds the recipe index that last touched an item, so a recipe naming
        // planks in three ingredients is counted once.
        for (index, recipe) in self.recipes.iter().enumerate() {
            for choice in choices_of(recipe) {
                for &id in &self.choices[choice.start as usize..][..choice.len as usize] {
                    let id = id as usize;
                    if id < items && seen[id] != index as u32 {
                        seen[id] = index as u32;
                        counts[id] += 1;
                    }
                }
            }
        }
        self.by_item = Vec::with_capacity(items);
        let mut start = 0u32;
        for count in &counts {
            self.by_item.push((start, *count));
            start += count;
        }
        self.candidates = vec![0u32; start as usize];
        let mut filled = vec![0u32; items];
        seen.fill(u32::MAX);
        for (index, recipe) in self.recipes.iter().enumerate() {
            for choice in choices_of(recipe) {
                for &id in &self.choices[choice.start as usize..][..choice.len as usize] {
                    let id = id as usize;
                    if id < items && seen[id] != index as u32 {
                        seen[id] = index as u32;
                        let at = self.by_item[id].0 + filled[id];
                        self.candidates[at as usize] = index as u32;
                        filled[id] += 1;
                    }
                }
            }
        }
    }

    /// How many recipes compiled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// Whether nothing compiled, which is what a server with no `[data] path`
    /// has and is why the crafting output never fills there.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// How many entries the index holds — one per (item, recipe) pair. This is
    /// the number the boot line prints, because it is what the index costs to
    /// hold: four bytes each, plus eight per item id for the ranges.
    #[must_use]
    pub fn index_len(&self) -> usize {
        self.candidates.len()
    }

    /// How many item slots the ingredient pool holds, across every ingredient
    /// of every recipe. Two bytes each.
    #[must_use]
    pub fn choice_len(&self) -> usize {
        self.choices.len()
    }

    /// What this grid makes, or `None`.
    ///
    /// `cells` is row-major, `width * height` long, `width` and `height` at
    /// most three. A cell holds the *item* in that slot and not how many —
    /// every ingredient in this language takes exactly one, so a stack of
    /// sixty-four planks and a single plank satisfy the same ingredient.
    ///
    /// Components are not compared. On 1.21.1 an ingredient is `{"item": id}`
    /// or `{"tag": id}` and nothing else — measured, not assumed — so a
    /// renamed log is still a log to a recipe, which is what a real server
    /// does with one.
    #[must_use]
    pub fn find(&self, width: usize, height: usize, cells: &[Option<Item>]) -> Option<&Recipe> {
        if width == 0 || height == 0 || width > MAX_GRID || height > MAX_GRID {
            return None;
        }
        if cells.len() != width * height {
            return None;
        }
        // The rarest item in the grid decides which recipes are even worth
        // testing. Iron is in nine recipes and a plank is in a hundred and
        // fifty, so a grid holding both tests nine.
        let mut rarest: Option<(u32, u32)> = None;
        for cell in cells.iter().flatten() {
            let id = cell.protocol_id() as usize;
            let (start, len) = *self.by_item.get(id)?;
            if len == 0 {
                return None;
            }
            if rarest.is_none_or(|(_, best)| len < best) {
                rarest = Some((start, len));
            }
        }
        let (start, len) = rarest?;
        for &candidate in &self.candidates[start as usize..][..len as usize] {
            let recipe = &self.recipes[candidate as usize];
            if self.matches(recipe, width, height, cells) {
                return Some(recipe);
            }
        }
        None
    }

    fn matches(&self, recipe: &Recipe, width: usize, height: usize, cells: &[Option<Item>]) -> bool {
        match &recipe.kind {
            Kind::Shaped {
                width: rw,
                height: rh,
                cells: pattern,
            } => self.shaped(*rw as usize, *rh as usize, pattern, width, height, cells),
            Kind::Shapeless { ingredients } => self.shapeless(ingredients, cells),
        }
    }

    /// Minecraft's `ShapedRecipe.matches`: the grid is trimmed to the box its
    /// stacks occupy, the pattern is already trimmed, and the two must be the
    /// same size — **either way round**, because a shaped recipe also matches
    /// mirrored. A door built left-handed is the same door.
    fn shaped(
        &self,
        rw: usize,
        rh: usize,
        pattern: &[Option<Choice>],
        width: usize,
        height: usize,
        cells: &[Option<Item>],
    ) -> bool {
        let Some((left, top, bw, bh)) = bounds(width, height, cells) else {
            return false;
        };
        if bw != rw || bh != rh {
            return false;
        }
        for mirrored in [false, true] {
            let mut all = true;
            for y in 0..rh {
                for x in 0..rw {
                    let want = pattern[y * rw + if mirrored { rw - 1 - x } else { x }];
                    let have = cells[(top + y) * width + left + x].as_ref();
                    let ok = match (want, have) {
                        (None, None) => true,
                        (Some(choice), Some(item)) => self.accepts(choice, *item),
                        _ => false,
                    };
                    if !ok {
                        all = false;
                        break;
                    }
                }
                if !all {
                    break;
                }
            }
            if all {
                return true;
            }
        }
        false
    }

    /// Minecraft's `ShapelessRecipe.matches`: as many stacks as ingredients,
    /// and a one-to-one assignment between them.
    ///
    /// The assignment is a bipartite matching by augmenting paths, not a
    /// greedy pass. Greedy is wrong and wrong in the direction that matters: a
    /// grid holding an oak plank and an oak log against a recipe wanting
    /// `#planks` and `#logs` can be assigned greedily so that `#logs` takes the
    /// oak log first and `#planks` is left with nothing — a recipe that plainly
    /// matches, refused. Nine by nine is at most 81 edges, so exactness is free.
    fn shapeless(&self, ingredients: &[Choice], cells: &[Option<Item>]) -> bool {
        let mut items = [None; MAX_GRID * MAX_GRID];
        let mut count = 0;
        for item in cells.iter().flatten() {
            items[count] = Some(*item);
            count += 1;
        }
        if count != ingredients.len() {
            return false;
        }
        // `taken[i]` is the ingredient that item `i` is assigned to, or
        // `usize::MAX` for an item nothing has claimed yet.
        let mut taken = [usize::MAX; MAX_GRID * MAX_GRID];
        for ingredient in 0..ingredients.len() {
            let mut visited = [false; MAX_GRID * MAX_GRID];
            if !self.assign(ingredient, ingredients, &items[..count], &mut taken, &mut visited) {
                return false;
            }
        }
        true
    }

    /// One augmenting step: find an item for this ingredient, moving another
    /// ingredient onto a different item if that is what it takes.
    fn assign(
        &self,
        ingredient: usize,
        ingredients: &[Choice],
        items: &[Option<Item>],
        taken: &mut [usize],
        visited: &mut [bool],
    ) -> bool {
        for (index, item) in items.iter().enumerate() {
            let Some(item) = item else { continue };
            if visited[index] || !self.accepts(ingredients[ingredient], *item) {
                continue;
            }
            visited[index] = true;
            let held = taken[index];
            if held == usize::MAX || self.assign(held, ingredients, items, taken, visited) {
                // The recursion above has already moved `held` elsewhere, or
                // there was nothing here. Either way this item is now ours.
                taken[index] = ingredient;
                return true;
            }
        }
        false
    }

    fn accepts(&self, choice: Choice, item: Item) -> bool {
        let id = item.protocol_id() as u16;
        self.choices[choice.start as usize..][..choice.len as usize]
            .binary_search(&id)
            .is_ok()
    }
}

fn choices_of(recipe: &Recipe) -> impl Iterator<Item = &Choice> {
    let (shaped, shapeless): (&[Option<Choice>], &[Choice]) = match &recipe.kind {
        Kind::Shaped { cells, .. } => (cells, &[]),
        Kind::Shapeless { ingredients } => (&[], ingredients),
    };
    shaped.iter().flatten().chain(shapeless.iter())
}

/// The box the grid's stacks occupy: left, top, width, height. `None` for an
/// empty grid.
fn bounds(width: usize, height: usize, cells: &[Option<Item>]) -> Option<(usize, usize, usize, usize)> {
    let (mut left, mut top) = (usize::MAX, usize::MAX);
    let (mut right, mut bottom) = (0usize, 0usize);
    for y in 0..height {
        for x in 0..width {
            if cells[y * width + x].is_some() {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    (left != usize::MAX).then(|| (left, top, right - left + 1, bottom - top + 1))
}

/// Trim empty edge rows and columns, which is what
/// `ShapedRecipePattern.shrink` does to Minecraft's own patterns before it
/// stores them. Both sides of a match are trimmed, so a pattern written with a
/// blank first row is the same pattern as one written without.
fn shrink(
    width: usize,
    height: usize,
    cells: Vec<Option<Choice>>,
) -> (usize, usize, Vec<Option<Choice>>) {
    let (mut left, mut top) = (usize::MAX, usize::MAX);
    let (mut right, mut bottom) = (0usize, 0usize);
    for y in 0..height {
        for x in 0..width {
            if cells[y * width + x].is_some() {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    if left == usize::MAX {
        return (0, 0, Vec::new());
    }
    let (w, h) = (right - left + 1, bottom - top + 1);
    let mut out = Vec::with_capacity(w * h);
    for y in top..=bottom {
        for x in left..=right {
            out.push(cells[y * width + x]);
        }
    }
    (w, h, out)
}

/// Deduplicate a sorted slice in place, returning how many are left.
fn dedup(values: &mut [u16]) -> usize {
    let mut kept = 0;
    for index in 0..values.len() {
        if kept == 0 || values[index] != values[kept - 1] {
            values[kept] = values[index];
            kept += 1;
        }
    }
    kept
}
