//! `/reload`: build the next world completely, then hand it over in one move.
//!
//! # Why the running server never sees a half-built stack
//!
//! A reload has two audiences and they want opposite things. The **operator**
//! wants the new packs read immediately and every problem reported at once;
//! the **players** want the world they are standing in to keep behaving while
//! that happens. Both are served by the same shape: the new stack is built
//! and validated *in full*, off to the side, and only then does the handle
//! the rest of the server reads through switch over — atomically, to one
//! complete old world or one complete new one. Nothing ever observes a
//! mixture.
//!
//! That is why [`ReloadHandle`] hands out [`Arc`] snapshots instead of
//! lending references. A reader takes a snapshot once and works through it
//! for as long as it likes; swaps underneath neither block it nor corrupt
//! it, because the snapshot it holds is immutable. The cost is one reference
//! count per reader per snapshot, which is the cheapest possible price for
//! "the data behind me cannot change".
//!
//! The swap itself is a short write-locked pointer replacement rather than
//! a wait-free algorithm. Readers never queue behind a rebuild — the rebuild
//! happens outside the lock — and the critical section is an `Arc` swap plus
//! a diff walk over ordered maps. Reaching for a fancier scheme would buy
//! nanoseconds at the price of the one thing this module cannot have: a
//! subtle concurrency story.
//!
//! # Failure keeps the old world
//!
//! [`ReloadPolicy::RequireClean`] is how a server refuses a broken stack:
//! if the candidate carries errors — files that did not load, parent loops
//! an advancement graph cannot resolve — the swap does not happen and the
//! previous stack stays exactly what it was. The rejected candidate's
//! findings come back in the [`RejectedReload`] so the operator can read
//! *why*, which is what makes refusal different from silence. Warnings do
//! not block: a pack whose cosmetic warning was made fatal by policy would
//! turn "loaded with notes" into "not loaded", which serves nobody.
//!
//! # What the diff reports
//!
//! Every definition that changed hands: resources and functions added,
//! removed, or replaced — with the pack that held each side, and whether a
//! replacement changed the document itself or merely moved provenance (two
//! packs shipping byte-identical copies is ordinary in modpacks). Tags are
//! reported separately and only by outcome, because a tag merges across
//! packs and "who contributed" is already answered by
//! [`crate::MergedTag::sources`]; what a reload summary wants is that the
//! answer changed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::finding::{error_count, Finding};
use crate::function::LoadedFunction;
use crate::location::ResourceLocation;
use crate::registry::RegistryId;
use crate::{LoadOptions, LoadedData, PackSource, Resource};

/// A live view of the loaded packs, replaceable in one step.
///
/// Clone-free reads: [`snapshot`](Self::snapshot) returns an owned
/// [`Arc`], so the caller holds its world steady for as long as it needs
/// while reloads happen around it. See the module documentation.
#[derive(Debug, Default)]
pub struct ReloadHandle {
    current: RwLock<Arc<LoadedData>>,
}

impl ReloadHandle {
    /// Start from an already-loaded stack.
    pub fn starting(data: LoadedData) -> Self {
        Self {
            current: RwLock::new(Arc::new(data)),
        }
    }

    /// Start from nothing — the state before the first load finished.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The current stack, held steady until released.
    pub fn snapshot(&self) -> Arc<LoadedData> {
        self.current.read().expect("reload lock").clone()
    }

    /// Replace the stack with `candidate`, returning what changed.
    ///
    /// This is the low-level half: it trusts whoever built `candidate`.
    /// The guarded version is [`Self::reload`], which builds, validates and
    /// may refuse.
    pub fn install(&self, candidate: LoadedData) -> ReloadDiff {
        let mut guard = self.current.write().expect("reload lock");
        let diff = ReloadDiff::between(&guard, &candidate);
        *guard = Arc::new(candidate);
        diff
    }

    /// Build the stack from `sources`, validate it, and either swap it in or
    /// refuse and keep the current one.
    ///
    /// Validation is everything this crate can check without help: the
    /// load's own findings, then the advancement-graph pass, since parents
    /// routinely live in another pack and only exist once the whole stack is
    /// in hand. Under [`ReloadPolicy::RequireClean`] any error refuses the
    /// swap; warnings never do.
    pub fn reload(
        &self,
        sources: &[&dyn PackSource],
        options: &LoadOptions,
        policy: ReloadPolicy,
    ) -> Result<ReloadReport, RejectedReload> {
        let candidate = crate::load(sources, options);
        let (graph, advancement_findings) = crate::advancement::validate(&candidate);

        let blocked = match policy {
            ReloadPolicy::AcceptFindings => false,
            ReloadPolicy::RequireClean => {
                error_count(candidate.findings()) + error_count(&advancement_findings) > 0
            }
        };
        if blocked {
            let mut refused = candidate.findings().to_vec();
            refused.extend(advancement_findings);
            return Err(RejectedReload {
                cycles: graph.cycles,
                findings: refused,
            });
        }

        let diff = {
            let mut guard = self.current.write().expect("reload lock");
            let diff = ReloadDiff::between(&guard, &candidate);
            *guard = Arc::new(candidate);
            diff
        };
        Ok(ReloadReport {
            diff,
            advancement_findings,
        })
    }
}

/// How strict a reload is about the candidate's problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadPolicy {
    /// Swap in whatever loaded; findings ride along on the new stack.
    #[default]
    AcceptFindings,
    /// Any error in the candidate keeps the current stack.
    RequireClean,
}

/// What a successful reload left behind.
#[derive(Debug)]
pub struct ReloadReport {
    pub diff: ReloadDiff,
    /// Graph-level findings about the *new* stack's advancements. Kept
    /// beside the diff rather than folded into the data, because the data's
    /// own findings were fixed when the load ran and these came after.
    pub advancement_findings: Vec<Finding>,
}

/// A refused reload: the current stack is untouched, and this is why.
#[derive(Debug)]
pub struct RejectedReload {
    /// Parent loops found in the candidate, if any — named here because a
    /// cycle is easier to fix from the loop than from one finding per file.
    pub cycles: Vec<crate::AdvancementCycle>,
    pub findings: Vec<Finding>,
}

/// Everything that changed between two stacks, old to new.
///
/// Every list is ordered by registry then name, so the rendering is stable
/// and two diffs can be compared as text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReloadDiff {
    pub added: Vec<Definition>,
    pub removed: Vec<Definition>,
    pub replaced: Vec<Replacement>,
    /// Tags whose merged form differs in any way, including appearing or
    /// disappearing. Counts are written entries, before and after.
    pub tags_changed: Vec<TagChange>,
}

/// One definition — a resource or a function — that appeared or vanished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub registry: RegistryId,
    pub name: ResourceLocation,
    /// The pack that holds it now (added) or held it (removed).
    pub pack: String,
}

/// One definition both stacks held, whose holder or contents changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub registry: RegistryId,
    pub name: ResourceLocation,
    /// The pack that won before.
    pub from_pack: String,
    /// The pack that wins now.
    pub to_pack: String,
    /// `true` when the winning document itself differs — as opposed to two
    /// packs shipping identical copies and only the provenance moving.
    pub content_changed: bool,
}

/// One tag whose merged form changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChange {
    pub registry: RegistryId,
    pub name: ResourceLocation,
    /// Written entries before the reload. Zero for a tag that only now
    /// appeared.
    pub entries_before: usize,
    /// Written entries after. Zero for a tag that disappeared.
    pub entries_after: usize,
}

impl ReloadDiff {
    /// Compare two stacks exhaustively.
    pub fn between(old: &LoadedData, new: &LoadedData) -> Self {
        let mut diff = Self::default();

        for registry in union_registries(old, new) {
            if registry.is_tags() {
                diff_tags(old, new, &registry, &mut diff.tags_changed);
                continue;
            }
            let is_function_registry = registry.as_str() == FUNCTION_REGISTRY;
            let old_side = side(old, &registry, is_function_registry);
            let new_side = side(new, &registry, is_function_registry);
            diff_definitions(&registry, old_side.as_ref(), new_side.as_ref(), &mut diff);
        }

        diff
    }

    /// Whether nothing at all changed — common when nobody touched the packs.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.replaced.is_empty()
            && self.tags_changed.is_empty()
    }

    /// The whole diff as stable, human-readable text, one line per change.
    ///
    /// Built for a server log line after `/reload`; the ordering guarantee
    /// makes it diffable the same way [`LoadedData::diagnostic_dump`] is.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for change in &self.added {
            out.push_str(&format!(
                "+ {}:{} <- {}\n",
                change.registry, change.name, change.pack
            ));
        }
        for change in &self.removed {
            out.push_str(&format!(
                "- {}:{} (was {}) \n",
                change.registry, change.name, change.pack
            ));
        }
        for change in &self.replaced {
            out.push_str(&format!(
                "~ {}:{} <- {} (was {}){}\n",
                change.registry,
                change.name,
                change.to_pack,
                change.from_pack,
                if change.content_changed {
                    ""
                } else {
                    ", identical"
                }
            ));
        }
        for change in &self.tags_changed {
            out.push_str(&format!(
                "~ #{}:{} entries {} -> {}\n",
                change.registry, change.name, change.entries_before, change.entries_after
            ));
        }
        out
    }
}

const FUNCTION_REGISTRY: &str = "function";

/// Which map a registry's winners live in, hidden behind an enum-free pair
/// of views. Functions have no JSON value; resources do.
fn side(
    data: &LoadedData,
    registry: &RegistryId,
    is_function_registry: bool,
) -> Option<BTreeMap<ResourceLocation, SideEntry>> {
    if is_function_registry {
        data.functions(registry)
            .map(|map| map.iter().map(function_entry).collect())
    } else {
        data.registry(registry)
            .map(|map| map.iter().map(resource_entry).collect())
    }
}

/// One winning definition, uniform across resources and functions, so the
/// diff logic is written once rather than twice with the same shape.
struct SideEntry {
    pack: String,
    fingerprint: Value,
}

fn resource_entry(
    (name, resource): (&ResourceLocation, &Resource),
) -> (ResourceLocation, SideEntry) {
    (
        name.clone(),
        SideEntry {
            pack: resource.pack.clone(),
            fingerprint: resource.value.clone(),
        },
    )
}

fn function_entry(
    (name, function): (&ResourceLocation, &LoadedFunction),
) -> (ResourceLocation, SideEntry) {
    (
        name.clone(),
        SideEntry {
            pack: function.pack.clone(),
            // Two functions mean the same thing when their commands match;
            // the file path inside the pack is bookkeeping, not behaviour.
            fingerprint: Value::String(
                function
                    .file
                    .lines
                    .iter()
                    .map(|l| l.command.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        },
    )
}

fn diff_definitions(
    registry: &RegistryId,
    old: Option<&BTreeMap<ResourceLocation, SideEntry>>,
    new: Option<&BTreeMap<ResourceLocation, SideEntry>>,
    diff: &mut ReloadDiff,
) {
    let empty = BTreeMap::new();
    let old = old.unwrap_or(&empty);
    let new = new.unwrap_or(&empty);

    for (name, entry) in new {
        match old.get(name) {
            None => diff.added.push(Definition {
                registry: registry.clone(),
                name: name.clone(),
                pack: entry.pack.clone(),
            }),
            Some(previous) => {
                if previous.pack == entry.pack && previous.fingerprint == entry.fingerprint {
                    continue;
                }
                diff.replaced.push(Replacement {
                    registry: registry.clone(),
                    name: name.clone(),
                    from_pack: previous.pack.clone(),
                    to_pack: entry.pack.clone(),
                    content_changed: previous.fingerprint != entry.fingerprint,
                });
            }
        }
    }
    for (name, entry) in old {
        if !new.contains_key(name) {
            diff.removed.push(Definition {
                registry: registry.clone(),
                name: name.clone(),
                pack: entry.pack.clone(),
            });
        }
    }
}

fn diff_tags(old: &LoadedData, new: &LoadedData, registry: &RegistryId, out: &mut Vec<TagChange>) {
    let names: BTreeSet<&ResourceLocation> = old
        .merged_tags(registry)
        .into_iter()
        .flat_map(|tags| tags.keys())
        .chain(
            new.merged_tags(registry)
                .into_iter()
                .flat_map(|tags| tags.keys()),
        )
        .collect();
    for name in names {
        let before = old.merged_tag(registry, name);
        let after = new.merged_tag(registry, name);
        if before == after {
            continue;
        }
        out.push(TagChange {
            registry: registry.clone(),
            name: name.clone(),
            entries_before: before.map_or(0, |tag| tag.entries.len()),
            entries_after: after.map_or(0, |tag| tag.entries.len()),
        });
    }
}

fn union_registries(old: &LoadedData, new: &LoadedData) -> BTreeSet<RegistryId> {
    let mut keys: BTreeSet<RegistryId> = old.registries().cloned().collect();
    keys.extend(new.registries().cloned());
    keys.extend(old.tag_registries().cloned());
    keys.extend(new.tag_registries().cloned());
    keys.extend(old.function_registries().cloned());
    keys.extend(new.function_registries().cloned());
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemPack;
    use crate::Severity;

    fn pack_with(id: &str, recipe_body: &str) -> MemPack {
        MemPack::with_meta(
            id,
            &[
                ("data/minecraft/recipe/stick.json", recipe_body),
                (
                    "data/minecraft/tags/block/planks.json",
                    r#"{"values":["minecraft:oak_planks"]}"#,
                ),
            ],
        )
    }

    fn refs(packs: &[MemPack]) -> Vec<&dyn PackSource> {
        packs.iter().map(|p| p as &dyn PackSource).collect()
    }

    fn recipe_named(name: &str) -> ResourceLocation {
        ResourceLocation::parse(name).expect("valid")
    }

    fn find(diff: &ReloadDiff, registry: &str, name: &str) -> bool {
        let target = recipe_named(name);
        let hit = |d: &Definition| d.registry.as_str() == registry && d.name == target;
        let hit_rep = |d: &Replacement| d.registry.as_str() == registry && d.name == target;
        diff.added.iter().any(hit)
            || diff.removed.iter().any(hit)
            || diff.replaced.iter().any(hit_rep)
    }

    #[test]
    fn a_swap_reports_what_was_added_removed_and_replaced() {
        let first = pack_with("a", r#"{"result":"stick"}"#);
        let second_pack = MemPack::with_meta(
            "b",
            &[
                // Same name, same bytes, different pack: provenance moved.
                ("data/minecraft/recipe/stick.json", r#"{"result":"stick"}"#),
                // Genuinely new.
                ("data/minecraft/recipe/rod.json", r#"{"result":"rod"}"#),
                // A bigger planks tag than pack a's.
                (
                    "data/minecraft/tags/block/planks.json",
                    r#"{"values":["minecraft:oak_planks","minecraft:spruce_planks"]}"#,
                ),
            ],
        );
        let second = [second_pack];

        let handle = ReloadHandle::starting(crate::load(&refs(&[first]), &LoadOptions::default()));
        let report = handle
            .reload(
                &refs(&second),
                &LoadOptions::default(),
                ReloadPolicy::default(),
            )
            .expect("swaps");

        assert!(
            find(&report.diff, "recipe", "minecraft:rod"),
            "{:#?}",
            report.diff
        );
        assert!(find(&report.diff, "recipe", "minecraft:stick"));
        let stick = report
            .diff
            .replaced
            .iter()
            .find(|r| r.name == recipe_named("minecraft:stick"))
            .expect("stick changed hands");
        assert_eq!(stick.from_pack, "a");
        assert_eq!(stick.to_pack, "b");
        assert!(
            !stick.content_changed,
            "identical documents moved provenance only"
        );
        // The tag grew by one written entry.
        let tag = report
            .diff
            .tags_changed
            .iter()
            .find(|t| t.name == recipe_named("minecraft:planks"))
            .expect("tag changed");
        assert_eq!(tag.entries_before, 1);
        assert_eq!(tag.entries_after, 2);
    }

    #[test]
    fn removing_the_only_definer_removes_the_definition() {
        let first = MemPack::with_meta("a", &[("data/minecraft/recipe/x.json", "{}")]);
        let empty: [MemPack; 0] = [];
        let handle = ReloadHandle::starting(crate::load(&refs(&[first]), &LoadOptions::default()));
        let report = handle
            .reload(
                &refs(&empty),
                &LoadOptions::default(),
                ReloadPolicy::default(),
            )
            .expect("swaps to empty");
        assert!(
            find(&report.diff, "recipe", "minecraft:x"),
            "{:?}",
            report.diff
        );
        assert_eq!(handle.snapshot().stats().resources, 0);
    }

    #[test]
    fn reloading_identical_packs_changes_nothing() {
        let first = pack_with("a", r#"{"result":"stick"}"#);
        let again = pack_with("a", r#"{"result":"stick"}"#);
        let handle = ReloadHandle::starting(crate::load(&refs(&[first]), &LoadOptions::default()));
        let report = handle
            .reload(
                &refs(&[again]),
                &LoadOptions::default(),
                ReloadPolicy::default(),
            )
            .expect("swaps");
        assert!(report.diff.is_empty(), "{:?}", report.diff);
    }

    #[test]
    fn a_required_clean_reload_that_finds_errors_keeps_the_old_stack() {
        let good = pack_with("good", typed());
        let handle = ReloadHandle::starting(crate::load(&refs(&[good]), &LoadOptions::default()));

        let broken = MemPack::with_meta(
            "broken",
            &[("data/minecraft/recipe/stick.json", "{not json")],
        );
        let error = handle
            .reload(
                &refs(&[
                    MemPack::with_meta("ok", &[("data/minecraft/recipe/other.json", typed())]),
                    broken,
                ]),
                &LoadOptions::default(),
                ReloadPolicy::RequireClean,
            )
            .expect_err("refused");
        assert!(!error.findings.is_empty());

        // The old world is exactly what it was: stick still there, rod not.
        let snapshot = handle.snapshot();
        assert!(snapshot
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:stick"))
            .is_some());
        assert!(snapshot
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:other"))
            .is_none());
    }

    #[test]
    fn an_advancement_cycle_in_the_candidate_blocks_a_required_clean_reload() {
        let good = pack_with("good", typed());
        let handle = ReloadHandle::starting(crate::load(&refs(&[good]), &LoadOptions::default()));

        let looping = MemPack::with_meta(
            "loopy",
            &[
                (
                    "data/minecraft/advancement/a.json",
                    r#"{"parent":"minecraft:b","criteria":{}}"#,
                ),
                (
                    "data/minecraft/advancement/b.json",
                    r#"{"parent":"minecraft:a","criteria":{}}"#,
                ),
            ],
        );
        let error = handle
            .reload(
                &refs(&[looping]),
                &LoadOptions::default(),
                ReloadPolicy::RequireClean,
            )
            .expect_err("refused");
        assert_eq!(error.cycles.len(), 1, "{:?}", error.cycles);
        assert!(handle
            .snapshot()
            .registry(&RegistryId::new("advancement"))
            .is_none());
    }

    #[test]
    fn warnings_do_not_block_a_required_clean_reload() {
        let handle = ReloadHandle::empty();
        // A legacy directory name warns; it must not be fatal.
        let legacy =
            MemPack::with_meta("legacy", &[("data/minecraft/recipes/stick.json", typed())]);
        let report = handle
            .reload(
                &refs(&[legacy]),
                &LoadOptions::default(),
                ReloadPolicy::RequireClean,
            )
            .expect("warnings are not errors");
        assert_eq!(handle.snapshot().findings()[0].severity, Severity::Warning);
        let _ = report;
    }

    #[test]
    fn a_reader_holds_its_snapshot_while_the_world_is_swapped() {
        let first = MemPack::with_meta("a", &[("data/minecraft/recipe/x.json", r#"{"v":1}"#)]);
        let second = MemPack::with_meta("a", &[("data/minecraft/recipe/y.json", r#"{"v":2}"#)]);
        let handle = ReloadHandle::starting(crate::load(&refs(&[first]), &LoadOptions::default()));

        let held = handle.snapshot();
        assert!(held
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:x"))
            .is_some());

        handle.install(crate::load(&refs(&[second]), &LoadOptions::default()));

        // The held snapshot is the old world, untouched.
        assert!(held
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:x"))
            .is_some());
        assert!(held
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:y"))
            .is_none());
        // And the live one is the new world, wholly.
        let fresh = handle.snapshot();
        assert!(fresh
            .get(&RegistryId::new("recipe"), &recipe_named("minecraft:y"))
            .is_some());
    }

    fn typed() -> &'static str {
        r#"{"type":"minecraft:crafting_shaped"}"#
    }
}
