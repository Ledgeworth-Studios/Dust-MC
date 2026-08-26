//! Finding the packs in a directory — the `datapacks/` folder, or the
//! server's own library of everything it ships.
//!
//! # The order rule
//!
//! Vanilla reads a datapack folder in file-name order and stacks the packs on
//! top of each other, so the alphabetically-last pack wins a conflict. That is
//! reproduced here exactly: [`discover`] returns packs sorted by name, and the
//! loader's existing "later overrides earlier" does the rest. Two consequences
//! are worth writing down:
//!
//! * **The order is the whole contract.** A caller that reorders the returned
//!   list changes who wins every conflict; there is no priority metadata to
//!   fall back on, because the format has none.
//! * **Overlays inside one pack follow the same positional rule** (see
//!   [`crate::overlay`]): last matching entry per file. One rule everywhere.
//!   Nothing in this crate resolves by "first".
//!
//! # What counts as a pack
//!
//! The same things Minecraft counts: a **directory**, and a file ending in
//! `.zip`. Anything else — a `.jar`, a README, a `.tar.gz` renamed — is one
//! warning apiece, because an operator who dropped a file in expecting it to
//! load has to find out from the log rather than from the missing behaviour.
//!
//! Names starting with `.` are skipped silently. That is vanilla's own escape
//! hatch (disabled packs stashed as `.whatever.zip`), so it is a convention,
//! not a mistake — warning about it would be a line on everyone who ever used
//! the convention.
//!
//! # Duplicate names
//!
//! `foo/` and `foo.zip` both answer to id `foo`. [`discover`] refuses **both**
//! with one error rather than picking a winner, for two reasons: the operator
//! cannot see which of them was meant to win, and a silent pick would decide
//! the question by which of the two sorts later — a rule nobody could be
//! expected to know. [`load`] independently refuses duplicate ids among
//! whatever list it is handed, so the invariant holds no matter how the list
//! was built.

use std::path::Path;

use crate::finding::Finding;
use crate::pack::{open, PackSource};
use crate::{LoadOptions, LoadedData};

/// Read `directory` and return the packs in it, in load order: name ascending,
/// last overriding first.
///
/// Findings describe what was seen and refused; they are returned rather than
/// printed because deciding where they belong is the caller's job. An absent
/// or unreadable directory is a single error finding and an empty list — the
/// server still starts with nothing loaded.
pub fn discover(directory: impl AsRef<Path>) -> (Vec<Box<dyn PackSource>>, Vec<Finding>) {
    let directory = directory.as_ref();
    let mut findings = Vec::new();

    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(source) => {
            findings.push(Finding::error(
                "",
                directory.display().to_string(),
                format!(
                    "could not be read as a pack directory: {source}. Nothing \
                     has been discovered."
                ),
            ));
            return (Vec::new(), findings);
        }
    };

    let mut candidates: Vec<(String, bool)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                // One unreadable directory entry must not cost the rest of
                // the scan.
                findings.push(Finding::warning(
                    "",
                    directory.display().to_string(),
                    format!("could not be listed: {source}"),
                ));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            // The documented hiding convention. Silent, deliberately.
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            findings.push(Finding::warning(
                "",
                entry.path().display().to_string(),
                "could not be inspected, so it was not treated as a pack.",
            ));
            continue;
        };
        if kind.is_dir() {
            candidates.push((name, false));
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            candidates.push((name, true));
        } else if kind.is_file() {
            findings.push(Finding::warning(
                "",
                entry.path().display().to_string(),
                "is not a pack: a pack here is a directory or a .zip, so this \
                 file has been left alone.",
            ));
        }
    }
    // Byte order, not case-insensitive order: a stable total order is worth
    // more than a friendlier partial one that ties.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let mut packs: Vec<Box<dyn PackSource>> = Vec::new();
    let mut ids_in_use: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (name, is_zip) in candidates {
        let path = directory.join(&name);
        // A zip answers to its stem, which is why `foo/` and `foo.zip`
        // collide at all.
        let id = match path.file_stem() {
            Some(stem) if is_zip => stem.to_string_lossy().into_owned(),
            _ => name.clone(),
        };
        if let Some(previous) = ids_in_use.get(&id) {
            findings.push(Finding::error(
                &id,
                directory.display().to_string(),
                format!(
                    "holds two packs answering to `{id}`: `{previous}` and \
                     `{}`. Both have been skipped, because which of them should \
                     win a conflict is not something Dust can guess. Rename one.",
                    path.display()
                ),
            ));
            // Drop the pack already collected under this id as well, so the
            // returned list never contains the pair.
            packs.retain(|pack| pack.id() != id);
            continue;
        }
        ids_in_use.insert(id, path.display().to_string());
        match open(&path) {
            Ok(pack) => packs.push(pack),
            Err(error) => findings.push(Finding::error(
                "",
                path.display().to_string(),
                error.to_string(),
            )),
        }
    }

    (packs, findings)
}

/// Discover the packs under `directory` and load them, base-first, in one call.
///
/// The convenience the server's startup path will use. Findings from discovery
/// — the ones explaining what was *not* loaded — come back ahead of the ones
/// from loading, so the report reads top-down from cause to effect.
pub fn load_directory(directory: impl AsRef<Path>, options: &LoadOptions) -> LoadedData {
    let (packs, discovery_findings) = discover(directory);
    let refs: Vec<&dyn PackSource> = packs.iter().map(|pack| pack.as_ref()).collect();
    let mut data = crate::load(&refs, options);
    data.prepend_findings(discovery_findings);
    data
}
