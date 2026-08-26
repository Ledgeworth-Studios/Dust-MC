//! Advancement graphs: parent chains resolved across the whole merged stack.
//!
//! # Why parents are a graph question, and why the graph spans packs
//!
//! An advancement hangs off its parent by name and nothing else. A pack that
//! adds one advancement to vanilla's `story` tree depends on vanilla's
//! `story/root` without saying so anywhere except in that one string — which
//! is exactly why the resolution has to run over the **merged** stack rather
//! than pack by pack: the parent usually lives in another pack, often in the
//! base layer, and checking within the defining pack would report every
//! correctly-built extension of vanilla's tree as an orphan.
//!
//! The graph is also where "one definition per name" pays off. Because every
//! resource has exactly one winner, every advancement has exactly one parent,
//! and the graph is a function rather than a negotiation.
//!
//! # What is checked, and how hard
//!
//! * **A missing parent is an error.** Vanilla refuses to load an advancement
//!   whose parent does not resolve, so the tree it was written for never
//!   appears; that is "the resource did not load", which is what
//!   [`Severity::Error`](crate::Severity::Error) means here.
//! * **A parent cycle is an error naming its members**, child first and last,
//!   the same shape as [`crate::tag::TagCycle`]. Two advancements cannot be
//!   each other's ancestors; vanilla detects this too, but only after failing
//!   both, and the message deserves to say who was involved.
//! * **The display spine is checked when the file claims to be shown.**
//!   Vanilla's own reader requires `display.icon.item` and `display.title`
//!   for any advancement with a `display` section; a file missing either
//!   loads *here* but is refused by the game, and a warning says exactly
//!   that rather than letting a silent difference decide. An advancement
//!   without `display` at all is not checked — hidden trigger advancements
//!   are ordinary and correct — and neither is `display.description`, which
//!   is prose and can never be structurally wrong.
//! * **Rewards are inventoried, not judged.** Everything under `rewards`
//!   travels on [`AdvancementSkeleton`]; the one field this module looks
//!   *through* is `rewards.function`, because functions are loaded now and a
//!   reward pointing at no function any pack defines will do nothing at the
//!   moment it matters most — when the player unlocks something.
//!
//! # What this deliberately does not check
//!
//! Whether criteria triggers exist, whether granted recipe or loot names
//! resolve, whether the icon item is a real item. Those are registry
//! questions — a [`crate::Vocabulary`] question at best — and this module has
//! the same answer the tag resolver has: the vocabulary comes from outside,
//! and "no problems" must not be able to mean "no check ran".
//!
//! Like everything else here, nothing fails early: one run reports every
//! broken parent, every cycle, every spine problem, once each.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::finding::{suggestion, Finding};
use crate::location::ResourceLocation;
use crate::registry::RegistryId;
use crate::shape::AdvancementSkeleton;
use crate::{LoadedData, Resource};

/// The loop of parent references that closes back onto itself, child-first
/// and last so it reads as a round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementCycle {
    pub members: Vec<ResourceLocation>,
}

impl std::fmt::Display for AdvancementCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self.members.iter().map(|m| m.to_string()).collect();
        write!(f, "{}", names.join(" → "))
    }
}

/// What walking every advancement's parent chain produced.
#[derive(Debug, Default)]
pub struct AdvancementReport {
    /// Every advancement with no parent — the visible tops of trees, plus
    /// whatever hidden roots a pack invented.
    pub roots: BTreeSet<ResourceLocation>,
    pub cycles: Vec<AdvancementCycle>,
    /// Longest chain of ancestors any advancement sits at the bottom of,
    /// counting the advancement itself, and the advancement that achieves it.
    pub deepest_chain: usize,
    pub deepest_end: Option<ResourceLocation>,
}

/// Resolve every advancement's parent chain over the merged stack.
///
/// Returns the graph summary and the findings about files whose chains or
/// display spines are broken. Findings carry the winning file's provenance,
/// like every other layer's do.
pub fn validate(data: &LoadedData) -> (AdvancementReport, Vec<Finding>) {
    let mut report = AdvancementReport::default();
    let mut findings = Vec::new();

    let key = RegistryId::new("advancement");
    let Some(registry) = data.registry(&key) else {
        return (report, findings);
    };

    // Pass one: spines and per-file facts. Nothing here needs the graph.
    let mut parents: BTreeMap<&ResourceLocation, Option<ResourceLocation>> = BTreeMap::new();
    for (name, resource) in registry {
        let skeleton = AdvancementSkeleton::from_raw(&resource.value);
        check_parent_exists(name, &skeleton, registry, resource, &mut findings);
        check_display_spine(&skeleton, resource, &mut findings);
        check_reward_function(data, &skeleton, resource, &mut findings);
        parents.insert(name, skeleton.parent);
    }

    // Pass two: walk the single-parent edges for cycles and depth. Every node
    // has at most one outgoing edge, so a chain is a straight line until it
    // stops — and the one interesting thing a straight line can do is revisit
    // itself, which is the cycle.
    let mut settled: BTreeMap<ResourceLocation, usize> = BTreeMap::new();
    for name in registry.keys() {
        if settled.contains_key(name) {
            continue;
        }
        let mut path: Vec<ResourceLocation> = Vec::new();
        let mut seen: BTreeSet<ResourceLocation> = BTreeSet::new();
        let mut cursor = Some(name.clone());
        let mut closed_cycle: Option<AdvancementCycle> = None;

        while let Some(current) = cursor {
            if let Some(depth) = settled.get(&current).copied() {
                // Ran into an already-measured ancestor: its whole chain
                // length is known, so ours is just distance up to it.
                let steps = path.len();
                for (offset, member) in path.iter().enumerate() {
                    settled.insert(member.clone(), depth + steps - offset);
                }
                path.clear();
                break;
            }
            if seen.contains(&current) {
                let start = path
                    .iter()
                    .position(|member| *member == current)
                    .unwrap_or(0);
                let mut members = path[start..].to_vec();
                members.push(current.clone());
                closed_cycle = Some(AdvancementCycle { members });
                break;
            }
            seen.insert(current.clone());
            path.push(current.clone());
            // Step up only onto a parent that itself exists in the stack. A
            // missing parent stops the chain here: the walk must not invent
            // phantom nodes for names nothing defined, or they would turn up
            // as depths — and worse, as roots — of their own.
            cursor = match parents.get(&current).and_then(Clone::clone) {
                Some(parent) if parents.contains_key(&parent) => Some(parent),
                _ => None,
            };
        }

        if let Some(cycle) = closed_cycle {
            // Attach the finding to the file that closes the loop: the last
            // member before it repeats.
            let closer = cycle.members[cycle.members.len() - 2].clone();
            if let Some(resource) = registry.get(&closer) {
                findings.push(
                    Finding::error(
                        &resource.pack,
                        &resource.file,
                        format!(
                            "is part of a loop of parent references: {}. An \
                             advancement cannot be its own ancestor, however \
                             many steps it takes.",
                            cycle
                        ),
                    )
                    .about(closer),
                );
            } else {
                findings.push(Finding::error(
                    "",
                    "",
                    format!("an advancement loop could not be attributed to a file: {cycle}"),
                ));
            }
            report.cycles.push(cycle);
            // Depths inside — or leading into — a loop are meaningless; settle
            // the walked prefix at zero so later walks stop here rather than
            // re-entering, without inventing a length for an unresolvable
            // chain.
            for member in path {
                settled.insert(member, 0);
            }
        } else {
            // The walk stopped at a root or a missing parent: the path holds
            // every unmeasured node from `name` upward, root last.
            let top_root = path
                .last()
                .filter(|top| parents.get(top).and_then(Clone::clone).is_none());
            if let Some(top) = top_root {
                report.roots.insert(top.clone());
            }
            let depth_of_last = 1usize;
            for (offset, member) in path.iter().enumerate() {
                settled.insert(member.clone(), depth_of_last + path.len() - 1 - offset);
            }
        }
    }

    for (name, depth) in &settled {
        if *depth > report.deepest_chain {
            report.deepest_chain = *depth;
            report.deepest_end = Some(name.clone());
        }
    }

    (report, findings)
}

/// One advancement's parent names something no pack defines.
fn check_parent_exists(
    name: &ResourceLocation,
    skeleton: &AdvancementSkeleton,
    registry: &BTreeMap<ResourceLocation, Resource>,
    resource: &Resource,
    findings: &mut Vec<Finding>,
) {
    let Some(parent) = &skeleton.parent else {
        return;
    };
    if registry.contains_key(parent) {
        return;
    }
    findings.push(
        Finding::error(
            &resource.pack,
            &resource.file,
            format!(
                "names the parent `{parent}`, which no loaded pack defines, so \
                 this advancement has nowhere to hang. Expected it at \
                 `data/{}/{}/{}.json`.{}",
                parent.namespace(),
                RegistryId::new("advancement"),
                parent.path(),
                suggestion(
                    parent.as_str(),
                    registry.keys().map(ResourceLocation::as_str)
                ),
            ),
        )
        .about(name.clone()),
    );
}

/// `display.icon.item` and `display.title` are required by the game's own
/// reader for anything with a `display` section. Dust loads such a file;
/// Minecraft would not, and the warning exists so the divergence is said
/// rather than discovered.
fn check_display_spine(
    skeleton: &AdvancementSkeleton,
    resource: &Resource,
    findings: &mut Vec<Finding>,
) {
    let raw = &resource.value;
    let Some(display) = raw.get("display") else {
        return;
    };
    if !display.is_object() {
        findings.push(Finding::warning(
            &resource.pack,
            &resource.file,
            format!(
                "has `display` as {}, but it must be an object. The game's own \
                 reader would refuse this advancement.",
                crate::json::kind_of(display)
            ),
        ));
        return;
    }

    match display.get("icon") {
        None => findings.push(Finding::warning(
            &resource.pack,
            &resource.file,
            "has a `display` section with no `icon`. The game's own reader \
             requires one for a displayed advancement and refuses the file \
             without it.",
        )),
        Some(icon) => {
            let item = icon.get("item").and_then(Value::as_str);
            match item {
                None => findings.push(Finding::warning(
                    &resource.pack,
                    &resource.file,
                    "has a `display.icon` with no string `item` in it. The \
                     game's own reader requires the icon to name an item and \
                     refuses the file otherwise.",
                )),
                Some(text) if skeleton.icon.is_none() => findings.push(Finding::warning(
                    &resource.pack,
                    &resource.file,
                    format!(
                        "has `display.icon.item` as `{text}`, which is not a \
                         usable item id, so the game would show no icon."
                    ),
                )),
                Some(_) => {}
            }
        }
    }

    if display.get("title").is_none() {
        findings.push(Finding::warning(
            &resource.pack,
            &resource.file,
            "has a `display` section with no `title`. The game's own reader \
             requires one for a displayed advancement and refuses the file \
             without it.",
        ));
    }
}

/// `rewards.function` is the one rewards field that points into data this
/// loader holds, so it is the one whose absence can be stated as fact.
fn check_reward_function(
    data: &LoadedData,
    skeleton: &AdvancementSkeleton,
    resource: &Resource,
    findings: &mut Vec<Finding>,
) {
    let Some(function) = &skeleton.reward_function else {
        return;
    };
    let functions = data.functions(&RegistryId::new("function"));
    if functions.is_some_and(|map| map.contains_key(function)) {
        return;
    }
    findings.push(Finding::warning(
        &resource.pack,
        &resource.file,
        format!(
            "grants a call to the function `{function}`, which no loaded pack \
             defines. Unlocking this advancement will pay its other rewards \
             and quietly skip this one."
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::error_count;
    use crate::testing::MemPack;
    use crate::{load, LoadOptions, PackSource};

    fn load_advancements(packs: &[MemPack]) -> LoadedData {
        let refs: Vec<&dyn PackSource> = packs.iter().map(|p| p as &dyn PackSource).collect();
        load(&refs, &LoadOptions::default())
    }

    #[test]
    fn a_chain_across_packs_resolves_and_reports_its_depth() {
        let base = MemPack::with_meta(
            "base",
            &[(
                "data/minecraft/advancement/story/root.json",
                r#"{"display":{"icon":{"item":"minecraft:grass_block"},"title":"Root"}}"#,
            )],
        );
        let mid = MemPack::with_meta(
            "mid",
            &[(
                "data/somemod/advancement/mid.json",
                r#"{"parent":"minecraft:story/root","criteria":{}}"#,
            )],
        );
        let top = MemPack::with_meta(
            "top",
            &[(
                "data/somemod/advancement/top.json",
                r#"{"parent":"somemod:mid","criteria":{}}"#,
            )],
        );
        let data = load_advancements(&[base, mid, top]);
        let (report, findings) = validate(&data);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(report.roots.len(), 1);
        assert!(report
            .roots
            .contains(&ResourceLocation::parse("minecraft:story/root").unwrap()));
        assert_eq!(report.deepest_chain, 3);
        assert_eq!(
            report.deepest_end.map(|e| e.to_string()),
            Some("somemod:top".to_owned())
        );
    }

    #[test]
    fn a_missing_parent_is_an_error_that_suggests_the_closest_name() {
        let pack = MemPack::with_meta(
            "orphaned",
            &[(
                "data/minecraft/advancement/lost.json",
                r#"{"parent":"minecraft:story/rot","criteria":{}}"#,
            )],
        );
        let sibling = MemPack::with_meta(
            "anchor",
            &[(
                "data/minecraft/advancement/story/root.json",
                r#"{"criteria":{}}"#,
            )],
        );
        let data = load_advancements(&[pack, sibling]);
        let (report, findings) = validate(&data);
        assert_eq!(error_count(&findings), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("minecraft:story/rot"),
            "{}",
            findings[0]
        );
        assert!(
            findings[0]
                .message
                .contains("Did you mean `minecraft:story/root`?"),
            "{}",
            findings[0]
        );
        // The broken chain still counts itself — the resolvable part of it is
        // one advancement long.
        assert_eq!(report.deepest_chain, 1);
    }

    #[test]
    fn two_advancements_cannot_be_each_others_ancestors() {
        let pack = MemPack::with_meta(
            "loop",
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
        let data = load_advancements(&[pack]);
        let (report, findings) = validate(&data);
        assert_eq!(report.cycles.len(), 1, "{:?}", report.cycles);
        let printed = report.cycles[0].to_string();
        assert!(printed.contains("minecraft:a"), "{printed}");
        assert!(printed.contains("minecraft:b"), "{printed}");
        assert_eq!(error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn an_advancement_that_is_its_own_parent_is_a_one_step_loop() {
        let pack = MemPack::with_meta(
            "ouroboros",
            &[(
                "data/minecraft/advancement/self.json",
                r#"{"parent":"minecraft:self","criteria":{}}"#,
            )],
        );
        let data = load_advancements(&[pack]);
        let (report, findings) = validate(&data);
        assert_eq!(report.cycles.len(), 1);
        assert_eq!(report.cycles[0].members.len(), 2, "first and last repeat");
        assert_eq!(error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_diamond_of_children_is_not_a_cycle() {
        // Three children share one parent. Only one parent edge each means
        // the graph cannot actually branch upward, but children branching
        // downward must not confuse the walk into seeing a loop.
        let pack = MemPack::with_meta(
            "triple",
            &[
                ("data/minecraft/advancement/root.json", r#"{"criteria":{}}"#),
                (
                    "data/minecraft/advancement/a.json",
                    r#"{"parent":"minecraft:root","criteria":{}}"#,
                ),
                (
                    "data/minecraft/advancement/b.json",
                    r#"{"parent":"minecraft:root","criteria":{}}"#,
                ),
                (
                    "data/minecraft/advancement/c.json",
                    r#"{"parent":"minecraft:root","criteria":{}}"#,
                ),
            ],
        );
        let data = load_advancements(&[pack]);
        let (report, findings) = validate(&data);
        assert!(report.cycles.is_empty(), "{:?}", report.cycles);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(report.deepest_chain, 2);
        assert_eq!(report.roots.len(), 1);
    }

    #[test]
    fn a_display_without_an_icon_or_title_warns_about_the_games_reader() {
        let pack = MemPack::with_meta(
            "spineless",
            &[(
                "data/minecraft/advancement/shown.json",
                r#"{"display":{"description":"look"},"criteria":{}}"#,
            )],
        );
        let data = load_advancements(&[pack]);
        let (_, findings) = validate(&data);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings
                .iter()
                .all(|f| f.message.contains("game's own reader")),
            "{findings:?}"
        );
    }

    #[test]
    fn an_unusable_icon_item_is_named_rather_than_swallowed() {
        let pack = MemPack::with_meta(
            "bad_icon",
            &[(
                "data/minecraft/advancement/shown.json",
                r#"{"display":{"icon":{"item":"Not An Item"},"title":"t"},"criteria":{}}"#,
            )],
        );
        let data = load_advancements(&[pack]);
        let (_, findings) = validate(&data);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("`Not An Item`"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn a_hidden_advancement_with_no_display_draws_no_spine_warning() {
        let pack = MemPack::with_meta(
            "hidden_trigger",
            &[(
                "data/minecraft/advancement/invisible.json",
                r#"{"criteria":{"t":{"trigger":"minecraft:tick"}},"rewards":{"experience":10}}"#,
            )],
        );
        let data = load_advancements(&[pack]);
        let (report, findings) = validate(&data);
        assert!(findings.is_empty(), "{findings:?}");
        assert!(report.roots.len() == 1);
    }

    #[test]
    fn the_whole_rewards_inventory_travels_on_the_skeleton() {
        let pack = MemPack::with_meta(
            "generous",
            &[(
                "data/minecraft/advancement/paid.json",
                r#"{
                    "criteria": {},
                    "rewards": {
                        "recipes": ["somemod:r"],
                        "loot": ["somemod:l"],
                        "experience": 100,
                        "function": "somemod:on_unlock"
                    }
                }"#,
            )],
        );
        let data = load_advancements(&[pack]);
        let skeleton = AdvancementSkeleton::from_raw(
            &data
                .registry(&RegistryId::new("advancement"))
                .expect("registry")
                .get(&ResourceLocation::parse("minecraft:paid").unwrap())
                .expect("present")
                .value,
        );
        assert_eq!(skeleton.granted_recipes.len(), 1);
        assert_eq!(skeleton.granted_loot.len(), 1);
        assert_eq!(skeleton.granted_experience, Some(100));
        assert_eq!(
            skeleton.reward_function.map(|f| f.to_string()),
            Some("somemod:on_unlock".to_owned())
        );

        // No pack defines somemod:on_unlock, so the validator says the reward
        // will not fire — while the recipes and loot stay inventoried only.
        let (_, findings) = validate(&data);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("quietly skip"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn a_reward_function_that_exists_silences_the_check() {
        let pack = MemPack::with_meta(
            "wired_up",
            &[
                (
                    "data/minecraft/advancement/paid.json",
                    r#"{"criteria":{},"rewards":{"function":"somemod:on_unlock"}}"#,
                ),
                ("data/somemod/function/on_unlock.mcfunction", "say done\n"),
            ],
        );
        let data = load_advancements(&[pack]);
        let (_, findings) = validate(&data);
        assert!(findings.is_empty(), "{findings:?}");
    }
}
