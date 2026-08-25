//! Tags: the one thing in a datapack that merges instead of overriding.
//!
//! # Why tags are different
//!
//! Every other resource is a definition. Two packs that both define
//! `minecraft:stone_pickaxe` are two answers to one question, and the later
//! pack's answer is the one that wins — that is what installing a pack *means*.
//!
//! A tag is not a definition, it is a **membership list**, and the question it
//! answers is "what is in this set". A pack that adds a copper axe to
//! `#minecraft:pickaxes` is not disagreeing with vanilla about what a pickaxe
//! is; it is adding one. If tags overrode, installing two mods that each add a
//! tool would silently give you whichever mod loaded last — and the operator
//! would see one working mod, one that appears installed and does nothing, and
//! no error anywhere.
//!
//! So the merge is the correct behaviour and `"replace": true` is the way a
//! pack says "no, I really do mean to throw the earlier list away" — which is
//! how you remove a vanilla entry, since there is no syntax for subtracting
//! one. Both are implemented here.
//!
//! # `required: false`
//!
//! An entry may be written `{"id": "somemod:thing", "required": false}`, which
//! means *include this if it exists and say nothing if it does not*. This form
//! exists so a pack can support several mods at once without needing all of
//! them installed, and treating it as required breaks every modpack. A missing
//! optional entry is dropped and **counted** — see
//! [`TagStats::optional_dropped`] — because a pack that is silently losing
//! half its entries is exactly the thing a count is for.
//!
//! # What the resolver guarantees
//!
//! * References are followed **transitively**. `#minecraft:logs` contains three
//!   tags, each of which contains more; the resolved answer is the flat set of
//!   actual blocks, and nothing downstream should ever have to follow a `#`.
//! * A cycle is an error naming its members, not a hang and not a stack
//!   overflow. The traversal uses an explicit stack for that second reason: a
//!   pack with sixty thousand tags could nest deeper than the call stack.
//! * A reference to a tag that does not exist names the missing tag **and the
//!   file that referenced it**, which is why every entry remembers where it
//!   came from.
//!
//! # What the resolver does not catch
//!
//! Whether a *non-reference* entry names anything real. `#minecraft:logs`
//! containing `minecraft:stobe` resolves to a set containing
//! `minecraft:stobe`, and only a [`Vocabulary`](crate::Vocabulary) can say that
//! is not a block. With the default [`Unchecked`](crate::vocabulary::Unchecked)
//! vocabulary, no entry is checked and [`TagStats::unvalidated_entries`] counts
//! every one of them, so that "0 problems" is never mistaken for "0 problems
//! found by a check that ran".

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::finding::Finding;
use crate::json;
use crate::registry::RegistryId;
use crate::vocabulary::{Known, Vocabulary};
use crate::ResourceLocation;

/// Keys a tag file may have.
const TAG_KEYS: &[&str] = &["values", "replace"];
/// Keys an entry object may have.
const ENTRY_KEYS: &[&str] = &["id", "required"];

/// Whether an entry names a member or another tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A name in the tag's own registry.
    Member,
    /// Written with a leading `#`: another tag, to be inlined.
    Reference,
}

/// One line of a tag's `values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEntry {
    pub id: ResourceLocation,
    pub kind: EntryKind,
    /// `false` only for `{"id": …, "required": false}`.
    pub required: bool,
    /// Index into [`MergedTag::sources`] — which pack's file wrote this line.
    pub source: usize,
}

/// One `tags/…/x.json` as it was written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagFile {
    /// `true` throws away everything packs before this one contributed.
    pub replace: bool,
    pub values: Vec<(ResourceLocation, EntryKind, bool)>,
}

impl TagFile {
    /// Read a tag file. Malformed entries are dropped with a finding each,
    /// rather than losing the whole file to one bad line.
    pub fn parse(value: &Value, pack: &str, file: &str) -> (Self, Vec<Finding>) {
        let mut findings = Vec::new();
        let Some(object) = value.as_object() else {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "is {}, but a tag file must be an object with a `values` list.",
                    json::kind_of(value)
                ),
            ));
            return (Self::default(), findings);
        };
        findings.extend(json::unknown_keys(object, TAG_KEYS, pack, file, ""));

        let replace = match object.get("replace") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(other) => {
                findings.push(Finding::error(
                    pack,
                    file,
                    format!(
                        "has `replace` as {}, but it must be true or false. \
                         Treating it as false, which appends.",
                        json::kind_of(other)
                    ),
                ));
                false
            }
        };

        let mut values = Vec::new();
        match object.get("values") {
            None | Some(Value::Null) => findings.push(Finding::warning(
                pack,
                file,
                "has no `values` list, so it contributes nothing. An empty tag \
                 is written `\"values\": []`.",
            )),
            Some(Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    match parse_entry(item, pack, file, index, &mut findings) {
                        Ok(entry) => values.push(entry),
                        Err(finding) => findings.push(finding),
                    }
                }
            }
            Some(other) => findings.push(Finding::error(
                pack,
                file,
                format!(
                    "has `values` as {}, but it must be a list.",
                    json::kind_of(other)
                ),
            )),
        }

        (Self { replace, values }, findings)
    }
}

fn parse_entry(
    item: &Value,
    pack: &str,
    file: &str,
    index: usize,
    findings: &mut Vec<Finding>,
) -> Result<(ResourceLocation, EntryKind, bool), Finding> {
    let (written, required) = match item {
        Value::String(text) => (text.as_str(), true),
        Value::Object(object) => {
            findings.extend(json::unknown_keys(
                object,
                ENTRY_KEYS,
                pack,
                file,
                &format!("values[{index}]"),
            ));
            let Some(Value::String(text)) = object.get("id") else {
                return Err(Finding::error(
                    pack,
                    file,
                    format!(
                        "has an entry at position {index} with no `id` string. An \
                         entry is either a name or `{{\"id\": \"…\", \"required\": false}}`."
                    ),
                ));
            };
            let required = match object.get("required") {
                None | Some(Value::Null) => true,
                Some(Value::Bool(flag)) => *flag,
                Some(other) => {
                    return Err(Finding::error(
                        pack,
                        file,
                        format!(
                            "has an entry at position {index} whose `required` is {}, \
                             but it must be true or false.",
                            json::kind_of(other)
                        ),
                    ))
                }
            };
            (text.as_str(), required)
        }
        other => {
            return Err(Finding::error(
                pack,
                file,
                format!(
                    "has an entry at position {index} that is {}. An entry is a \
                     name, or `#` and a tag name, or an object with an `id`.",
                    json::kind_of(other)
                ),
            ))
        }
    };

    let (kind, body) = match written.strip_prefix('#') {
        Some(rest) => (EntryKind::Reference, rest),
        None => (EntryKind::Member, written),
    };
    let id = ResourceLocation::parse(body).map_err(|error| {
        Finding::error(
            pack,
            file,
            format!("has an entry at position {index} that {error}"),
        )
    })?;
    Ok((id, kind, required))
}

/// Which pack and file one entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSource {
    pub pack: String,
    pub file: String,
}

/// One tag after every pack has had its say.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergedTag {
    pub entries: Vec<TagEntry>,
    /// Every pack that contributed, in load order. A `replace` clears this,
    /// so what is left is the packs whose lines are actually still in
    /// `entries` — which is the answer to "where did this come from".
    pub sources: Vec<TagSource>,
}

impl MergedTag {
    /// Fold one pack's file into the tag so far.
    pub fn apply(&mut self, file: &TagFile, pack: &str, path: &str) {
        if file.replace {
            self.entries.clear();
            self.sources.clear();
        }
        let source = self.sources.len();
        self.sources.push(TagSource {
            pack: pack.to_owned(),
            file: path.to_owned(),
        });
        for (id, kind, required) in &file.values {
            self.entries.push(TagEntry {
                id: id.clone(),
                kind: *kind,
                required: *required,
                source,
            });
        }
    }

    fn source_of(&self, entry: &TagEntry) -> (&str, &str) {
        match self.sources.get(entry.source) {
            Some(source) => (source.pack.as_str(), source.file.as_str()),
            None => ("", ""),
        }
    }
}

/// Every tag of one registry, merged but not yet resolved.
pub type TagSet = BTreeMap<ResourceLocation, MergedTag>;

/// Every tag of one registry, flattened: no `#` left anywhere.
pub type FlatTags = BTreeMap<ResourceLocation, BTreeSet<ResourceLocation>>;

/// A `#a` → `#b` → `#a` loop, in the order it was walked.
///
/// The first and last members are the same tag, so the message reads as a
/// round trip rather than as a list somebody has to close themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCycle {
    pub registry: RegistryId,
    pub members: Vec<ResourceLocation>,
}

impl std::fmt::Display for TagCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self
            .members
            .iter()
            .map(|member| format!("##{member}"))
            .collect();
        write!(f, "{}", names.join(" → "))
    }
}

/// The numbers a tag resolution produces, which are the ones worth reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagStats {
    pub tags: usize,
    /// Entries as written, after merging and before following any `#`.
    pub entries_before: usize,
    /// Members in the flattened sets, summed over every tag.
    pub entries_after: usize,
    /// Optional entries dropped because nothing of that name exists.
    pub optional_dropped: usize,
    /// Entries no [`Vocabulary`] could say anything about. Not "valid" — see
    /// the module documentation.
    pub unvalidated_entries: usize,
    /// Entries a vocabulary confirmed exist.
    pub validated_entries: usize,
    /// Longest chain of tags reachable by `#` references, counting the tag
    /// itself, and the tag that achieves it.
    pub deepest_chain: usize,
    pub deepest_tag: Option<(RegistryId, ResourceLocation)>,
    /// Largest resolved set, and the tag that achieves it.
    pub widest_tag: Option<(RegistryId, ResourceLocation)>,
    pub widest_size: usize,
}

/// Tags with every `#` followed and every set flattened.
#[derive(Debug, Clone, Default)]
pub struct ResolvedTags {
    tags: BTreeMap<RegistryId, FlatTags>,
    cycles: Vec<TagCycle>,
    stats: TagStats,
}

impl ResolvedTags {
    /// Everything in `tag`, flattened.
    pub fn get(
        &self,
        registry: &RegistryId,
        tag: &ResourceLocation,
    ) -> Option<&BTreeSet<ResourceLocation>> {
        self.tags.get(registry)?.get(tag)
    }

    /// Every tag of one registry.
    pub fn registry(&self, registry: &RegistryId) -> Option<&FlatTags> {
        self.tags.get(registry)
    }

    pub fn registries(&self) -> impl Iterator<Item = &RegistryId> {
        self.tags.keys()
    }

    pub fn cycles(&self) -> &[TagCycle] {
        &self.cycles
    }

    pub fn stats(&self) -> &TagStats {
        &self.stats
    }
}

/// Follow every `#` reference and flatten every tag.
///
/// `member_registry` says, for each tag registry, which registry its
/// non-reference entries name — `tags/block` holds blocks. That is what the
/// vocabulary is asked about.
pub fn resolve(
    merged: &BTreeMap<RegistryId, TagSet>,
    member_registry: &BTreeMap<RegistryId, String>,
    vocabulary: &dyn Vocabulary,
) -> (ResolvedTags, Vec<Finding>) {
    let mut resolved = ResolvedTags::default();
    let mut findings = Vec::new();

    for (registry, tags) in merged {
        let unknown = String::new();
        let members = member_registry.get(registry).unwrap_or(&unknown);
        let walked = resolve_one(
            registry,
            tags,
            members,
            vocabulary,
            &mut findings,
            &mut resolved.stats,
        );
        resolved.stats.tags += tags.len();
        for (name, set) in &walked.flat {
            resolved.stats.entries_after += set.len();
            if set.len() > resolved.stats.widest_size {
                resolved.stats.widest_size = set.len();
                resolved.stats.widest_tag = Some((registry.clone(), name.clone()));
            }
        }
        for (name, depth) in walked.depths {
            if depth > resolved.stats.deepest_chain {
                resolved.stats.deepest_chain = depth;
                resolved.stats.deepest_tag = Some((registry.clone(), name));
            }
        }
        for tag in tags.values() {
            resolved.stats.entries_before += tag.entries.len();
        }
        resolved.cycles.extend(walked.cycles);
        resolved.tags.insert(registry.clone(), walked.flat);
    }

    (resolved, findings)
}

/// Where a tag is in the depth-first walk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Colour {
    Unvisited,
    /// On the current path. A reference to one of these is a cycle.
    InProgress,
    Done,
}

enum Step<'a> {
    Enter(&'a ResourceLocation),
    Leave(&'a ResourceLocation),
}

/// One registry's tags after the walk.
#[derive(Debug, Default)]
struct RegistryResolution {
    flat: FlatTags,
    cycles: Vec<TagCycle>,
    /// The longest chain of `#` references starting at each tag, counting the
    /// tag itself, so a tag with no references is 1.
    depths: Vec<(ResourceLocation, usize)>,
}

fn resolve_one(
    registry: &RegistryId,
    tags: &TagSet,
    member_registry: &str,
    vocabulary: &dyn Vocabulary,
    findings: &mut Vec<Finding>,
    stats: &mut TagStats,
) -> RegistryResolution {
    let mut colour: BTreeMap<&ResourceLocation, Colour> = BTreeMap::new();
    let mut flat: FlatTags = FlatTags::new();
    let mut depth: BTreeMap<&ResourceLocation, usize> = BTreeMap::new();
    let mut cycles = Vec::new();
    // The tags currently on the path, so a back edge can be printed as the
    // loop it is rather than as the single tag it landed on.
    let mut path: Vec<&ResourceLocation> = Vec::new();

    for root in tags.keys() {
        if colour.get(root).copied().unwrap_or(Colour::Unvisited) == Colour::Done {
            continue;
        }
        // An explicit stack rather than recursion: the depth here is the depth
        // of a pack author's tag graph, and nothing stops that being tens of
        // thousands.
        let mut stack = vec![Step::Enter(root)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(name) => {
                    match colour.get(name).copied().unwrap_or(Colour::Unvisited) {
                        Colour::Done | Colour::InProgress => continue,
                        Colour::Unvisited => {}
                    }
                    colour.insert(name, Colour::InProgress);
                    path.push(name);
                    stack.push(Step::Leave(name));

                    let tag = &tags[name];
                    for entry in &tag.entries {
                        if entry.kind != EntryKind::Reference {
                            continue;
                        }
                        let (pack, file) = tag.source_of(entry);
                        match tags.get_key_value(&entry.id) {
                            None => {
                                if entry.required {
                                    findings.push(
                                        Finding::error(
                                            pack,
                                            file,
                                            format!(
                                                "refers to the tag `#{}`, which no pack \
                                                 defines. Expected it at \
                                                 `data/{}/{registry}/{}.json`.{}",
                                                entry.id,
                                                entry.id.namespace(),
                                                entry.id.path(),
                                                crate::finding::suggestion(
                                                    entry.id.as_str(),
                                                    tags.keys().map(ResourceLocation::as_str),
                                                ),
                                            ),
                                        )
                                        .about(name.clone()),
                                    );
                                } else {
                                    stats.optional_dropped += 1;
                                }
                            }
                            Some((key, _)) => {
                                match colour.get(key).copied().unwrap_or(Colour::Unvisited) {
                                    Colour::InProgress => {
                                        let cycle = cycle_from(registry, &path, key);
                                        findings.push(
                                            Finding::error(
                                                pack,
                                                file,
                                                format!(
                                                    "is part of a loop of tag references: \
                                                     {cycle}. A tag cannot contain itself, \
                                                     however many steps it takes."
                                                ),
                                            )
                                            .about(name.clone()),
                                        );
                                        cycles.push(cycle);
                                    }
                                    Colour::Done => {}
                                    Colour::Unvisited => stack.push(Step::Enter(key)),
                                }
                            }
                        }
                    }
                }
                Step::Leave(name) => {
                    path.pop();
                    let tag = &tags[name];
                    let mut set = BTreeSet::new();
                    let mut chain = 1usize;
                    for entry in &tag.entries {
                        match entry.kind {
                            EntryKind::Member => {
                                let (pack, file) = tag.source_of(entry);
                                match vocabulary.contains(member_registry, &entry.id) {
                                    Known::Yes => {
                                        stats.validated_entries += 1;
                                        set.insert(entry.id.clone());
                                    }
                                    Known::Unknown => {
                                        stats.unvalidated_entries += 1;
                                        set.insert(entry.id.clone());
                                    }
                                    Known::No if entry.required => {
                                        findings.push(
                                            Finding::error(
                                                pack,
                                                file,
                                                format!(
                                                    "lists `{}`, which is not in the \
                                                     `{member_registry}` registry.{}",
                                                    entry.id,
                                                    vocabulary
                                                        .suggest(member_registry, &entry.id)
                                                        .map(|s| format!(" Did you mean `{s}`?"))
                                                        .unwrap_or_default(),
                                                ),
                                            )
                                            .about(name.clone()),
                                        );
                                    }
                                    Known::No => stats.optional_dropped += 1,
                                }
                            }
                            EntryKind::Reference => {
                                // A reference still in progress is the back
                                // edge of a cycle already reported at Enter.
                                if colour.get(&entry.id).copied() == Some(Colour::Done) {
                                    if let Some(inner) = flat.get(&entry.id) {
                                        set.extend(inner.iter().cloned());
                                    }
                                    chain =
                                        chain.max(1 + depth.get(&entry.id).copied().unwrap_or(0));
                                }
                            }
                        }
                    }
                    // Insert the key from the map so the borrow outlives the loop.
                    let (key, _) = tags.get_key_value(name).expect("walked from this map");
                    depth.insert(key, chain);
                    flat.insert(name.clone(), set);
                    colour.insert(name, Colour::Done);
                }
            }
        }
    }

    let depths = depth
        .into_iter()
        .map(|(name, value)| (name.clone(), value))
        .collect();
    RegistryResolution {
        flat,
        cycles,
        depths,
    }
}

/// The loop that closes back onto `target`, as a list ending where it started.
fn cycle_from(
    registry: &RegistryId,
    path: &[&ResourceLocation],
    target: &ResourceLocation,
) -> TagCycle {
    let start = path
        .iter()
        .position(|member| *member == target)
        .unwrap_or(0);
    let mut members: Vec<ResourceLocation> = path[start..].iter().map(|m| (*m).clone()).collect();
    members.push(target.clone());
    TagCycle {
        registry: registry.clone(),
        members,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocabulary::{KnownNames, Unchecked};

    fn location(text: &str) -> ResourceLocation {
        ResourceLocation::parse(text).expect("valid")
    }

    fn tag_file(json: &str) -> TagFile {
        let value: Value = serde_json::from_str(json).expect("valid json");
        let (file, findings) = TagFile::parse(&value, "p", "f.json");
        assert!(findings.is_empty(), "{findings:?}");
        file
    }

    /// Build one registry's worth of tags from `(name, json)` pairs, each from
    /// its own pack so that merge order is visible.
    fn merge(files: &[(&str, &str)]) -> TagSet {
        let mut set = TagSet::new();
        for (name, json) in files {
            let entry = set.entry(location(name)).or_default();
            entry.apply(&tag_file(json), "p", &format!("tags/block/{name}.json"));
        }
        set
    }

    fn resolve_blocks(tags: TagSet, vocabulary: &dyn Vocabulary) -> (ResolvedTags, Vec<Finding>) {
        let registry = RegistryId::new("tags/block");
        let mut merged = BTreeMap::new();
        merged.insert(registry.clone(), tags);
        let mut members = BTreeMap::new();
        members.insert(registry, "block".to_owned());
        resolve(&merged, &members, vocabulary)
    }

    #[test]
    fn a_string_entry_and_an_object_entry_mean_the_same_thing() {
        let plain = tag_file(r##"{"values":["minecraft:stone"]}"##);
        let object = tag_file(r##"{"values":[{"id":"minecraft:stone"}]}"##);
        assert_eq!(plain.values, object.values);
        assert!(plain.values[0].2, "a bare entry is required");
    }

    #[test]
    fn a_hash_makes_an_entry_a_reference() {
        let file = tag_file(r##"{"values":["#minecraft:logs","minecraft:stone"]}"##);
        assert_eq!(file.values[0].1, EntryKind::Reference);
        assert_eq!(file.values[0].0, location("minecraft:logs"));
        assert_eq!(file.values[1].1, EntryKind::Member);
    }

    #[test]
    fn an_optional_entry_is_marked_optional() {
        let file = tag_file(r##"{"values":[{"id":"somemod:thing","required":false}]}"##);
        assert!(!file.values[0].2);
    }

    #[test]
    fn a_reference_may_also_be_optional() {
        // `{"id": "#somemod:tag", "required": false}` is the form a pack uses
        // to depend on a mod's tag without depending on the mod.
        let file = tag_file(r##"{"values":[{"id":"#somemod:tag","required":false}]}"##);
        assert_eq!(file.values[0].1, EntryKind::Reference);
        assert!(!file.values[0].2);
    }

    #[test]
    fn a_second_pack_appends_by_default() {
        let tags = merge(&[
            (
                "minecraft:pickaxes",
                r##"{"values":["minecraft:stone_pickaxe"]}"##,
            ),
            (
                "minecraft:pickaxes",
                r##"{"values":["copper:copper_pickaxe"]}"##,
            ),
        ]);
        let merged = &tags[&location("minecraft:pickaxes")];
        assert_eq!(merged.entries.len(), 2);
        assert_eq!(merged.sources.len(), 2);
    }

    #[test]
    fn replace_throws_the_earlier_list_away() {
        let tags = merge(&[
            (
                "minecraft:pickaxes",
                r##"{"values":["minecraft:stone_pickaxe"]}"##,
            ),
            (
                "minecraft:pickaxes",
                r##"{"replace":true,"values":["copper:copper_pickaxe"]}"##,
            ),
        ]);
        let merged = &tags[&location("minecraft:pickaxes")];
        assert_eq!(merged.entries.len(), 1);
        assert_eq!(merged.entries[0].id, location("copper:copper_pickaxe"));
        // And the pack that was thrown away is no longer listed as a source,
        // because it no longer contributed anything.
        assert_eq!(merged.sources.len(), 1);
    }

    #[test]
    fn references_are_followed_all_the_way_down() {
        let tags = merge(&[
            (
                "minecraft:logs",
                r##"{"values":["#minecraft:oak_logs","#minecraft:birch_logs"]}"##,
            ),
            (
                "minecraft:oak_logs",
                r##"{"values":["minecraft:oak_log","minecraft:oak_wood"]}"##,
            ),
            (
                "minecraft:birch_logs",
                r##"{"values":["minecraft:birch_log"]}"##,
            ),
        ]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(findings.is_empty(), "{findings:?}");
        let logs = resolved
            .get(&RegistryId::new("tags/block"), &location("minecraft:logs"))
            .expect("resolved");
        assert_eq!(logs.len(), 3);
        assert!(logs.contains(&location("minecraft:birch_log")));
        assert_eq!(resolved.stats().deepest_chain, 2);
    }

    #[test]
    fn a_chain_three_deep_resolves_and_is_measured() {
        let tags = merge(&[
            ("minecraft:a", r##"{"values":["#minecraft:b"]}"##),
            ("minecraft:b", r##"{"values":["#minecraft:c"]}"##),
            ("minecraft:c", r##"{"values":["minecraft:stone"]}"##),
        ]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(
            resolved
                .get(&RegistryId::new("tags/block"), &location("minecraft:a"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(resolved.stats().deepest_chain, 3);
    }

    #[test]
    fn a_cycle_is_an_error_naming_its_members() {
        let tags = merge(&[
            ("minecraft:a", r##"{"values":["#minecraft:b"]}"##),
            ("minecraft:b", r##"{"values":["#minecraft:a"]}"##),
        ]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert_eq!(resolved.cycles().len(), 1, "{:?}", resolved.cycles());
        let printed = resolved.cycles()[0].to_string();
        assert!(printed.contains("minecraft:a"), "{printed}");
        assert!(printed.contains("minecraft:b"), "{printed}");
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_tag_that_contains_itself_is_a_cycle_of_one_step() {
        let tags = merge(&[("minecraft:a", r##"{"values":["#minecraft:a"]}"##)]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert_eq!(resolved.cycles().len(), 1);
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_long_cycle_terminates_rather_than_hanging() {
        // Two hundred tags in a ring. The assertion that matters is that this
        // test finishes at all.
        let names: Vec<String> = (0..200).map(|i| format!("minecraft:t{i}")).collect();
        let mut tags = TagSet::new();
        for (index, name) in names.iter().enumerate() {
            let next = &names[(index + 1) % names.len()];
            let json = format!(r##"{{"values":["#{next}"]}}"##);
            tags.entry(location(name))
                .or_default()
                .apply(&tag_file(&json), "p", "f.json");
        }
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(!resolved.cycles().is_empty());
        assert!(!findings.is_empty());
    }

    #[test]
    fn a_diamond_is_not_a_cycle() {
        // `a` reaches `d` two ways. A colour scheme that marked "seen" rather
        // than "on the current path" would call this a loop.
        let tags = merge(&[
            (
                "minecraft:a",
                r##"{"values":["#minecraft:b","#minecraft:c"]}"##,
            ),
            ("minecraft:b", r##"{"values":["#minecraft:d"]}"##),
            ("minecraft:c", r##"{"values":["#minecraft:d"]}"##),
            ("minecraft:d", r##"{"values":["minecraft:stone"]}"##),
        ]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(resolved.cycles().is_empty(), "{:?}", resolved.cycles());
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(resolved.stats().deepest_chain, 3);
    }

    #[test]
    fn a_missing_required_reference_names_the_tag_and_the_file() {
        let tags = merge(&[("minecraft:a", r##"{"values":["#minecraft:nowhere"]}"##)]);
        let (_, findings) = resolve_blocks(tags, &Unchecked);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("minecraft:nowhere"),
            "{}",
            findings[0]
        );
        assert!(
            findings[0].file.contains("minecraft:a.json"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn a_missing_optional_reference_is_dropped_and_counted() {
        let tags = merge(&[(
            "minecraft:a",
            r##"{"values":[{"id":"#somemod:nowhere","required":false},"minecraft:stone"]}"##,
        )]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(resolved.stats().optional_dropped, 1);
        assert_eq!(
            resolved
                .get(&RegistryId::new("tags/block"), &location("minecraft:a"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn without_a_vocabulary_nothing_is_validated_and_the_count_says_so() {
        let tags = merge(&[("minecraft:a", r##"{"values":["minecraft:not_a_block"]}"##)]);
        let (resolved, findings) = resolve_blocks(tags, &Unchecked);
        assert!(findings.is_empty());
        assert_eq!(resolved.stats().unvalidated_entries, 1);
        assert_eq!(resolved.stats().validated_entries, 0);
    }

    #[test]
    fn with_a_vocabulary_an_unknown_required_member_is_an_error_with_a_suggestion() {
        let tags = merge(&[("minecraft:a", r##"{"values":["minecraft:stobe"]}"##)]);
        let vocabulary = KnownNames::new().with("block", [location("minecraft:stone")]);
        let (resolved, findings) = resolve_blocks(tags, &vocabulary);
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
        assert!(
            findings[0]
                .message
                .contains("Did you mean `minecraft:stone`?"),
            "{}",
            findings[0]
        );
        assert_eq!(resolved.stats().unvalidated_entries, 0);
    }

    #[test]
    fn with_a_vocabulary_an_unknown_optional_member_is_dropped_and_counted() {
        let tags = merge(&[(
            "minecraft:a",
            r##"{"values":[{"id":"somemod:thing","required":false},"minecraft:stone"]}"##,
        )]);
        let vocabulary = KnownNames::new().with("block", [location("minecraft:stone")]);
        let (resolved, findings) = resolve_blocks(tags, &vocabulary);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(resolved.stats().optional_dropped, 1);
        assert_eq!(resolved.stats().validated_entries, 1);
        assert_eq!(
            resolved
                .get(&RegistryId::new("tags/block"), &location("minecraft:a"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_widest_tag_is_reported() {
        let tags = merge(&[
            ("minecraft:one", r##"{"values":["minecraft:a"]}"##),
            (
                "minecraft:three",
                r##"{"values":["minecraft:a","minecraft:b","minecraft:c"]}"##,
            ),
        ]);
        let (resolved, _) = resolve_blocks(tags, &Unchecked);
        assert_eq!(resolved.stats().widest_size, 3);
        assert_eq!(
            resolved.stats().widest_tag.as_ref().map(|(_, n)| n.clone()),
            Some(location("minecraft:three"))
        );
    }

    #[test]
    fn duplicate_entries_collapse_in_the_resolved_set_but_not_in_the_written_one() {
        // Two packs both adding the same block is normal and must not produce
        // it twice; the written entry count still shows both lines, which is
        // what makes "entries before" and "entries after" different numbers.
        let tags = merge(&[
            ("minecraft:a", r##"{"values":["minecraft:stone"]}"##),
            ("minecraft:a", r##"{"values":["minecraft:stone"]}"##),
        ]);
        let (resolved, _) = resolve_blocks(tags, &Unchecked);
        assert_eq!(resolved.stats().entries_before, 2);
        assert_eq!(resolved.stats().entries_after, 1);
    }

    #[test]
    fn a_malformed_entry_costs_one_line_and_not_the_file() {
        let value: Value =
            serde_json::from_str(r##"{"values":["minecraft:stone", 7, "minecraft:sand"]}"##)
                .unwrap();
        let (file, findings) = TagFile::parse(&value, "p", "f.json");
        assert_eq!(file.values.len(), 2);
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].message.contains("position 1"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn an_unknown_key_in_a_tag_file_is_reported() {
        let value: Value = serde_json::from_str(r##"{"values":[],"remove":[]}"##).unwrap();
        let (_, findings) = TagFile::parse(&value, "p", "f.json");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("`remove`"), "{}", findings[0]);
    }

    #[test]
    fn a_tag_file_with_no_values_says_so_rather_than_being_an_empty_tag() {
        let value: Value = serde_json::from_str(r##"{"replace":true}"##).unwrap();
        let (_, findings) = TagFile::parse(&value, "p", "f.json");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, crate::Severity::Warning);
    }
}
