//! Datapacks: reading `data/` from vanilla, from `datapacks/`, and from
//! whatever an operator put in them.
//!
//! # What a datapack is
//!
//! A tree of JSON under `data/<namespace>/<registry>/<path>.json`, plus a
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
//! # What this crate deliberately does not model
//!
//! Recipes, loot tables and advancements stay as [`serde_json::Value`]. They
//! are not turned into Rust structs here and they should not be turned into
//! generated Rust anywhere.
//!
//! The line is between an **identifier** and a **schema**. Block state ids,
//! registry ids and packet ids became generated Rust in Phase 0.5 because the
//! wire format depends on them: a codec writing id 1,234 cannot go and read a
//! file to find out what it means. A recipe is the other thing — it is the
//! datapack schema, the shape an operator's own files are full of, and reading
//! those files is this crate's entire job. Generating structs for them as well
//! would give Dust **two readers for one schema, and two readers of one schema
//! disagree**: one learns a new recipe type and the other does not, and the
//! result is a recipe that loads and then does nothing. This project already
//! made the identical argument about configuration, where environment
//! overrides are overlaid onto the parsed TOML *before* deserialisation so that
//! an override cannot reach the server having skipped a check.
//!
//! So: this crate answers *what files exist, which pack won, and is the JSON
//! well-formed*. The crate that consumes a recipe decides what a recipe is, and
//! [`json::unknown_keys`] is public so it reports its unknown keys in the same
//! words this one does.
//!
//! Also not modelled, on purpose: `function/` (`.mcfunction` is a command
//! language, not data), `structure/` (NBT, which belongs to `dust-nbt`), pack
//! `overlays` and `filter` sections, and feature flags. The first two are
//! [`registry::RegistryKind::Unread`] so their directories are not mistaken for
//! typos; the last three parse and produce a warning saying they are not
//! applied. Nothing is skipped silently.
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
//! # What the guards here do not catch
//!
//! * **Well-formed is not correct.** A recipe with no ingredients is valid JSON
//!   in the right directory with the right name, and nothing at this layer can
//!   tell. Half-validating it would be the two readers again.
//! * **`pack_format` is a claim.** A pack declaring 48 and containing 1.16 loot
//!   tables passes here and fails later.
//! * **Unknown keys are only checked in the shapes this crate owns** —
//!   `pack.mcmeta` and tag files. A misspelled key inside a recipe is invisible
//!   from here, by the same argument as above.
//! * **The registry table is vanilla 1.21.1's.** A mod's registry directory
//!   will be reported as unknown until somebody calls
//!   [`registry::Registries::with_extra`].
//! * **A vocabulary is optional and its absence is the default.** Read the
//!   unvalidated count before believing a clean run.

pub mod finding;
pub mod inflate;
pub mod json;
pub mod location;
pub mod meta;
pub mod pack;
pub mod registry;
pub mod tag;
pub mod vocabulary;
pub mod zip;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

pub use finding::{error_count, Finding, Severity};
pub use location::{LocationError, ResourceLocation, MINECRAFT};
pub use meta::{PackMeta, DUST_PACK_FORMAT};
pub use pack::{DirectoryPack, PackError, PackSource, ZipPack};
pub use registry::{Registries, RegistryDef, RegistryId, RegistryKind};
pub use tag::{MergedTag, ResolvedTags, TagStats};
pub use vocabulary::{Known, Vocabulary};

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
    /// `false` when the pack was refused — an unreadable `pack.mcmeta`, or a
    /// format Dust does not read under [`UnknownFormat::Reject`].
    pub loaded: bool,
    /// Files this pack contributed a resource or a tag from.
    pub files_read: usize,
    /// Files in the pack that were not read, for any reason.
    pub files_skipped: usize,
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
    /// Resources a later pack replaced.
    pub overrides: usize,
}

/// Everything the packs contained, overlaid.
#[derive(Debug, Default)]
pub struct LoadedData {
    packs: Vec<PackReport>,
    resources: BTreeMap<RegistryId, BTreeMap<ResourceLocation, Resource>>,
    tags: BTreeMap<RegistryId, TagSet>,
    member_registry: BTreeMap<RegistryId, String>,
    findings: Vec<Finding>,
    stats: LoadStats,
}

impl LoadedData {
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

    /// One tag as merged, before references are followed.
    pub fn merged_tag(&self, registry: &RegistryId, name: &ResourceLocation) -> Option<&MergedTag> {
        self.tags.get(registry)?.get(name)
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
        out
    }
}

/// Read every pack in order, later overriding earlier.
///
/// Errors are collected, never returned early. A pack with forty problems
/// produces forty findings from one run; making an operator fix one, restart,
/// and be told about the next teaches them to distrust the server rather than
/// the file.
pub fn load(packs: &[&dyn PackSource], options: &LoadOptions) -> LoadedData {
    let mut data = LoadedData {
        stats: LoadStats {
            packs_offered: packs.len(),
            ..LoadStats::default()
        },
        ..LoadedData::default()
    };

    for source in packs {
        let report = load_pack(*source, options, &mut data);
        if report.loaded {
            data.stats.packs_loaded += 1;
        }
        data.packs.push(report);
    }

    data.stats.resources = data.resources.values().map(BTreeMap::len).sum();
    data.stats.tags = data.tags.values().map(BTreeMap::len).sum();
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
    let mut tally = PackTally::default();

    for path in &listing {
        if read_file(source, &id, path, options, data, &mut tally) {
            report.files_read += 1;
        } else {
            report.files_skipped += 1;
        }
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
    data.stats.files_read += 1;
    true
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack held entirely in memory, so the loader's rules can be tested
    /// without a filesystem and without a fixture to keep in step.
    #[derive(Debug)]
    struct MemoryPack {
        id: String,
        files: BTreeMap<String, Vec<u8>>,
    }

    impl MemoryPack {
        fn new(id: &str, files: &[(&str, &str)]) -> Self {
            Self {
                id: id.to_owned(),
                files: files
                    .iter()
                    .map(|(path, body)| ((*path).to_owned(), body.as_bytes().to_vec()))
                    .collect(),
            }
        }

        fn with_meta(id: &str, files: &[(&str, &str)]) -> Self {
            let mut all = vec![(
                "pack.mcmeta",
                r#"{"pack":{"pack_format":48,"description":"test"}}"#,
            )];
            all.extend_from_slice(files);
            Self::new(id, &all)
        }
    }

    impl PackSource for MemoryPack {
        fn id(&self) -> &str {
            &self.id
        }

        fn origin(&self) -> String {
            format!("<memory:{}>", self.id)
        }

        fn list(&self) -> Result<Vec<String>, PackError> {
            Ok(self.files.keys().cloned().collect())
        }

        fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError> {
            Ok(self.files.get(path).cloned())
        }
    }

    fn location(text: &str) -> ResourceLocation {
        ResourceLocation::parse(text).expect("valid")
    }

    #[test]
    fn a_later_pack_overrides_an_earlier_one_and_the_earlier_is_remembered() {
        let base = MemoryPack::with_meta(
            "base",
            &[("data/minecraft/recipe/stick.json", r#"{"result":"stick"}"#)],
        );
        let over = MemoryPack::with_meta(
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
        let base = MemoryPack::with_meta(
            "base",
            &[(
                "data/minecraft/tags/block/logs.json",
                r#"{"values":["minecraft:oak_log"]}"#,
            )],
        );
        let over = MemoryPack::with_meta(
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
        let pack = MemoryPack::new("broken", &[("data/minecraft/recipe/stick.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.stats().packs_loaded, 0);
        assert_eq!(data.error_count(), 1, "{:?}", data.findings());
        assert!(data.findings()[0].message.contains("pack.mcmeta"));
    }

    #[test]
    fn a_pack_zipped_one_level_too_high_is_told_so() {
        let pack = MemoryPack::new(
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
        let old = MemoryPack::new(
            "old",
            &[
                (
                    "pack.mcmeta",
                    r#"{"pack":{"pack_format":15,"description":"d"}}"#,
                ),
                ("data/minecraft/recipe/stick.json", "{}"),
            ],
        );
        let current = MemoryPack::with_meta("current", &[("data/minecraft/recipe/rod.json", "{}")]);
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
        let old = MemoryPack::new(
            "old",
            &[
                (
                    "pack.mcmeta",
                    r#"{"pack":{"pack_format":15,"description":"d"}}"#,
                ),
                ("data/minecraft/recipe/stick.json", "{}"),
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
        let pack = MemoryPack::with_meta(
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
        let pack = MemoryPack::with_meta(
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
        let pack =
            MemoryPack::with_meta("old_layout", &[("data/minecraft/recipes/stick.json", "{}")]);
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
        let pack = MemoryPack::with_meta(
            "with_functions",
            &[("data/minecraft/function/tick.mcfunction", "say hi")],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.error_count(), 0, "{:?}", data.findings());
        assert!(
            data.findings()[0].message.contains(".mcfunction"),
            "{:?}",
            data.findings()
        );
    }

    #[test]
    fn a_namespace_directory_holds_registries_and_not_loose_files() {
        let pack = MemoryPack::with_meta("loose", &[("data/minecraft/stray.json", "{}")]);
        let data = load(&[&pack], &LoadOptions::default());
        assert_eq!(data.findings().len(), 1, "{:?}", data.findings());
    }

    #[test]
    fn a_second_namespace_is_loaded_as_its_own() {
        let pack = MemoryPack::with_meta(
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
        let pack = MemoryPack::with_meta("shouty", &[("data/minecraft/recipe/Stick.json", "{}")]);
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
        let pack = MemoryPack::with_meta(
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
        let pack = MemoryPack::with_meta(
            "documented",
            &[
                ("README.md", "hello"),
                ("pack.png", "not really a png"),
                ("assets/minecraft/lang/en_us.json", "{}"),
                ("data/minecraft/recipe/a.json", "{}"),
            ],
        );
        let data = load(&[&pack], &LoadOptions::default());
        assert!(data.findings().is_empty(), "{:?}", data.findings());
        assert_eq!(data.packs()[0].files_read, 1);
    }
}
