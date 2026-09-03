//! What a stack carries besides its item and its count.
//!
//! # The wall this module is here to get past
//!
//! [`crate::types::Slot`] used to refuse every component and say why: **a
//! component carries no length.** It is a VarInt type id followed by that
//! type's own layout, fifty-seven of them, each different. A reader that meets
//! one it cannot walk does not lose that component — it loses the position of
//! every field after it, and a desynchronised body corrupts the rest of the
//! packet rather than the field it failed on.
//!
//! That argument is still true. What it does not say is that walking a
//! component is *much* less work than modelling one. To store a name, an
//! enchantment or a shulker box's contents, this server does not need to know
//! what any of those mean; it needs to know where each of them **ends**. So
//! that is all this module does: [`measure`] walks a component's bytes and
//! returns their length, and the bytes themselves are kept, compared and sent
//! back exactly as they arrived.
//!
//! # Why the layouts are written here and the numbers are not
//!
//! The fifty-seven layouts are protocol knowledge, the same kind as the packet
//! bodies in `packets/`, and they are written by hand for the same reason: they
//! are in none of Mojang's reports. They are keyed by **name**.
//!
//! The names' protocol *ids* are Minecraft's data and are not written down
//! here. `minecraft:data_component_type` is a real registry, it is extracted
//! from the operator's own jar, and [`install_type_names`] is how the id-to-name
//! half reaches this crate — `dust-protocol` sits at the bottom of the
//! dependency graph and cannot reach `dust-registry`, so the lookup is handed
//! in at boot rather than duplicated here. A hand-written `custom_data = 0`
//! would be a second answer to a question the registry already answers, and it
//! would go stale on its own.
//!
//! Until the lookup is installed, every component is refused by number. That is
//! deliberate and it is checked: a build that forgot to install it fails
//! loudly on the first named item rather than dropping names quietly.
//!
//! # Canonical form, and why equality is byte equality
//!
//! Two stacks merge only if their components are equal. Vanilla compares parsed
//! values; this compares the bytes. The two differ in exactly one direction:
//! two patches that mean the same thing can be spelled differently (a different
//! order in the map, a different key order inside an NBT compound) and would
//! compare unequal here. **That is the safe direction.** Distinct values cannot
//! collide, because every component codec is injective, so byte equality never
//! merges two stacks that vanilla would keep apart — it can only fail to merge
//! two that vanilla would join, which a player sees as two stacks of thirty-two
//! rather than as an item that was destroyed or duplicated.
//!
//! To keep even that from happening for the ordinary case, a patch is stored in
//! a **canonical form**: entries sorted by type id, removals sorted, duplicates
//! refused. Two patches built from the same components by two different clients
//! land on the same bytes.
//!
//! # What it costs
//!
//! [`ComponentPatch`] is one `Option<Arc<[u8]>>` — eight bytes in a [`Stack`],
//! `None` for the overwhelming majority of stacks, which allocate nothing and
//! compare in one branch. Sending a slot is a `memcpy` of bytes that were
//! already validated; nothing is re-encoded per send and nothing is parsed per
//! read.
//!
//! [`Stack`]: https://docs.rs/dust-server

use std::sync::{Arc, OnceLock};

use crate::nbt;
use crate::varint;
use crate::wire::{read_var, DecodeError, EncodeError, WireRead, WireWrite};

/// The largest patch this server will hold for one stack.
///
/// A shulker box's `container` component is twenty-seven stacks, each of which
/// may carry components of its own, so the honest bound is not small. This one
/// is a refusal of the absurd rather than a model of the plausible: a client
/// that sends a megabyte of components for one slot is not a client.
pub const MAX_PATCH_BYTES: usize = 64 * 1024;

/// How deep a component may nest before it is refused.
///
/// `container` holds stacks, which hold `container`, which holds stacks. A
/// depth limit is the only thing between that and a stack overflow, and it is
/// reached by a hostile packet rather than by a real item: vanilla's own
/// shulker box cannot hold a shulker box.
const MAX_DEPTH: u32 = 16;

/// Resolves a data-component type's protocol id to its registry name.
///
/// Installed once, at boot, by whoever can see the registry. A plain function
/// pointer rather than a table so that installing it allocates nothing and
/// leaks nothing.
static TYPE_NAMES: OnceLock<fn(i32) -> Option<&'static str>> = OnceLock::new();

/// Tell this crate how to name a data-component type id.
///
/// Returns `false` if a lookup was already installed, which is not an error —
/// a second server on the same process would install the same one.
pub fn install_type_names(lookup: fn(i32) -> Option<&'static str>) -> bool {
    TYPE_NAMES.set(lookup).is_ok()
}

/// The registry name of a data-component type id, if one is installed.
#[must_use]
pub fn type_name(id: i32) -> Option<&'static str> {
    TYPE_NAMES.get().and_then(|lookup| lookup(id))
}

/// Whether a lookup has been installed.
#[must_use]
pub fn type_names_installed() -> bool {
    TYPE_NAMES.get().is_some()
}

// ---------------------------------------------------------------------------
// The patch
// ---------------------------------------------------------------------------

/// The data components a stack adds to, or removes from, its item's defaults.
///
/// Held as the canonical wire bytes of the whole patch — the added count, the
/// removed count, the added entries in type-id order, then the removed type
/// ids in order. That is a valid wire tail, so encoding is a copy.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ComponentPatch(Option<Arc<[u8]>>);

impl ComponentPatch {
    /// No components. The default, and what almost every stack carries.
    pub const EMPTY: Self = Self(None);

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// The canonical wire tail, ready to be written after the item id.
    #[must_use]
    pub fn as_wire_bytes(&self) -> &[u8] {
        /// Two zero VarInts: no additions, no removals.
        const NOTHING: &[u8] = &[0, 0];
        match &self.0 {
            None => NOTHING,
            Some(bytes) => bytes,
        }
    }

    /// How many bytes this patch occupies on the wire.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        self.as_wire_bytes().len()
    }

    /// The patch as lowercase hex, or `None` when it is empty.
    ///
    /// Used by the save file, which writes it beside the item's name.
    #[must_use]
    pub fn to_hex(&self) -> Option<String> {
        let bytes = self.0.as_ref()?;
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes.iter() {
            use std::fmt::Write;
            let _ = write!(text, "{byte:02x}");
        }
        Some(text)
    }

    /// Read a patch back from the hex a save wrote.
    ///
    /// Re-validated rather than trusted: a save is a file an operator can edit,
    /// and bytes that are not a patch must not become one.
    pub fn from_hex(text: &str) -> Result<Self, DecodeError> {
        if text.len() % 2 != 0 {
            return Err(DecodeError::Nbt {
                why: "a component patch's hex has an odd number of digits",
            });
        }
        let mut bytes = Vec::with_capacity(text.len() / 2);
        for index in (0..text.len()).step_by(2) {
            let pair = text.get(index..index + 2).ok_or(DecodeError::NotUtf8)?;
            bytes.push(u8::from_str_radix(pair, 16).map_err(|_| DecodeError::Nbt {
                why: "a component patch's hex has a digit that is not one",
            })?);
        }
        Self::from_wire_bytes(&bytes)
    }

    /// A patch that only strips components from an item's defaults.
    ///
    /// The one constructor that needs no layout table, because a removal is a
    /// bare type id.
    #[must_use]
    pub fn removing(ids: &[i32]) -> Self {
        if ids.is_empty() {
            return Self::EMPTY;
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut out = Vec::with_capacity(2 + sorted.len());
        varint::write_var_int(0, &mut out);
        varint::write_var_int(sorted.len() as i32, &mut out);
        for id in sorted {
            varint::write_var_int(id, &mut out);
        }
        Self(Some(Arc::from(out.into_boxed_slice())))
    }

    /// Parse and canonicalise a patch from its wire tail.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut cursor = Cursor::new(bytes);
        let patch = Self::read(&mut cursor)?;
        if cursor.left() != 0 {
            return Err(DecodeError::TrailingBytes { left: cursor.left() });
        }
        Ok(patch)
    }

    /// Read a patch from a packet body, canonicalising as it goes.
    pub fn decode<R: WireRead + ?Sized>(input: &mut R) -> Result<Self, DecodeError> {
        // The patch has no length prefix, so the only way to find its end is to
        // walk it. Walking it out of the body's remaining bytes and then
        // consuming exactly what was walked keeps that in one place.
        let rest = input.peek();
        let mut cursor = Cursor::new(rest);
        let patch = Self::read(&mut cursor)?;
        let used = cursor.at;
        input.read_slice(used)?;
        Ok(patch)
    }

    fn read(cursor: &mut Cursor<'_>) -> Result<Self, DecodeError> {
        let added = cursor.count("added components")?;
        let removed = cursor.count("removed components")?;
        if added == 0 && removed == 0 {
            return Ok(Self::EMPTY);
        }
        // (type id, payload range) for each addition, in the order they arrived.
        let mut entries: Vec<(i32, usize, usize)> = Vec::with_capacity(added.min(64));
        for _ in 0..added {
            let id = cursor.var_int()?;
            let Some(name) = type_name(id) else {
                return Err(unknown_type(id));
            };
            let start = cursor.at;
            let len = measure_at(name, &cursor.bytes[start..], 0)?;
            cursor.skip(len)?;
            entries.push((id, start, cursor.at));
        }
        // A removal is a bare type id and carries no payload, so walking it
        // needs no layout and none is demanded. A client stripping a component
        // this build has never heard of is asking for something perfectly
        // well-formed, and refusing it would be this crate's ignorance costing
        // the player a click.
        let mut removals: Vec<i32> = Vec::with_capacity(removed.min(64));
        for _ in 0..removed {
            removals.push(cursor.var_int()?);
        }

        entries.sort_unstable_by_key(|(id, _, _)| *id);
        removals.sort_unstable();
        if entries.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(DecodeError::Nbt {
                why: "a component patch names one component type twice",
            });
        }
        if removals.windows(2).any(|w| w[0] == w[1]) {
            return Err(DecodeError::Nbt {
                why: "a component patch removes one component type twice",
            });
        }
        // A type cannot be both set and unset; vanilla's patch is a map and
        // cannot express it. Refused rather than resolved, because either
        // resolution would be this crate inventing what the client meant.
        if removals
            .iter()
            .any(|id| entries.binary_search_by_key(id, |(id, _, _)| *id).is_ok())
        {
            return Err(DecodeError::Nbt {
                why: "a component patch both sets and removes one component type",
            });
        }

        let mut out = Vec::with_capacity(cursor.at.min(MAX_PATCH_BYTES));
        varint::write_var_int(entries.len() as i32, &mut out);
        varint::write_var_int(removals.len() as i32, &mut out);
        for (id, start, end) in &entries {
            varint::write_var_int(*id, &mut out);
            out.extend_from_slice(&cursor.bytes[*start..*end]);
        }
        for id in &removals {
            varint::write_var_int(*id, &mut out);
        }
        if out.len() > MAX_PATCH_BYTES {
            return Err(DecodeError::NegativeLength {
                field: "component patch",
                value: -1,
            });
        }
        Ok(Self(Some(Arc::from(out.into_boxed_slice()))))
    }

    /// Write the patch's canonical bytes.
    pub fn encode<W: WireWrite + ?Sized>(&self, out: &mut W) -> Result<(), EncodeError> {
        out.write_slice(self.as_wire_bytes());
        Ok(())
    }
}

impl std::fmt::Debug for ComponentPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => f.write_str("ComponentPatch(empty)"),
            Some(bytes) => write!(f, "ComponentPatch({} bytes)", bytes.len()),
        }
    }
}

fn unknown_type(id: i32) -> DecodeError {
    if type_names_installed() {
        DecodeError::UnknownVariant {
            name: "data component type",
            value: id,
        }
    } else {
        DecodeError::Unsupported {
            field: "Slot components",
            why: "no data-component type registry was installed, so a component type id \
                  cannot be named and therefore cannot be walked",
        }
    }
}

// ---------------------------------------------------------------------------
// Walking one component
// ---------------------------------------------------------------------------

/// How many bytes the component named `name` occupies at the start of `bytes`.
///
/// The layouts are 1.21.1's. A name this build does not have a layout for is
/// [`DecodeError::Unsupported`] naming that component, which is the only honest
/// answer: the next field's position is unknown from that byte on.
pub fn measure(name: &str, bytes: &[u8]) -> Result<usize, DecodeError> {
    measure_at(name, bytes, 0)
}

fn measure_at(name: &str, bytes: &[u8], depth: u32) -> Result<usize, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError::Nbt {
            why: "a component nests deeper than this server will walk",
        });
    }
    let mut c = Cursor::new(bytes);
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    match short {
        // Whole values that are one network-NBT tag.
        "custom_data"
        | "custom_name"
        | "item_name"
        | "map_decorations"
        | "debug_stick_state"
        | "entity_data"
        | "bucket_entity_data"
        | "block_entity_data"
        | "recipes"
        | "lock"
        | "container_loot"
        | "intangible_projectile" => c.nbt()?,

        // A single VarInt.
        "max_stack_size" | "max_damage" | "damage" | "rarity" | "custom_model_data"
        | "repair_cost" | "map_id" | "map_post_processing" | "ominous_bottle_amplifier"
        | "base_color" => {
            c.var_int()?;
        }

        // A single byte.
        "unbreakable" | "enchantment_glint_override" => c.skip(1)?,

        // Nothing at all. Present or absent is the whole value.
        "hide_additional_tooltip" | "hide_tooltip" | "creative_slot_lock" | "fire_resistant" => {}

        // A single big-endian i32.
        "map_color" => c.skip(4)?,

        // A single length-prefixed string.
        "note_block_sound" => c.string()?,

        "lore" => {
            let n = c.count("lore lines")?;
            for _ in 0..n {
                c.nbt()?;
            }
        }
        "enchantments" | "stored_enchantments" => {
            let n = c.count("enchantments")?;
            for _ in 0..n {
                c.var_int()?;
                c.var_int()?;
            }
            c.skip(1)?;
        }
        "can_place_on" | "can_break" => {
            let n = c.count("block predicates")?;
            for _ in 0..n {
                // Optional<HolderSet<Block>>
                if c.bool()? {
                    c.holder_set()?;
                }
                // Optional<list of property matchers>
                if c.bool()? {
                    let properties = c.count("block properties")?;
                    for _ in 0..properties {
                        c.string()?;
                        if c.bool()? {
                            c.string()?;
                        } else {
                            c.string()?;
                            c.string()?;
                        }
                    }
                }
                // The NBT matcher is a bare tag, which may be TAG_End.
                c.nbt()?;
            }
            c.skip(1)?;
        }
        "attribute_modifiers" => {
            let n = c.count("attribute modifiers")?;
            for _ in 0..n {
                c.var_int()?; // attribute
                c.string()?; // the modifier's own id
                c.skip(8)?; // amount, f64
                c.var_int()?; // operation
                c.var_int()?; // which slot it applies in
            }
            c.skip(1)?;
        }
        "food" => {
            c.var_int()?; // nutrition
            c.skip(4)?; // saturation, f32
            c.skip(1)?; // can always eat
            c.skip(4)?; // seconds to eat, f32
            c.slot(depth + 1)?; // what eating it leaves behind
            let n = c.count("food effects")?;
            for _ in 0..n {
                c.mob_effect(depth + 1)?;
                c.skip(4)?; // probability, f32
            }
        }
        "tool" => {
            let n = c.count("tool rules")?;
            for _ in 0..n {
                c.holder_set()?;
                if c.bool()? {
                    c.skip(4)?; // speed, f32
                }
                if c.bool()? {
                    c.skip(1)?; // whether it drops
                }
            }
            c.skip(4)?; // default mining speed, f32
            c.var_int()?; // damage per block
        }
        "dyed_color" => {
            c.skip(4)?; // colour, i32
            c.skip(1)?;
        }
        "charged_projectiles" | "bundle_contents" | "container" => {
            let n = c.count("contained stacks")?;
            for _ in 0..n {
                c.slot(depth + 1)?;
            }
        }
        "potion_contents" => {
            if c.bool()? {
                c.var_int()?; // the potion
            }
            if c.bool()? {
                c.skip(4)?; // custom colour, i32
            }
            let n = c.count("potion effects")?;
            for _ in 0..n {
                c.var_int()?;
                c.mob_effect_details(depth + 1)?;
            }
            if c.bool()? {
                c.string()?; // custom name
            }
        }
        "suspicious_stew_effects" => {
            let n = c.count("stew effects")?;
            for _ in 0..n {
                c.var_int()?;
                c.var_int()?;
            }
        }
        "writable_book_content" => {
            let n = c.count("book pages")?;
            for _ in 0..n {
                c.string()?;
                if c.bool()? {
                    c.string()?;
                }
            }
        }
        "written_book_content" => {
            c.string()?; // title
            if c.bool()? {
                c.string()?; // filtered title
            }
            c.string()?; // author
            c.var_int()?; // generation
            let n = c.count("book pages")?;
            for _ in 0..n {
                c.nbt()?;
                c.nbt()?;
            }
            c.skip(1)?; // resolved
        }
        "trim" => {
            // Holder<TrimMaterial>
            if c.holder()? {
                c.string()?; // asset name
                c.var_int()?; // ingredient item
                let overrides = c.count("trim overrides")?;
                for _ in 0..overrides {
                    c.string()?;
                    c.string()?;
                }
                c.nbt()?; // description
            }
            // Holder<TrimPattern>
            if c.holder()? {
                c.string()?; // asset id
                c.var_int()?; // template item
                c.nbt()?; // description
                c.skip(1)?; // decal
            }
            c.skip(1)?;
        }
        "instrument" => {
            if c.holder()? {
                c.sound_event()?;
                c.skip(4)?; // use duration, f32
                c.skip(4)?; // range, f32
                c.nbt()?; // description
            }
        }
        "jukebox_playable" => {
            if c.bool()? {
                if c.holder()? {
                    c.sound_event()?;
                    c.nbt()?; // description
                    c.skip(4)?; // length in seconds, f32
                    c.var_int()?; // comparator output
                }
            } else {
                c.string()?; // the song by name
            }
            c.skip(1)?;
        }
        "lodestone_tracker" => {
            if c.bool()? {
                c.string()?; // dimension
                c.skip(8)?; // the position, one packed i64
            }
            c.skip(1)?; // tracked
        }
        "firework_explosion" => c.firework_explosion()?,
        "fireworks" => {
            c.var_int()?; // flight duration
            let n = c.count("firework explosions")?;
            for _ in 0..n {
                c.firework_explosion()?;
            }
        }
        "profile" => {
            if c.bool()? {
                c.string()?; // name
            }
            if c.bool()? {
                c.skip(16)?; // uuid
            }
            let n = c.count("profile properties")?;
            for _ in 0..n {
                c.string()?;
                c.string()?;
                if c.bool()? {
                    c.string()?;
                }
            }
        }
        "banner_patterns" => {
            let n = c.count("banner layers")?;
            for _ in 0..n {
                if c.holder()? {
                    c.string()?; // asset id
                    c.string()?; // translation key
                }
                c.var_int()?; // dye colour
            }
        }
        "pot_decorations" => {
            let n = c.count("pot decorations")?;
            for _ in 0..n {
                c.var_int()?;
            }
        }
        "block_state" => {
            let n = c.count("block state properties")?;
            for _ in 0..n {
                c.string()?;
                c.string()?;
            }
        }
        "bees" => {
            let n = c.count("bees")?;
            for _ in 0..n {
                c.nbt()?;
                c.var_int()?;
                c.var_int()?;
            }
        }
        _ => {
            return Err(DecodeError::Unsupported {
                field: "Slot components",
                why: "this build has no layout for that data component type, and a component \
                      carries no length, so the rest of the packet cannot be found",
            })
        }
    }
    Ok(c.at)
}

// ---------------------------------------------------------------------------
// The cursor the layouts are written against
// ---------------------------------------------------------------------------

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn left(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn skip(&mut self, len: usize) -> Result<(), DecodeError> {
        let end = self.at.checked_add(len).ok_or(DecodeError::Nbt {
            why: "a component length overflowed",
        })?;
        if end > self.bytes.len() {
            return Err(DecodeError::UnexpectedEnd {
                wanted: len,
                remaining: self.left(),
            });
        }
        self.at = end;
        Ok(())
    }

    fn var_int(&mut self) -> Result<i32, DecodeError> {
        let (value, used) = read_var(self.bytes, self.at, 32)?;
        self.at += used;
        Ok(value as i32)
    }

    fn bool(&mut self) -> Result<bool, DecodeError> {
        let start = self.at;
        self.skip(1)?;
        Ok(self.bytes[start] != 0)
    }

    /// A count prefix. Bounded by what is left, so a hostile length cannot make
    /// this allocate or loop past the buffer before the first read fails.
    fn count(&mut self, field: &'static str) -> Result<usize, DecodeError> {
        let value = self.var_int()?;
        let count = usize::try_from(value).map_err(|_| DecodeError::NegativeLength {
            field,
            value,
        })?;
        if count > self.left() {
            return Err(DecodeError::UnexpectedEnd {
                wanted: count,
                remaining: self.left(),
            });
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<(), DecodeError> {
        let len = self.count("component string")?;
        self.skip(len)
    }

    fn nbt(&mut self) -> Result<(), DecodeError> {
        let len = nbt::scan(&self.bytes[self.at..])?;
        self.skip(len)
    }

    /// A registry holder: a VarInt that is either zero, meaning the value is
    /// written out inline, or one more than the entry's id. Returns whether the
    /// caller has to walk the inline form.
    fn holder(&mut self) -> Result<bool, DecodeError> {
        Ok(self.var_int()? == 0)
    }

    /// A holder set: a VarInt that is either zero, meaning a tag name follows,
    /// or one more than the number of ids that follow.
    fn holder_set(&mut self) -> Result<(), DecodeError> {
        let head = self.var_int()?;
        if head == 0 {
            return self.string();
        }
        let count = usize::try_from(head - 1).map_err(|_| DecodeError::NegativeLength {
            field: "holder set",
            value: head,
        })?;
        if count > self.left() {
            return Err(DecodeError::UnexpectedEnd {
                wanted: count,
                remaining: self.left(),
            });
        }
        for _ in 0..count {
            self.var_int()?;
        }
        Ok(())
    }

    fn sound_event(&mut self) -> Result<(), DecodeError> {
        if self.holder()? {
            self.string()?;
            if self.bool()? {
                self.skip(4)?; // fixed range, f32
            }
        }
        Ok(())
    }

    fn firework_explosion(&mut self) -> Result<(), DecodeError> {
        self.var_int()?; // shape
        let colours = self.count("firework colours")?;
        self.skip(colours.checked_mul(4).ok_or(DecodeError::Nbt {
            why: "a firework colour count overflowed",
        })?)?;
        let fades = self.count("firework fade colours")?;
        self.skip(fades.checked_mul(4).ok_or(DecodeError::Nbt {
            why: "a firework colour count overflowed",
        })?)?;
        self.skip(2) // trail, twinkle
    }

    fn mob_effect(&mut self, depth: u32) -> Result<(), DecodeError> {
        self.var_int()?;
        self.mob_effect_details(depth)
    }

    /// An effect's details, which contain an optional *further* effect. The
    /// recursion is real — a beacon's hidden effect chains — and is what the
    /// depth limit is for.
    fn mob_effect_details(&mut self, depth: u32) -> Result<(), DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::Nbt {
                why: "a component nests deeper than this server will walk",
            });
        }
        self.var_int()?; // amplifier
        self.var_int()?; // duration
        self.skip(3)?; // ambient, particles, icon
        if self.bool()? {
            self.mob_effect_details(depth + 1)?;
        }
        Ok(())
    }

    /// A whole nested stack, components and all.
    fn slot(&mut self, depth: u32) -> Result<(), DecodeError> {
        let count = self.var_int()?;
        if count <= 0 {
            return Ok(());
        }
        self.var_int()?; // item id
        let added = self.count("added components")?;
        let removed = self.count("removed components")?;
        for _ in 0..added {
            let id = self.var_int()?;
            let Some(name) = type_name(id) else {
                return Err(unknown_type(id));
            };
            let len = measure_at(name, &self.bytes[self.at..], depth + 1)?;
            self.skip(len)?;
        }
        for _ in 0..removed {
            self.var_int()?;
        }
        Ok(())
    }
}
