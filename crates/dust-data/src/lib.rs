//! Datapacks: reading `data/` from vanilla, from `datapacks/`, and from
//! whatever an operator put in them.
//!
//! # What a datapack is
//!
//! A tree of JSON under `data/<namespace>/<registry>/<path>.json` — plus
//! `.mcfunction` text under `function/`, read by [`function`] — and a
//! `pack.mcmeta` at the root saying which version of the format it was written
//! for. It ships as a directory or as a zip; both are read here and both must
//! produce the same result. The server's own vanilla data is the same shape and
//! is loaded as the bottom layer, so there is one reader rather than a special
//! case for "ours".
//!
//! ```text
//! my_pack/
//!   pack.mcmeta
//!   data/
//!     minecraft/
//!       recipe/stick.json          ← replaces vanilla's
//!       tags/block/logs.json       ← merges into vanilla's
//!     my_pack/
//!       loot_table/chests/mine.json
//! ```
//!
//! # The overlay model
//!
//! Packs are ordered, vanilla first and the operator's list after it. Later
//! wins. Every resource remembers which pack it came from and which packs it
//! displaced ([`Resource::pack`], [`Resource::overridden`]) — an operator
//! asking "why is this recipe wrong" needs that sentence, and keeping it costs
//! one string per resource.
//!
//! **Everything overrides. Tags merge.** That is the one exception and it is
//! not an inconsistency: a recipe is a definition and two definitions of one
//! name are a disagreement, while a tag is a membership list and two packs
//! adding to one list are not disagreeing about anything. If tags overrode,
//! installing two mods that each add a tool to `#minecraft:pickaxes` would
//! silently give you whichever loaded last. `"replace": true` is how a pack
//! says it really did mean to throw the earlier list away — it is also the only
//! way to *remove* a vanilla entry, since a tag has no subtract. See
//! [`tag`] for the whole argument and for what the resolver does not catch.
//!
//! One pack can carry its own layers too: an `overlays` section in
//! `pack.mcmeta` lists directories that stand in for the pack's own `data/`
//! when their format range matches. Those stack by the same rule — later wins
//! per file, with the base at the bottom — and [`overlay`] is where the exact
//! semantics live.
//!
//! # What this crate deliberately does not model
//!
//! Recipes, loot tables and advancements stay as [`serde_json::Value`] in the
//! loaded data. What this crate adds on top is a **skeleton** — see
//! [`shape`] — which pulls out only the identifying spine of those three
//! shapes: the serializer id each one opens with (`"type":
//! "minecraft:crafting_shaped"`), and the handful of reference targets a
//! report needs (a recipe's result, an advancement's parent). The full raw
//! document travels alongside the skeleton everywhere, so the skeleton can
//! never disagree with the file it came from: it is a summary pinned to the
//! thing it summarises, not a second reader. Generating structs for these
//! shapes as well would give Dust **two readers for one schema, and two
//! readers of one schema disagree**: one learns a new recipe type and the
//! other does not, and the result is a recipe that loads and then does
//! nothing.
//!
//! The line is between an **identifier** and a **schema**. Block state ids,
//! registry ids and packet ids became generated Rust in Phase 0.5 because the
//! wire format depends on them: a codec writing id 1,234 cannot go and read a
//! file to find out what it means. Recipes and loot tables are the datapack
//! schema, the shape an operator's own files are full of, and reading those
//! files is this crate's entire job — but *reading* them means holding them,
//! not deciding them. The crate that consumes a recipe decides what a recipe
//! is, and [`json::unknown_keys`] is public so it reports its unknown keys in
//! the same words this one does.
//!
//! Also not fully modelled: `function/` files are **read** — every command
//! line is kept, with its line number, under the file rules in [`function`] —
//! but the commands stay opaque strings, because deciding what
//! `execute as @e at @s run tp @s ~ ~-1 ~` means belongs to the layer that
//! runs commands, and half-parsing the grammar would be the two-readers
//! mistake again with much sharper edges. `structure/` (NBT, which belongs to
//! `dust-nbt`) remains [`registry::RegistryKind::Unread`] so its directory is
//! not mistaken for a typo; pack `filter` sections and feature flags parse
//! and produce a warning saying they are not applied. Nothing is skipped
//! silently.
//!
//! # The `dust-registry` seam
//!
//! Asking whether `minecraft:stobe` is a real block needs the block, item,
//! entity and fluid registries, which live in `dust-registry`. This crate does
//! not depend on it. It takes a [`Vocabulary`] instead — see that module for
//! the three reasons, the first of which is decisive: **a datapack adds
//! registry entries**, so the vocabulary a tag must be checked against is the
//! loaded world's, which does not exist until after this crate has run.
//!
//! The seam has a trap built into it and [`vocabulary::Known`] is the guard: a
//! vocabulary that knows nothing answers `Unknown`, never `Yes`, and
//! [`tag::TagStats::unvalidated_entries`] counts every entry nothing checked.
//! "No problems" must never be able to mean "no check ran".
//!
//! The other seam is NBT, which simply does not appear here: structures are
//! [`registry::RegistryKind::Unread`] until `dust-nbt` lands.
//!
//! # The surface, module by module
//!
//! Reading runs bottom-up, so the map goes the same way:
//!
//! * [`location`] — `namespace:path`, defaulted at the parse boundary and
//!   settled forever after;
//! * [`registry`] — which directories under `data/<namespace>/` are
//!   registries, and what kind of thing each holds;
//! * [`pack`] — the two containers, directories and zips; [`zip`] is the
//!   archive reader with its caps and refusals, [`inflate`] its
//!   decompressor;
//! * [`json`] and [`function`] — the two file readers, JSON with positions
//!   and lines under Minecraft's comment rules;
//! * [`meta`] — `pack.mcmeta`; [`overlay`] — a pack's own per-format layers,
//!   applied as a name mapping over any container;
//! * [`tag`] — the one resource that merges, and the resolver that flattens
//!   it; [`shape`] — the skeletons pinning a recipe, loot table or
//!   advancement to its raw document;
//! * [`advancement`] and [`loot`] — the two opt-in passes over a finished
//!   load: parent graphs, and misspelled serializer keys against the
//!   baseline definitions;
//! * [`vocabulary`] — the registry seam, with providers for what the packs
//!   themselves defined and a chain for whoever supplies the real
//!   registries later;
//! * [`reload`] — the atomic stack swap a running server owns, with the
//!   old-to-new diff and the policy that keeps a broken candidate out;
//! * [`finding`] — how all of the above say what went wrong, named by pack
//!   and file.
//!
//! Two conveniences tie it together for callers: [`discover::discover`]
//! reads a `datapacks/` folder into load order
//! ([`discover::load_directory`] does discovery plus loading in one call),
//! and [`LoadedData::diagnostic_dump`] renders the whole result — winners,
//! losers, provenance, findings — as stable, diffable text, the same
//! guarantee [`ReloadDiff::render`](reload::ReloadDiff::render) gives a
//! reload summary.
//!
//! # What the guards here do not catch
//!
//! * **Well-formed is not correct.** A recipe with no ingredients is valid JSON
//!   in the right directory with the right name, and nothing at this layer can
//!   tell. Half-validating it would be the two readers again.
//! * **`pack_format` is a claim.** A pack declaring 48 and containing 1.16 loot
//!   tables passes here and fails later.
//! * **The skeleton spine is best effort by design.** A recipe whose `result`
//!   names nothing parseable comes back with `None`, not a finding — deciding
//!   whether that is broken belongs to whoever reads recipes for real.
//! * **Unknown keys are only checked in the shapes this crate owns** —
//!   `pack.mcmeta` and tag files — plus the one opt-in pass: [`loot::audit`]
//!   reports misspelled keys on the loot conditions and functions its
//!   baseline tables cover. A misspelled key inside a recipe is still
//!   invisible from here, by the same argument as above.
//! * **The registry table is vanilla 1.21.1's.** A mod's registry directory
//!   will be reported as unknown until somebody calls
//!   [`registry::Registries::with_extra`].
//! * **A vocabulary is optional and its absence is the default.** Read the
//!   unvalidated count before believing a clean run.

pub mod advancement;
pub mod discover;
pub mod finding;
pub mod function;
pub mod inflate;
pub mod json;
pub mod location;
pub mod loot;
pub mod meta;
pub mod overlay;
pub mod pack;
pub mod registry;
pub mod reload;
pub mod shape;
pub mod tag;
pub mod vocabulary;
pub mod zip;

#[cfg(test)]
mod testing;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

pub use advancement::{validate as validate_advancements, AdvancementCycle, AdvancementReport};
pub use discover::{discover, load_directory};
pub use finding::{error_count, Finding, Severity};
pub use function::{FunctionFile, FunctionLine, LoadedFunction};
pub use location::{LocationError, ResourceLocation, MINECRAFT};
pub use loot::{
    audit as audit_loot, condition_def, function_def, SerializerDef, CONDITION_DEFS, FUNCTION_DEFS,
};
pub use meta::{PackMeta, DUST_PACK_FORMAT};
pub use overlay::{OverlainPack, OverlayPlan, Refusal};
pub use pack::{DirectoryPack, PackError, PackSource, ZipPack};
pub use registry::{Registries, RegistryDef, RegistryId, RegistryKind};
pub use reload::{
    Definition, RejectedReload, ReloadDiff, ReloadHandle, ReloadPolicy, Replacement, TagChange,
};
pub use shape::{AdvancementSkeleton, LootTableSkeleton, RecipeSkeleton, ShapeReport};
pub use tag::{MergedTag, ResolvedTags, TagStats};
pub use vocabulary::{Chained, Known, KnownNames, PackDefined, Unchecked, Vocabulary};

use registry::DirectoryMatch;
use tag::{TagFile, TagSet};

/// What to do with a pack whose declared format Dust does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownFormat {
    /// Do not load the pack; report it as an error. The default, and the
    /// argument for it is in [`meta`].
    #[default]
    Reject,
    /// Load it anyway, with a warning. For an operator who knows their
    /// format-15 pack is fine. The warning is not optional: a decision that
    /// stops being visible stops being a decision.
    LoadAnyway,
}

/// How a load is configured.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// The pack format Dust reads.
    pub pack_format: u32,
    pub unknown_format: UnknownFormat,
    pub registries: Registries,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            pack_format: DUST_PACK_FORMAT,
            unknown_format: UnknownFormat::default(),
            registries: Registries::vanilla(),
        }
    }
}

/// One resource, and where it came from.
#[derive(Debug, Clone)]
pub struct Resource {
    /// The parsed JSON, unmodelled. See the crate documentation.
    pub value: Value,
    /// The pack that won.
    pub pack: String,
    /// The file inside that pack.
    pub file: String,
    /// Packs that defined this and were overridden, earliest first.
    ///
    /// Kept because "which pack is this recipe from" is the first question
    /// anybody asks about a misbehaving modpack, and the answer is only useful
    /// with the list of who else tried.
    pub overridden: Vec<String>,
}

/// What one pack contributed.
#[derive(Debug, Clone)]
pub struct PackReport {
    pub id: String,
    pub origin: String,
    pub meta: Option<PackMeta>,
    /// `false` when the pack was refused — an unreadable `pack.mcmeta`, a
    /// format Dust does not read under [`UnknownFormat::Reject`], or an id
    /// another pack in the same load already holds.
    pub loaded: bool,
    /// Files this pack contributed a resource or a tag from.
    pub files_read: usize,
    /// Files in the pack that were not read, for any reason — including files
    /// inside overlays whose formats did not match.
    pub files_skipped: usize,
    /// What each namespace held, by name. A namespace with files seen but
    /// nothing read is where a mistyped registry directory shows up twice
    /// over; this is the per-namespace roll-up of it, for the diagnostics.
    pub namespaces: BTreeMap<String, NamespaceTally>,
}

/// Per-namespace file counts for one pack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NamespaceTally {
    /// Files under `data/<namespace>/`.
    pub files_seen: usize,
    /// Files that became a resource or part of a tag.
    pub files_read: usize,
}

/// The counts a load produces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadStats {
    pub packs_offered: usize,
    pub packs_loaded: usize,
    /// Every path listed in every loaded pack, including ones not read.
    pub files_seen: usize,
    /// Files that parsed into a resource or a tag.
    pub files_read: usize,
    /// Distinct resources after overriding.
    pub resources: usize,
    /// Distinct tags after merging.
    pub tags: usize,
    /// Distinct function files after overriding.
    pub functions: usize,
    /// Resources a later pack replaced.
    pub overrides: usize,
}

/// Everything the packs contained, overlaid.
#[derive(Debug, Default)]
pub struct LoadedData {
    packs: Vec<PackReport>,
    resources: BTreeMap<RegistryId, BTreeMap<ResourceLocation, Resource>>,
    tags: BTreeMap<RegistryId, TagSet>,
    functions: BTreeMap<RegistryId, BTreeMap<ResourceLocation, LoadedFunction>>,
    member_registry: BTreeMap<RegistryId, String>,
    findings: Vec<Finding>,
    stats: LoadStats,
}

impl LoadedData {
    /// Put discovery findings ahead of the load's own, without reordering
    /// either group. Used by [`discover::load_directory`].
    pub(crate) fn prepend_findings(&mut self, mut extra: Vec<Finding>) {
        extra.append(&mut self.findings);
        self.findings = extra;
    }

    pub fn packs(&self) -> &[PackReport] {
        &self.packs
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// How many findings stopped something loading.
    pub fn error_count(&self) -> usize {
        error_count(&self.findings)
    }

    pub fn stats(&self) -> &LoadStats {
        &self.stats
    }

    /// One resource, if any pack defined it.
    pub fn get(&self, registry: &RegistryId, name: &ResourceLocation) -> Option<&Resource> {
        self.resources.get(registry)?.get(name)
    }

    /// Every resource of one registry, in name order.
    pub fn registry(&self, registry: &RegistryId) -> Option<&BTreeMap<ResourceLocation, Resource>> {
        self.resources.get(registry)
    }

    pub fn registries(&self) -> impl Iterator<Item = &RegistryId> {
        self.resources.keys()
    }

    /// Every tag registry that any pack contributed to.
    pub fn tag_registries(&self) -> impl Iterator<Item = &RegistryId> {
        self.tags.keys()
    }

    /// Every loaded function file of one registry — `function`, today — by
    /// name, winners only. Commands are opaque strings; see
    /// [`crate::function`] for where that line is drawn.
    pub fn functions(
        &self,
        registry: &RegistryId,
    ) -> Option<&BTreeMap<ResourceLocation, LoadedFunction>> {
        self.functions.get(registry)
    }

    /// Every registry holding function files.
    pub fn function_registries(&self) -> impl Iterator<Item = &RegistryId> {
        self.functions.keys()
    }

    /// One tag as merged, before references are followed.
    pub fn merged_tag(&self, registry: &RegistryId, name: &ResourceLocation) -> Option<&MergedTag> {
        self.tags.get(registry)?.get(name)
    }

    /// Every tag of one registry, merged, before references are followed.
    pub fn merged_tags(&self, registry: &RegistryId) -> Option<&TagSet> {
        self.tags.get(registry)
    }

    /// Follow every `#` reference and flatten every tag.
    ///
    /// Separate from the load because the [`Vocabulary`] comes from somewhere
    /// this crate cannot see, and because the packs themselves contribute to
    /// it: a caller builds the vocabulary out of `self` and *then* calls this.
    pub fn resolve_tags(&self, vocabulary: &dyn Vocabulary) -> (ResolvedTags, Vec<Finding>) {
        tag::resolve(&self.tags, &self.member_registry, vocabulary)
    }

    /// The namespaces any loaded pack contributed data for.
    pub fn namespaces(&self) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        for registry in self.resources.values() {
            out.extend(registry.keys().map(ResourceLocation::namespace));
        }
        for registry in self.tags.values() {
            out.extend(registry.keys().map(ResourceLocation::namespace));
        }
        for registry in self.functions.values() {
            out.extend(registry.keys().map(ResourceLocation::namespace));
        }
        out
    }

    /// Every resource and tag whose name sits in `namespace`, with provenance.
    ///
    /// This is the merged view an integrator actually consumes: one name, one
    /// winning [`Resource`], whoever lost still listed under
    /// [`Resource::overridden`]. Tags come back in their merged-but-unresolved
    /// form; [`Self::resolve_tags`] is the flattened view.
    pub fn namespace(&self, namespace: &str) -> NamespaceView<'_> {
        let resources = self
            .resources
            .iter()
            .map(|(registry, entries)| {
                (
                    registry,
                    entries
                        .iter()
                        .filter(|(name, _)| name.namespace() == namespace)
                        .collect::<BTreeMap<_, _>>(),
                )
            })
            .filter(|(_, entries)| !entries.is_empty())
            .collect();
        let tags = self
            .tags
            .iter()
            .map(|(registry, entries)| {
                (
                    registry,
                    entries
                        .iter()
                        .filter(|(name, _)| name.namespace() == namespace)
                        .collect::<BTreeMap<_, _>>(),
                )
            })
            .filter(|(_, entries)| !entries.is_empty())
            .collect();
        let functions = self
            .functions
            .iter()
            .map(|(registry, entries)| {
                (
                    registry,
                    entries
                        .iter()
                        .filter(|(name, _)| name.namespace() == namespace)
                        .collect::<BTreeMap<_, _>>(),
                )
            })
            .filter(|(_, entries)| !entries.is_empty())
            .collect();
        NamespaceView {
            resources,
            tags,
            functions,
        }
    }

    /// The whole load as a human-readable provenance report: which pack won
    /// every resource, where each tag's lines came from, what was refused and
    /// why.
    ///
    /// Written for the person staring at a modpack asking "why is this recipe
    /// wrong", and built to be the foundation of the Phase 10 feasibility
    /// tooling, which needs exactly this answer for hundreds of packs at once.
    /// That is why it is a *stable* rendering — every line is derived from
    /// ordered maps, so two loads of the same packs produce byte-identical
    /// text, and a diff between two dumps is meaningful without anyone writing
    /// a parser first. A structured API already exists alongside it
    /// ([`Self::registry`], [`Self::namespace`], [`Resource::overridden`],
    /// [`PackReport`]); this is the same facts said once, readably.
    pub fn diagnostic_dump(&self) -> String {
        let mut out = String::new();
        let stats = &self.stats;
        out.push_str(&format!(
            "datapack load: {} pack(s) offered, {} loaded\n",
            stats.packs_offered, stats.packs_loaded
        ));

        out.push_str("packs:\n");
        for pack in &self.packs {
            match (&pack.meta, pack.loaded) {
                (Some(meta), true) => {
                    out.push_str(&format!(
                        "  {} [loaded] {} — format {} (accepting {}), read {} of \
                         {} file(s)\n",
                        pack.id,
                        pack.origin,
                        meta.pack_format,
                        meta.supported,
                        pack.files_read,
                        pack.files_read + pack.files_skipped,
                    ));
                }
                (Some(meta), false) => {
                    out.push_str(&format!(
                        "  {} [refused] {} — format {} (accepting {})\n",
                        pack.id, pack.origin, meta.pack_format, meta.supported,
                    ));
                }
                (None, _) => {
                    out.push_str(&format!("  {} [refused] {}\n", pack.id, pack.origin));
                }
            }
            if pack.namespaces.is_empty() {
                continue;
            }
            let names: Vec<String> = pack
                .namespaces
                .iter()
                .map(|(name, tally)| format!("{name} ({}/{})", tally.files_read, tally.files_seen))
                .collect();
            out.push_str(&format!("    namespaces: {}\n", names.join(", ")));
        }

        out.push_str("resources:\n");
        if self.resources.is_empty() {
            out.push_str("  <none>\n");
        }
        for (registry, entries) in &self.resources {
            out.push_str(&format!("  {registry}: {} resource(s)\n", entries.len()));
            for (name, resource) in entries {
                out.push_str(&format!("    {name} <- {}", resource.pack));
                if !resource.overridden.is_empty() {
                    out.push_str(&format!(" (displaced: {})", resource.overridden.join(", ")));
                }
                out.push_str(&format!("\n      {}\n", resource.file));
            }
        }

        out.push_str("tags:\n");
        if self.tags.is_empty() {
            out.push_str("  <none>\n");
        }
        for (registry, tags) in &self.tags {
            out.push_str(&format!("  {registry}: {} tag(s)\n", tags.len()));
            for (name, tag) in tags {
                let sources: Vec<String> = tag
                    .sources
                    .iter()
                    .map(|source| format!("{} ({})", source.pack, source.file))
                    .collect();
                out.push_str(&format!(
                    "    #{name}: {} written entr{} from {}\n",
                    tag.entries.len(),
                    if tag.entries.len() == 1 { "y" } else { "ies" },
                    sources.join(", "),
                ));
            }
        }

        out.push_str("functions:\n");
        if self.functions.is_empty() {
            out.push_str("  <none>\n");
        }
        for (registry, functions) in &self.functions {
            out.push_str(&format!("  {registry}: {} function(s)\n", functions.len()));
            for (name, function) in functions {
                out.push_str(&format!(
                    "    {name} <- {} ({} command(s))",
                    function.pack,
                    function.file.command_count()
                ));
                if !function.overridden.is_empty() {
                    out.push_str(&format!(" (displaced: {})", function.overridden.join(", ")));
                }
                out.push_str(&format!("\n      {}\n", function.path));
            }
        }

        out.push_str(&format!(
            "totals: {} file(s) seen, {} read; {} resource(s); {} tag(s); {} \
             override(s); {} function(s)\n",
            stats.files_seen,
            stats.files_read,
            stats.resources,
            stats.tags,
            stats.overrides,
            stats.functions,
        ));

        out.push_str(&format!("findings: {}\n", self.findings.len()));
        for finding in &self.findings {
            out.push_str(&format!("  {finding}\n"));
        }
        out
    }
}

/// One namespace's slice through the merged data, filtered by name.
///
/// Per-registry maps of only the entries whose namespace matched. Built by
/// [`LoadedData::namespace`].
#[derive(Debug, Default)]
pub struct NamespaceView<'a> {
    pub resources: BTreeMap<&'a RegistryId, BTreeMap<&'a ResourceLocation, &'a Resource>>,
    pub tags: BTreeMap<&'a RegistryId, BTreeMap<&'a ResourceLocation, &'a MergedTag>>,
    pub functions: BTreeMap<&'a RegistryId, BTreeMap<&'a ResourceLocation, &'a LoadedFunction>>,
}

/// Read every pack in order, later overriding earlier.
///
/// Errors are collected, never returned early. A pack with forty problems
/// produces forty findings from one run; making an operator fix one, restart,
/// and be told about the next teaches them to distrust the server rather than
/// the file.
///
/// Pack ids must be unique within one load. The id is on every finding and
/// every provenance line, so two packs answering to one name would make the
/// whole report ambiguous; a duplicate is refused (the later of the two) with
/// an error saying so. [`discover::discover`] refuses at discovery time as well, which
/// is where an operator actually meets the problem.
pub fn load(packs: &[&dyn PackSource], options: &LoadOptions) -> LoadedData {
    let mut data = LoadedData {
        stats: LoadStats {
            packs_offered: packs.len(),
            ..LoadStats::default()
        },
        ..LoadedData::default()
    };

    let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
    for source in packs {
        // Refuse without listing: nothing in a duplicate can be trusted to a
        // provenance line, because every line it produced would be
        // indistinguishable from the other pack's.
        if !seen_ids.insert(source.id()) {
            data.findings.push(Finding::error(
                source.id(),
                "",
                format!(
                    "is named `{}`, which a pack earlier in this load already \
                     has. Two packs cannot answer to one id — every finding and \
                     every `this came from` line would name both — so this one \
                     has not been loaded. Rename one of them.",
                    source.id()
                ),
            ));
            data.packs.push(PackReport {
                id: source.id().to_owned(),
                origin: source.origin(),
                meta: None,
                loaded: false,
                files_read: 0,
                files_skipped: 0,
                namespaces: BTreeMap::new(),
            });
            continue;
        }
        let report = load_pack(*source, options, &mut data);
        if report.loaded {
            data.stats.packs_loaded += 1;
        }
        data.packs.push(report);
    }

    data.stats.resources = data.resources.values().map(BTreeMap::len).sum();
    data.stats.tags = data.tags.values().map(BTreeMap::len).sum();
    data.stats.functions = data.functions.values().map(BTreeMap::len).sum();
    data
}

/// Counters that only make sense within one pack, so that a directory Dust
/// does not read produces one line rather than one line per file.
#[derive(Default)]
struct PackTally {
    unknown_directories: BTreeMap<String, usize>,
    unread_registries: BTreeMap<String, usize>,
    legacy_directories: BTreeMap<String, RegistryId>,
    non_json: BTreeMap<String, usize>,
    /// Files in a [`RegistryKind::Commands`] registry whose extension is not
    /// the one that registry holds. Counted per registry, with the expected
    /// extension kept beside the count so the warning can name it.
    wrong_extension: BTreeMap<String, (usize, &'static str)>,
    /// Paths inside the pack that are themselves zip archives. A pack inside
    /// a pack is never read — not by Minecraft, not by here — and it is
    /// almost always an accident of zipping one directory too high, so it is
    /// named rather than treated as inert junk.
    nested_archives: Vec<String>,
    /// Files seen under each namespace. Read counts are kept separately so a
    /// namespace that held files but contributed nothing is visible as such.
    namespace_files: BTreeMap<String, usize>,
    namespace_reads: BTreeMap<String, usize>,
}

fn load_pack(source: &dyn PackSource, options: &LoadOptions, data: &mut LoadedData) -> PackReport {
    let id = source.id().to_owned();
    let origin = source.origin();
    let mut report = PackReport {
        id: id.clone(),
        origin,
        meta: None,
        loaded: false,
        files_read: 0,
        files_skipped: 0,
        namespaces: BTreeMap::new(),
    };

    let listing = match source.list() {
        Ok(listing) => listing,
        Err(error) => {
            data.findings.push(Finding::error(
                &id,
                "",
                format!("could not be read: {error}"),
            ));
            return report;
        }
    };
    // Every physical path counts against the pack even when an overlay makes
    // it invisible: the diagnostics answer "what does this pack hold", not
    // "what did this format use".
    data.stats.files_seen += listing.len();

    let Some(meta) = read_meta(source, &id, &listing, data) else {
        report.files_skipped = listing.len();
        return report;
    };

    if !meta.is_compatible_with(options.pack_format) {
        let complaint = format!(
            "declares pack format {} (accepting {}), and Dust reads format {}.",
            meta.pack_format, meta.supported, options.pack_format
        );
        match options.unknown_format {
            UnknownFormat::Reject => {
                data.findings.push(Finding::error(
                    &id,
                    "pack.mcmeta",
                    format!(
                        "{complaint} Nothing in this pack has been loaded. Update the \
                         pack, or set `unknown_format` to load it anyway and accept \
                         that its files may be written against a different schema."
                    ),
                ));
                report.meta = Some(meta);
                report.files_skipped = listing.len();
                return report;
            }
            UnknownFormat::LoadAnyway => data.findings.push(Finding::warning(
                &id,
                "pack.mcmeta",
                format!(
                    "{complaint} It is being loaded anyway because Dust was told to. \
                     Directory names and file shapes changed between formats, so \
                     anything missing from this pack is probably why."
                ),
            )),
        }
    }

    report.loaded = true;

    // Overlays are planned once, from the listing already in hand, and the
    // rest of the load runs over the layered view — every rule below this
    // point stays written against "a pack", not "a pack plus its overlays".
    let plan = OverlayPlan::build(&listing, &meta.overlays, options.pack_format);
    for (directory, refusal) in &plan.refused {
        data.findings.push(Finding::error(
            &id,
            "pack.mcmeta",
            format!(
                "declares an overlay entry whose directory `{directory}` {}. \
                 Nothing has been read from it.",
                refusal.reason()
            ),
        ));
    }

    let mut tally = PackTally::default();

    // Nested archives are spotted on the raw listing rather than during the
    // per-file walk: a `.zip` beside `pack.mcmeta` never reaches a registry
    // match, so the walk would stay silent about exactly the case that most
    // needs saying. The raw listing rather than the layered view, because an
    // archive under an inert overlay is still being carried.
    for path in &listing {
        if path.ends_with(".zip") || path.ends_with(".ZIP") {
            tally.nested_archives.push(path.clone());
        }
    }

    if plan.applied.is_empty() {
        for path in &listing {
            if read_file(source, &id, path, options, data, &mut tally) {
                report.files_read += 1;
            }
        }
    } else {
        let layered = OverlainPack::new(source, plan);
        for path in layered.list().expect("an overlay view lists without io") {
            if read_file(&layered, &id, &path, options, data, &mut tally) {
                report.files_read += 1;
            }
        }
    }

    // Whatever was listed and did not become a resource or a tag — inert
    // overlay files included — is accounted for here rather than one increment
    // at a time, so the two numbers can never drift apart.
    report.files_skipped = listing.len() - report.files_read;
    for (namespace, files) in &tally.namespace_files {
        report.namespaces.insert(
            namespace.clone(),
            NamespaceTally {
                files_seen: *files,
                files_read: tally.namespace_reads.get(namespace).copied().unwrap_or(0),
            },
        );
    }

    report_tally(&id, &tally, options, data);
    report.meta = Some(meta);
    report
}

/// Read and check `pack.mcmeta`, or explain its absence.
fn read_meta(
    source: &dyn PackSource,
    id: &str,
    listing: &[String],
    data: &mut LoadedData,
) -> Option<PackMeta> {
    let bytes = match source.read("pack.mcmeta") {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => None,
        Err(error) => {
            data.findings.push(Finding::error(
                id,
                "pack.mcmeta",
                format!("could not be read: {error}"),
            ));
            return None;
        }
    };

    match bytes {
        Some(bytes) => {
            let (meta, findings) = PackMeta::parse(&bytes, id, "pack.mcmeta");
            data.findings.extend(findings);
            meta
        }
        None => match source.assumed_format() {
            // The built-in layer. Its format comes from the build, not a file.
            Some(format) => Some(PackMeta::assumed(format)),
            None => {
                data.findings.push(Finding::error(
                    id,
                    "",
                    format!(
                        "has no pack.mcmeta, so Dust cannot tell what it is and has \
                         not loaded it.{}",
                        nested_pack_hint(listing)
                    ),
                ));
                None
            }
        },
    }
}

/// The single most common way a datapack is broken: zipped one directory too
/// high, so the archive holds a folder that holds the pack.
///
/// Worth its own sentence because the generic message — "no pack.mcmeta" —
/// sends people looking for a file that is right there.
fn nested_pack_hint(listing: &[String]) -> String {
    let mut candidates: Vec<&str> = listing
        .iter()
        .filter_map(|path| path.strip_suffix("/pack.mcmeta"))
        .filter(|prefix| !prefix.contains('/'))
        .collect();
    candidates.sort_unstable();
    match candidates.first() {
        Some(directory) => format!(
            " There is a pack.mcmeta one level down, in `{directory}/`, so this \
             was probably zipped from one directory too high — the archive \
             should start at pack.mcmeta, not at the folder containing it."
        ),
        None => String::new(),
    }
}

/// Returns whether the file became a resource or a tag.
fn read_file(
    source: &dyn PackSource,
    id: &str,
    path: &str,
    options: &LoadOptions,
    data: &mut LoadedData,
    tally: &mut PackTally,
) -> bool {
    let Some(rest) = path.strip_prefix("data/") else {
        // `pack.mcmeta`, `pack.png`, a README, or the `assets/` half of a pack
        // that is also a resource pack. None of those are data and none of them
        // are mistakes, so none of them are worth a line each. A *directory*
        // that is not `data/` is caught by the nested-pack hint above when it
        // matters.
        return false;
    };

    let Some((namespace, relative)) = rest.split_once('/') else {
        data.findings.push(Finding::warning(
            id,
            path,
            "is directly inside `data/`, which holds one directory per \
             namespace and no loose files.",
        ));
        return false;
    };
    // Counted before anything decides whether the file is readable, so the
    // per-namespace roll-up says "held N files" rather than "held the files
    // that happened to load".
    *tally
        .namespace_files
        .entry(namespace.to_owned())
        .or_default() += 1;

    let Some(matched) = options.registries.classify(relative) else {
        let directory = Registries::unmatched_directory(relative);
        *tally
            .unknown_directories
            .entry(directory.to_owned())
            .or_default() += 1;
        return false;
    };

    if let RegistryKind::Unread { .. } = matched.def.kind {
        *tally
            .unread_registries
            .entry(matched.def.key.to_string())
            .or_default() += 1;
        return false;
    }

    if matched.is_legacy() {
        tally
            .legacy_directories
            .insert(matched.written_as.to_string(), matched.def.key.clone());
    }

    // A commands registry holds one file extension and no JSON. Everything
    // below this point is the JSON pipeline, so it branches off here.
    if let RegistryKind::Commands { extension } = matched.def.kind {
        return read_function(
            source, id, path, &matched, namespace, extension, tally, data,
        );
    }

    let Some(stem) = matched.remainder.strip_suffix(".json") else {
        *tally
            .non_json
            .entry(matched.def.key.to_string())
            .or_default() += 1;
        return false;
    };

    let name = match ResourceLocation::new(namespace, stem) {
        Ok(name) => name,
        Err(error) => {
            data.findings.push(Finding::error(
                id,
                path,
                format!("is not in a usable place: {error}"),
            ));
            return false;
        }
    };

    let bytes = match source.read(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            // Listed a moment ago and gone now: a pack being edited under a
            // running server, which is exactly when /reload is used.
            data.findings.push(Finding::error(
                id,
                path,
                "was listed by the pack and then could not be read. If the pack \
                 is being edited, reload again once it has settled.",
            ));
            return false;
        }
        Err(error) => {
            data.findings.push(Finding::error(
                id,
                path,
                format!("could not be read: {error}"),
            ));
            return false;
        }
    };

    let value = match json::parse(&bytes, id, path) {
        Ok(value) => value,
        Err(finding) => {
            data.findings.push(finding.about(name));
            return false;
        }
    };

    // Three registries open with a serializer id — `"type": "minecraft:…"` —
    // and without it no consumer can even pick a schema to read the rest by.
    // That is not shape-checking ahead of the reader (the crate documentation's
    // refusal stands); it is checking that the one key every variant of the
    // shape shares is present, which is as far as this layer can see without
    // becoming a second opinion about what a recipe is.
    if matches!(matched.def.kind, RegistryKind::Content)
        && matches!(
            matched.def.key.as_str(),
            "recipe" | "loot_table" | "advancement"
        )
    {
        let spine = value.get("type").and_then(Value::as_str);
        if spine.is_none() {
            data.findings.push(
                Finding::warning(
                    id,
                    path,
                    format!(
                        "has no string `type` naming what kind of {} it is. Every \
                         one starts with `\"type\": \"minecraft:…\"`; without it \
                         nothing downstream can tell which reader to use, so this \
                         file will have no effect.",
                        matched.def.key
                    ),
                )
                .about(name.clone()),
            );
        }
    }

    match matched.def.kind {
        RegistryKind::Tag(of) => {
            let (file, findings) = TagFile::parse(&value, id, path);
            data.findings
                .extend(findings.into_iter().map(|f| f.about(name.clone())));
            data.member_registry
                .entry(matched.def.key.clone())
                .or_insert_with(|| of.to_owned());
            data.tags
                .entry(matched.def.key.clone())
                .or_default()
                .entry(name)
                .or_default()
                .apply(&file, id, path);
        }
        _ => insert_resource(&matched, name, value, id, path, data),
    }
    *tally
        .namespace_reads
        .entry(namespace.to_owned())
        .or_default() += 1;
    data.stats.files_read += 1;
    true
}

/// Returns whether the file became a loaded function file.
///
/// The function-side twin of the JSON pipeline below it: same listing, same
/// provenance, same override rule, but a text reader instead of a JSON one
/// and no shape work at all.
#[allow(clippy::too_many_arguments)]
fn read_function(
    source: &dyn PackSource,
    id: &str,
    path: &str,
    matched: &DirectoryMatch<'_>,
    namespace: &str,
    extension: &'static str,
    tally: &mut PackTally,
    data: &mut LoadedData,
) -> bool {
    let Some(stem) = matched.remainder.strip_suffix(extension) else {
        let slot = tally
            .wrong_extension
            .entry(matched.def.key.to_string())
            .or_insert((0, extension));
        slot.0 += 1;
        return false;
    };

    let name = match ResourceLocation::new(namespace, stem) {
        Ok(name) => name,
        Err(error) => {
            data.findings.push(Finding::error(
                id,
                path,
                format!("is not in a usable place: {error}"),
            ));
            return false;
        }
    };

    let bytes = match source.read(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            // Listed a moment ago and gone now — the pack-being-edited case
            // the JSON path describes. Same situation, same message, because
            // an operator should not have to know which kind of file lost
            // the race.
            data.findings.push(Finding::error(
                id,
                path,
                "was listed by the pack and then could not be read. If the pack \
                 is being edited, reload again once it has settled.",
            ));
            return false;
        }
        Err(error) => {
            data.findings.push(Finding::error(
                id,
                path,
                format!("could not be read: {error}"),
            ));
            return false;
        }
    };

    let (file, findings) = function::FunctionFile::parse(&bytes, id, path);
    let previous_errors = error_count(&data.findings);
    data.findings
        .extend(findings.into_iter().map(|f| f.about(name.clone())));

    // An error means the resource is not loaded — the same contract the JSON
    // pipeline keeps. An undecodable file contributes nothing rather than an
    // empty function wearing its name.
    if error_count(&data.findings) > previous_errors {
        return false;
    }

    insert_function(matched, name, file, id, path, data);
    *tally
        .namespace_reads
        .entry(namespace.to_owned())
        .or_default() += 1;
    data.stats.files_read += 1;
    true
}

fn insert_function(
    matched: &DirectoryMatch<'_>,
    name: ResourceLocation,
    file: function::FunctionFile,
    id: &str,
    path: &str,
    data: &mut LoadedData,
) {
    let slot = data
        .functions
        .entry(matched.def.key.clone())
        .or_default()
        .entry(name.clone());
    match slot {
        std::collections::btree_map::Entry::Vacant(vacant) => {
            vacant.insert(LoadedFunction {
                file,
                pack: id.to_owned(),
                path: path.to_owned(),
                overridden: Vec::new(),
            });
        }
        std::collections::btree_map::Entry::Occupied(mut occupied) => {
            let previous = occupied.get_mut();
            if previous.pack == id {
                // One pack reached one name by two paths: the pre-1.21
                // spelling beside the current one. Minecraft would never see
                // both; Dust merges the spellings into one namespace, so the
                // collision has to be resolved here, and silently picking a
                // winner would leave half the pack mysteriously inert. The
                // current spelling wins, whatever order the listing had.
                let incoming_is_canonical = !matched.is_legacy();
                let winner = if incoming_is_canonical {
                    path
                } else {
                    &previous.path
                };
                data.findings.push(Finding::warning(
                    id,
                    path,
                    format!(
                        "defines the function `{name}` twice: once at `{}` and \
                         again at `{path}`. `{winner}` is the copy under the \
                         current directory spelling, so that one is used.",
                        previous.path
                    ),
                ));
                if incoming_is_canonical {
                    *previous = LoadedFunction {
                        file,
                        pack: id.to_owned(),
                        path: path.to_owned(),
                        overridden: std::mem::take(&mut previous.overridden),
                    };
                }
                return;
            }
            // A different pack: the ordinary override, recorded the same way
            // resources record it.
            let mut overridden = std::mem::take(&mut previous.overridden);
            overridden.push(previous.pack.clone());
            data.stats.overrides += 1;
            *previous = LoadedFunction {
                file,
                pack: id.to_owned(),
                path: path.to_owned(),
                overridden,
            };
        }
    }
}

fn insert_resource(
    matched: &DirectoryMatch<'_>,
    name: ResourceLocation,
    value: Value,
    id: &str,
    path: &str,
    data: &mut LoadedData,
) {
    let slot = data
        .resources
        .entry(matched.def.key.clone())
        .or_default()
        .entry(name);
    match slot {
        std::collections::btree_map::Entry::Vacant(vacant) => {
            vacant.insert(Resource {
                value,
                pack: id.to_owned(),
                file: path.to_owned(),
                overridden: Vec::new(),
            });
        }
        std::collections::btree_map::Entry::Occupied(mut occupied) => {
            let previous = occupied.get_mut();
            let mut overridden = std::mem::take(&mut previous.overridden);
            overridden.push(previous.pack.clone());
            data.stats.overrides += 1;
            *previous = Resource {
                value,
                pack: id.to_owned(),
                file: path.to_owned(),
                overridden,
            };
        }
    }
}

/// Turn the per-pack counters into one finding each.
fn report_tally(id: &str, tally: &PackTally, options: &LoadOptions, data: &mut LoadedData) {
    for (directory, count) in &tally.unknown_directories {
        data.findings.push(Finding::warning(
            id,
            format!("data/*/{directory}/"),
            format!(
                "is not a registry Dust loads, so the {count} file(s) under it \
                 have no effect.{}",
                finding::suggestion(directory, options.registries.directory_names()),
            ),
        ));
    }
    for (registry, count) in &tally.unread_registries {
        let why = options
            .registries
            .get(&RegistryId::new(registry.as_str()))
            .and_then(|def| match &def.kind {
                RegistryKind::Unread { extension, why } => Some((*extension, *why)),
                _ => None,
            });
        let (extension, why) = why.unwrap_or(("", "is not read"));
        data.findings.push(Finding::warning(
            id,
            format!("data/*/{registry}/"),
            format!("holds {count} {extension} file(s), which Dust does not read: {why}."),
        ));
    }
    for (written, canonical) in &tally.legacy_directories {
        data.findings.push(Finding::warning(
            id,
            format!("data/*/{written}/"),
            format!(
                "uses the pre-1.21 directory name `{written}`. Dust has read it as \
                 `{canonical}`, but Minecraft itself would not, so rename it before \
                 this pack is used anywhere else."
            ),
        ));
    }
    for (registry, count) in &tally.non_json {
        data.findings.push(Finding::warning(
            id,
            format!("data/*/{registry}/"),
            format!(
                "holds {count} file(s) that do not end in `.json`. Resources are \
                 named after their file, so those files are not loaded."
            ),
        ));
    }
    for (registry, (count, extension)) in &tally.wrong_extension {
        data.findings.push(Finding::warning(
            id,
            format!("data/*/{registry}/"),
            format!(
                "holds {count} file(s) that do not end in `{extension}`. Functions \
                 are named after their file, so those files are not loaded."
            ),
        ));
    }
    if !tally.nested_archives.is_empty() {
        let (named, more) = match tally.nested_archives.len() {
            0..=5 => (tally.nested_archives.join(", "), 0),
            n => (
                format!("{} …", tally.nested_archives[..5].join(", ")),
                n - 5,
            ),
        };
        let and_more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        data.findings.push(Finding::warning(
            id,
            &tally.nested_archives[0],
            format!(
                "is a zip archive carried inside this pack, as {}{and_more}. \
                 Nothing inside a zip inside a pack is read — Minecraft would \
                 not read it either — so if that archive was meant to be part \
                 of the pack, its files have to be unpacked into it.",
                named
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemPack;

    fn location(text: &str) -> ResourceLocation {
        ResourceLocation::parse(text).expect("valid")
    }

    /// A recipe with its spine filled in, for the tests that are about
    /// something other than the missing-`type` warning.
    fn typed_recipe() -> &'static str {
        r#"{"type":"minecraft:crafting_shaped"}"#
    }

    #[test]
    fn a_later_pack_overrides_an_earlier_one_and_the_earlier_is_remembered() {
        let base = MemPack::with_meta(
            "base",
            &[("data/minecraft/recipe/stick.json", r#"{"result":"stick"}"#)],
        );
        let over = MemPack::with_meta(
            "over",
            &[("data/minecraft/recipe/stick.json", r#"{"result":"twig"}"#)],
        );
        let data = load(&[&base, &over], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());

        let resource = data
            .get(&RegistryId::new("recipe"), &location("minecraft:stick"))
            .expect("loaded");
        assert_eq!(resource.pack, "over");
        assert_eq!(resource.overridden, vec!["base".to_owned()]);
        assert_eq!(data.stats().overrides, 1);
        assert_eq!(data.stats().resources, 1);
    }

    #[test]
    fn tags_merge_where_recipes_override() {
        let base = MemPack::with_meta(
            "base",
            &[(
                "data/minecraft/tags/block/logs.json",
                r#"{"values":["minecraft:oak_log"]}"#,
            )],
        );
        let over = MemPack::with_meta(
            "over",
            &[(
                "data/minecraft/tags/block/logs.json",
                r#"{"values":["copper:copper_log"]}"#,
            )],
        );
        let data = load(&[&base, &over], &LoadOptions::default());
        let (resolved, findings) = data.resolve_tags(&vocabulary::Unchecked);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(
            resolved
                .get(&RegistryId::new("tags/block"), &location("minecraft:logs"))
                .expect("resolved")
                .len(),
            2
        );
    }

    #[test]
    fn a_pack_with_no_mcmeta_is_refused_and_the_reason_is_specific() {
        let pack = MemPack::new("broken", &[("data/minecraft/recipe/stick.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.stats().packs_loaded, 0);
        assert_eq!(data.error_count(), 1, "{:?}", data.findings());
        assert!(data.findings()[0].message.contains("pack.mcmeta"));
    }

    #[test]
    fn a_pack_zipped_one_level_too_high_is_told_so() {
        let pack = MemPack::new(
            "wrapped",
            &[
                ("my_pack/pack.mcmeta", r#"{"pack":{"pack_format":48}}"#),
                ("my_pack/data/minecraft/recipe/stick.json", "{}"),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert!(
            data.findings()[0]
                .message
                .contains("one directory too high"),
            "{:?}",
            data.findings()
        );
    }

    #[test]
    fn an_incompatible_format_refuses_the_pack_without_stopping_the_load() {
        let old = MemPack::new(
            "old",
            &[
                (
                    "pack.mcmeta",
                    r#"{"pack":{"pack_format":15,"description":"d"}}"#,
                ),
                ("data/minecraft/recipe/stick.json", "{}"),
            ],
        );
        let current = MemPack::with_meta("current", &[("data/minecraft/recipe/rod.json", "{}")]);
        let data = load(&[&old, &current], &LoadOptions::default());

        assert_eq!(data.stats().packs_loaded, 1);
        assert_eq!(data.error_count(), 1, "{:?}", data.findings());
        assert!(data
            .get(&RegistryId::new("recipe"), &location("minecraft:stick"))
            .is_none());
        // The point of not making it fatal: the next pack still loaded.
        assert!(data
            .get(&RegistryId::new("recipe"), &location("minecraft:rod"))
            .is_some());
    }

    #[test]
    fn the_escape_hatch_loads_it_and_keeps_saying_so() {
        let old = MemPack::new(
            "old",
            &[
                (
                    "pack.mcmeta",
                    r#"{"pack":{"pack_format":15,"description":"d"}}"#,
                ),
                ("data/minecraft/recipe/stick.json", typed_recipe()),
            ],
        );
        let options = LoadOptions {
            unknown_format: UnknownFormat::LoadAnyway,
            ..LoadOptions::default()
        };
        let data = load(&[&old], &options);
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert_eq!(data.findings().len(), 1);
        assert_eq!(data.findings()[0].severity, Severity::Warning);
        assert!(data
            .get(&RegistryId::new("recipe"), &location("minecraft:stick"))
            .is_some());
    }

    #[test]
    fn one_run_reports_every_broken_file_rather_than_the_first() {
        let pack = MemPack::with_meta(
            "messy",
            &[
                ("data/minecraft/recipe/a.json", "{"),
                ("data/minecraft/recipe/b.json", "not json"),
                ("data/minecraft/recipe/c.json", "[1,2,"),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 3, "{:?}", data.findings());
    }

    #[test]
    fn a_directory_that_is_not_a_registry_is_one_finding_and_not_one_per_file() {
        let pack = MemPack::with_meta(
            "typo",
            &[
                ("data/minecraft/recipies/a.json", "{}"),
                ("data/minecraft/recipies/b.json", "{}"),
                ("data/minecraft/recipies/c.json", "{}"),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.findings().len(), 1, "{:?}", data.findings());
        assert!(data.findings()[0].message.contains("3 file(s)"));
        assert!(
            data.findings()[0]
                .message
                .contains("Did you mean `recipe`?"),
            "{}",
            data.findings()[0]
        );
    }

    #[test]
    fn a_legacy_directory_loads_and_says_it_should_be_renamed() {
        let pack = MemPack::with_meta("old_layout", &[("data/minecraft/recipes/stick.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert!(data
            .get(&RegistryId::new("recipe"), &location("minecraft:stick"))
            .is_some());
        assert!(
            data.findings()
                .iter()
                .any(|f| f.message.contains("pre-1.21")),
            "{:?}",
            data.findings()
        );
    }

    #[test]
    fn a_directory_dust_knows_and_does_not_read_says_which_it_is() {
        let pack = MemPack::with_meta(
            "with_structures",
            &[("data/minecraft/structure/hut.nbt", "not really nbt")],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert!(
            data.findings()[0].message.contains(".nbt"),
            "{:?}",
            data.findings()
        );
    }

    #[test]
    fn a_function_file_loads_with_its_commands_opaque_and_counted() {
        let pack = MemPack::with_meta(
            "functions",
            &[(
                "data/minecraft/function/tick.mcfunction",
                "# setup\nsay hello\n\ntellraw @a \"bye\"\n",
            )],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert_eq!(data.stats().functions, 1);
        let functions = data
            .functions(&RegistryId::new("function"))
            .expect("function registry");
        let tick = functions.get(&location("minecraft:tick")).expect("loaded");
        assert_eq!(tick.file.command_count(), 2);
        assert_eq!(tick.file.lines[1].command, r#"tellraw @a "bye""#);
        assert_eq!(tick.pack, "functions");
    }

    #[test]
    fn a_nested_zip_inside_a_pack_is_named_rather_than_ignored() {
        // The classic accident: the whole `datapacks/` folder zipped instead
        // of one pack. The inner archives are inert here and in Minecraft,
        // and an operator deserves to be told that in those words.
        let pack = MemPack::with_meta(
            "wrapped",
            &[
                ("inner_pack.zip", "not really a zip, but named like one"),
                ("data/minecraft/recipe/stick.json", typed_recipe()),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert!(data
            .get(&RegistryId::new("recipe"), &location("minecraft:stick"))
            .is_some());
        let finding = data
            .findings()
            .iter()
            .find(|f| f.message.contains("nested archive") || f.file == "inner_pack.zip")
            .expect("the nested archive is named");
        assert!(finding.message.contains("unpacked into it"), "{}", finding);
    }

    #[test]
    fn a_nested_zip_is_one_finding_even_when_there_are_several() {
        let files: Vec<(String, &str)> = (0..7)
            .map(|index| (format!("bundle{index}.zip"), "junk"))
            .collect();
        let owned: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let pack = MemPack::with_meta("many", &owned);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(
            data.findings()
                .iter()
                .filter(|f| f.message.contains("zip"))
                .count(),
            1,
            "{:?}",
            data.findings()
        );
        // Five are named; the rest are counted rather than listed.
        assert!(
            data.findings()[0].message.contains("and 2 more"),
            "{}",
            data.findings()[0]
        );
    }

    #[test]
    fn a_namespace_directory_holds_registries_and_not_loose_files() {
        let pack = MemPack::with_meta("loose", &[("data/minecraft/stray.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.findings().len(), 1, "{:?}", data.findings());
    }

    #[test]
    fn a_second_namespace_is_loaded_as_its_own() {
        let pack = MemPack::with_meta(
            "two",
            &[
                ("data/minecraft/recipe/a.json", "{}"),
                ("data/my_pack/recipe/b.json", "{}"),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert_eq!(
            data.namespaces().into_iter().collect::<Vec<_>>(),
            vec!["minecraft", "my_pack"]
        );
    }

    #[test]
    fn an_uppercase_file_name_is_an_error_that_explains_itself() {
        let pack = MemPack::with_meta("shouty", &[("data/minecraft/recipe/Stick.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 1, "{:?}", data.findings());
        assert!(
            data.findings()[0].message.contains("lowercase"),
            "{}",
            data.findings()[0]
        );
    }

    #[test]
    fn a_nested_resource_path_keeps_its_directories_in_the_name() {
        let pack = MemPack::with_meta(
            "nested",
            &[("data/minecraft/loot_table/blocks/stone.json", "{}")],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert!(data
            .get(
                &RegistryId::new("loot_table"),
                &location("minecraft:blocks/stone")
            )
            .is_some());
    }

    #[test]
    fn files_outside_data_are_not_findings() {
        // README, LICENSE and pack.png are in half the packs on the internet.
        // A warning that is always there teaches people to stop reading them.
        let pack = MemPack::with_meta(
            "documented",
            &[
                ("README.md", "hello"),
                ("pack.png", "not really a png"),
                ("assets/minecraft/lang/en_us.json", "{}"),
                ("data/minecraft/recipe/a.json", typed_recipe()),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert!(data.findings().is_empty(), "{:?}", data.findings());
        assert_eq!(data.packs()[0].files_read, 1);
    }
}
