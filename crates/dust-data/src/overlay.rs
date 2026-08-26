//! Overlays: a pack's own per-format replacements for its `data/` directory.
//!
//! # The vanilla behaviour, pinned down
//!
//! Since 1.20.2 a `pack.mcmeta` may carry an `overlays` section:
//!
//! ```json
//! "overlays": {
//!   "entries": [
//!     { "directory": "overlay_1_21", "formats": [45, 48] }
//!   ]
//! }
//! ```
//!
//! An entry whose format range contains the format the loader is running
//! becomes an extra layer **above** the pack's own `data/`. When two layers
//! hold the same file, the overlay's copy wins; when only the base holds it,
//! the base's copy is used. Nothing else changes: the merged view behaves, file
//! for file, as if the pack had been written that way.
//!
//! Three details are worth stating because each one has a wrong version
//! circulating:
//!
//! * **Order.** Entries are stacked in declaration order and the *last*
//!   matching entry wins per file, exactly the way packs themselves stack —
//!   later wins. There is no first-wins rule anywhere in this crate; both the
//!   overlay list inside one pack and the pack list in a directory resolve
//!   conflicts by position, last first. Two rules would be two things to
//!   remember where one will do, and vanilla's layered lookup ends up top-most
//!   queried first for overlays the same way it does across packs.
//! * **Non-matching entries are inert, and that is normal.** A pack shipping
//!   overlays for several game versions carries entries for all of them;
//!   under 1.21.1 most are expected not to match. Warning about them would put
//!   a warning on every correctly-built multi-version pack, and a warning that
//!   is always there teaches people to stop reading warnings.
//! * **The negotiation uses the same spellings as `supported_formats`** — a
//!   bare number, `[min, max]`, or the `min_inclusive`/`max_inclusive`
//!   object. [`Overlay::applies_to`] is the one place that decides.
//!
//! # How this is applied
//!
//! As a name mapping, and nothing more. [`OverlainPack`] wraps any
//! [`PackSource`] and answers `list`/`read` from the layered view; every rule
//! above it — registries, tags, findings — runs unchanged over the wrapped
//! source, because those rules live above the container and must never learn
//! that containers can have layers. That is the same shape argument
//! [`crate::pack`] makes for directories and zips loading identically.
//!
//! Files living under an entry that did **not** apply disappear from the view
//! entirely: they are alternatives for another game version, not junk next to
//! the data. Leaving them in would produce a warning apiece for directories
//! the loader cannot recognise, on packs that are built correctly.

use std::collections::BTreeMap;

use crate::meta::Overlay;
use crate::pack::{PackError, PackSource};

/// Why an overlay entry was refused outright, as opposed to merely not
/// matching the running format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No `directory` was written, so there is nothing to layer.
    NoDirectory,
    /// The directory name could not be a path inside the pack: empty,
    /// absolute, or containing `..`, a backslash or a NUL. A name like that
    /// cannot be reached through the listing, so treating it as inert would
    /// hide a malformed `pack.mcmeta`.
    UnsafeName,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoDirectory => "has no `directory`",
            Self::UnsafeName => "has a `directory` that is not a usable path inside the pack",
        }
    }
}

/// Which overlay entries apply, computed once from the pack's listing.
#[derive(Debug, Clone, Default)]
pub struct OverlayPlan {
    /// Virtual path → real path. Insertion order encodes priority: the base's
    /// files go in first, then each matching overlay earliest-first, so a
    /// later insert overwrites an earlier copy of the same virtual path.
    map: BTreeMap<String, String>,
    /// Directories that were layered, earliest first.
    pub applied: Vec<String>,
    /// Entries skipped because their formats do not include the running
    /// format. Normal for multi-version packs; reported so a diagnostic dump
    /// can say what a pack *carries* rather than only what it used.
    pub inert: Vec<String>,
    /// Entries that named no usable directory. These are findings above this
    /// layer; they are returned rather than acted on because deciding
    /// severities is the loader's job.
    pub refused: Vec<(String, Refusal)>,
}

impl OverlayPlan {
    /// Plan the layers for a pack whose files are `listing`, given the
    /// metadata's overlay entries and the format being loaded for.
    ///
    /// Overlay directories sit at the pack root, beside `data/`; an entry
    /// applies when its range contains `target_format`.
    pub fn build(listing: &[String], overlays: &[Overlay], target_format: u32) -> Self {
        let mut plan = Self::default();

        for overlay in overlays {
            if !overlay.formats.contains(target_format) {
                plan.inert.push(overlay.directory.clone());
                continue;
            }
            match usability(&overlay.directory) {
                Ok(()) => plan.applied.push(overlay.directory.clone()),
                Err(reason) => plan.refused.push((overlay.directory.clone(), reason)),
            }
        }

        // Base first: everything that is not underneath a declared overlay
        // directory. That includes the directories which did *not* apply —
        // their files belong to another game version, and leaving them visible
        // would turn every multi-version pack into a pile of mystery
        // directories.
        let declared: Vec<&str> = overlays
            .iter()
            .map(|overlay| overlay.directory.as_str())
            .collect();
        for path in listing {
            let under_declared = declared.iter().any(|directory| {
                !directory.is_empty()
                    && path.starts_with(directory)
                    && path[directory.len()..].starts_with('/')
            });
            if !under_declared {
                plan.map.insert(path.clone(), path.clone());
            }
        }

        // Then each applied overlay, earliest first — so a later one
        // overwrites an earlier copy of the same file, which is the order
        // rule the module documentation pins down.
        for directory in &plan.applied {
            let prefix = format!("{directory}/");
            for path in listing {
                let Some(rest) = path.strip_prefix(&prefix) else {
                    continue;
                };
                plan.map.insert(rest.to_owned(), path.clone());
            }
        }

        plan
    }

    /// Whether the layered view holds anything at all beyond the base — used
    /// to skip the wrapping entirely for packs without applicable overlays.
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }
}

/// `Ok` when the name can be used as a path relative to the pack root.
fn usability(directory: &str) -> Result<(), Refusal> {
    if directory.is_empty() {
        return Err(Refusal::NoDirectory);
    }
    if directory.starts_with('/')
        || directory.split('/').any(|segment| segment == "..")
        || directory.contains('\\')
        || directory.contains('\0')
    {
        return Err(Refusal::UnsafeName);
    }
    Ok(())
}

/// A pack seen through its applicable overlays.
///
/// Constructed with [`OverlayPlan::build`] plus the source it wraps; see the
/// module documentation for why this is a wrapper and not a load-time special
/// case.
#[derive(Debug)]
pub struct OverlainPack<'a> {
    inner: &'a dyn PackSource,
    plan: OverlayPlan,
}

impl<'a> OverlainPack<'a> {
    /// Layer `inner` according to `plan`. The plan is built from the caller's
    /// listing so a load walks the pack once, not twice.
    pub fn new(inner: &'a dyn PackSource, plan: OverlayPlan) -> Self {
        Self { inner, plan }
    }

    /// The plan this view was built from, for diagnostics.
    pub fn plan(&self) -> &OverlayPlan {
        &self.plan
    }
}

impl PackSource for OverlainPack<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn origin(&self) -> String {
        self.inner.origin()
    }

    fn assumed_format(&self) -> Option<u32> {
        self.inner.assumed_format()
    }

    fn list(&self) -> Result<Vec<String>, PackError> {
        Ok(self.plan.map.keys().cloned().collect())
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, PackError> {
        match self.plan.map.get(path) {
            None => Ok(None),
            Some(real) => self.inner.read(real),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemPack;

    fn overlay(directory: &str, min: u32, max: u32) -> Overlay {
        Overlay {
            directory: directory.to_owned(),
            formats: crate::meta::FormatRange { min, max },
        }
    }

    #[test]
    fn an_applicable_overlay_shadows_only_the_files_it_holds() {
        let listing = vec![
            "data/minecraft/recipe/base.json".to_owned(),
            "data/minecraft/recipe/shared.json".to_owned(),
            "new/data/minecraft/recipe/shared.json".to_owned(),
        ];
        let plan = OverlayPlan::build(&listing, &[overlay("new", 48, 48)], 48);
        assert_eq!(plan.applied, vec!["new".to_owned()]);
        assert_eq!(
            plan.map["data/minecraft/recipe/base.json"],
            "data/minecraft/recipe/base.json"
        );
        assert_eq!(
            plan.map["data/minecraft/recipe/shared.json"], "new/data/minecraft/recipe/shared.json",
            "the overlay's copy wins"
        );
        assert!(
            !plan
                .map
                .contains_key("new/data/minecraft/recipe/shared.json"),
            "the real path disappears behind its virtual name"
        );
    }

    #[test]
    fn the_last_matching_entry_wins_per_file() {
        // Both overlays carry `shared.json`; the second is declared later, so
        // it wins — the same positional rule as packs themselves.
        let listing = vec![
            "data/x.json".to_owned(),
            "first/data/x.json".to_owned(),
            "second/data/x.json".to_owned(),
        ];
        let plan = OverlayPlan::build(
            &listing,
            &[overlay("first", 48, 48), overlay("second", 48, 48)],
            48,
        );
        assert_eq!(plan.map["data/x.json"], "second/data/x.json");

        // And when only the earlier one matches, the earlier one is all there
        // is — matching is decided per entry, not per pack.
        let plan = OverlayPlan::build(
            &listing,
            &[overlay("first", 48, 48), overlay("second", 1, 2)],
            48,
        );
        assert_eq!(plan.map["data/x.json"], "first/data/x.json");
        assert_eq!(plan.inert, vec!["second".to_owned()]);
    }

    #[test]
    fn an_entry_for_another_format_is_inert_and_its_files_are_not_seen() {
        let listing = vec![
            "data/x.json".to_owned(),
            "old_layout/data/x.json".to_owned(),
        ];
        let plan = OverlayPlan::build(&listing, &[overlay("old_layout", 15, 20)], 48);
        assert!(plan.applied.is_empty());
        assert_eq!(plan.map.len(), 1, "the inert directory's files vanish");
        assert_eq!(plan.inert, vec!["old_layout".to_owned()]);
    }

    #[test]
    fn an_unusable_directory_name_is_refused_rather_than_silently_inert() {
        // An empty name has no prefix to hide files under, so only the named
        // cases get a file placed beneath them.
        for (name, expected) in [
            ("", Refusal::NoDirectory),
            ("../outside", Refusal::UnsafeName),
            ("/absolute", Refusal::UnsafeName),
            ("back\\slash", Refusal::UnsafeName),
        ] {
            let mut listing = vec!["data/x.json".to_owned()];
            if !name.is_empty() && !name.starts_with('/') {
                listing.push(format!("{name}/data/y.json"));
            }
            let plan = OverlayPlan::build(&listing, &[overlay(name, 48, 48)], 48);
            assert_eq!(plan.refused, vec![(name.to_owned(), expected)], "{name:?}");
            assert_eq!(plan.map.len(), 1, "{name:?}: its files are not read");
        }
    }

    #[test]
    fn a_name_that_merely_contains_dots_is_usable() {
        // Same rule as zip names: `..` climbs only as a whole segment.
        let listing = vec!["a..b/data/x.json".to_owned(), "data/x.json".to_owned()];
        let plan = OverlayPlan::build(&listing, &[overlay("a..b", 48, 48)], 48);
        assert!(plan.refused.is_empty());
        assert_eq!(plan.map["data/x.json"], "a..b/data/x.json");
    }

    #[test]
    fn the_wrapped_source_answers_from_the_layered_view() {
        let pack = MemPack::with_meta(
            "layered",
            &[
                ("data/minecraft/recipe/a.json", r#"{"from":"base"}"#),
                ("ov/data/minecraft/recipe/a.json", r#"{"from":"overlay"}"#),
                ("data/minecraft/recipe/b.json", r#"{"from":"base"}"#),
            ],
        );
        let listing = pack.list().unwrap();
        let overlays = [overlay("ov", 48, 48)];
        let plan = OverlayPlan::build(&listing, &overlays, 48);
        let wrapped = OverlainPack::new(&pack, plan);

        let listed = wrapped.list().unwrap();
        assert_eq!(listed.len(), 3, "pack.mcmeta, one a, one b");
        let a = wrapped
            .read("data/minecraft/recipe/a.json")
            .unwrap()
            .unwrap();
        assert_eq!(a, br#"{"from":"overlay"}"#.as_slice());
        assert!(wrapped
            .read("data/minecraft/recipe/b.json")
            .unwrap()
            .is_some());
        // Delegation: id, origin and assumed format come from the inner pack.
        assert_eq!(wrapped.id(), "layered");
        assert_eq!(wrapped.origin(), "<memory:layered>");
    }
}
