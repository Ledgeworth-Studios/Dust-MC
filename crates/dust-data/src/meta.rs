//! `pack.mcmeta` — the file that says what a pack is and what it was built for.
//!
//! # The shape, as 1.21.1 actually writes it
//!
//! ```json
//! {
//!   "pack": {
//!     "pack_format": 48,
//!     "description": { "translate": "dataPack.bundle.description" },
//!     "supported_formats": { "min_inclusive": 46, "max_inclusive": 48 }
//!   },
//!   "features": { "enabled": ["minecraft:bundle"] }
//! }
//! ```
//!
//! Two things about that are worth stating because a loader written from the
//! wiki gets them wrong. `description` is **not a string** — it is a text
//! component, and both of the packs Mojang ships inside the vanilla data tree
//! write it as `{"translate": …}`, which cannot be rendered without a language
//! file. And `supported_formats` (1.20.2 and later) has three legal spellings:
//! a bare number, a two-element list, and the object above. All three appear in
//! the wild; this reads all three.
//!
//! # Why an incompatible `pack_format` refuses the pack and does not stop
//! the server
//!
//! There were three options and each is wrong somewhere:
//!
//! * **Load it anyway** is what a permissive loader does and it is the worst of
//!   the three. `pack_format` is the pack author's own statement about which
//!   schema they wrote against, and the 1.21 directory rename is exactly the
//!   kind of change it tracks. Loading a format-15 pack under 1.21 rules gets
//!   an operator a server that started, a pack that appears installed, and no
//!   recipes — the silent-no-op this project rules out everywhere else.
//! * **Refuse to start** is honest and disproportionate. One stale cosmetic
//!   pack should not take a server down, and an operator who cannot start the
//!   server cannot read the log that explains why.
//! * **Refuse the pack, loudly, and start** is what this does. The finding is
//!   an [`Error`](crate::Severity::Error) because something the operator
//!   installed is not loaded; the *server* still starts, because the blast
//!   radius of a bad pack is that pack. [`crate::LoadOptions::unknown_format`]
//!   is the escape hatch for an operator who knows their format-15 pack is
//!   fine, and using it emits a warning so that the decision stays visible in
//!   the log rather than becoming the new normal.
//!
//! Minecraft itself takes a fourth position — it loads the pack and colours it
//! red in a screen. A server has no screen.
//!
//! # What this does not catch
//!
//! A matching `pack_format` is a claim, not a check. A pack that declares 48
//! and contains 1.16 loot tables passes here and fails later, in whatever crate
//! ends up reading a loot table. Nothing at this layer can tell the difference,
//! because this layer deliberately does not know what a loot table is.

use serde_json::{Map, Value};

use crate::finding::Finding;
use crate::json;

/// The datapack format Dust 1.21.1 reads.
pub const DUST_PACK_FORMAT: u32 = 48;

/// Keys `pack.mcmeta` may have at the root.
const ROOT_KEYS: &[&str] = &["pack", "features", "filter", "overlays", "language"];

/// Keys the `pack` object may have.
const PACK_KEYS: &[&str] = &["pack_format", "description", "supported_formats"];

/// An inclusive range of pack formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRange {
    pub min: u32,
    pub max: u32,
}

impl FormatRange {
    pub fn exactly(format: u32) -> Self {
        Self {
            min: format,
            max: format,
        }
    }

    pub fn contains(&self, format: u32) -> bool {
        self.min <= format && format <= self.max
    }
}

impl std::fmt::Display for FormatRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}

/// A pack's description, as written.
///
/// Kept as the raw JSON because it is a text component and Dust has no
/// component renderer at this layer. [`plain_text`](Self::plain_text) is a
/// best effort for a log line, not a renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct Description(Value);

impl Description {
    pub fn raw(&self) -> &Value {
        &self.0
    }

    /// Something short enough to put in a log line.
    ///
    /// **What this does not do**: it does not localise, it does not apply
    /// formatting, and it does not implement text components. A `translate`
    /// component comes back as its translation key in braces, which is the
    /// honest answer — `dataPack.bundle.description` is what the file says and
    /// Dust has no language file to turn it into English.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        Self::flatten(&self.0, &mut out);
        out
    }

    fn flatten(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => out.push_str(text),
            Value::Array(items) => {
                for item in items {
                    Self::flatten(item, out);
                }
            }
            Value::Object(object) => {
                if let Some(Value::String(text)) = object.get("text") {
                    out.push_str(text);
                } else if let Some(Value::String(key)) = object.get("translate") {
                    out.push('{');
                    out.push_str(key);
                    out.push('}');
                }
                if let Some(extra) = object.get("extra") {
                    Self::flatten(extra, out);
                }
            }
            _ => {}
        }
    }
}

/// An `overlays.entries` item — a directory that would replace `data/` for
/// some range of formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub directory: String,
    pub formats: FormatRange,
}

/// Everything `pack.mcmeta` says.
#[derive(Debug, Clone, PartialEq)]
pub struct PackMeta {
    /// The format the pack was written against.
    pub pack_format: u32,
    /// The formats the pack claims to work with. `pack_format` alone unless
    /// `supported_formats` says otherwise.
    pub supported: FormatRange,
    pub description: Option<Description>,
    /// `features.enabled` — the experimental feature flags this pack needs.
    pub features: Vec<String>,
    /// `overlays.entries`, parsed but **not applied**. See [`Self::parse`].
    pub overlays: Vec<Overlay>,
    /// Whether a `filter` section is present. Also not applied.
    pub has_filter: bool,
}

impl PackMeta {
    /// What a pack with no `pack.mcmeta` at all is treated as, for the vanilla
    /// base layer, whose metadata comes from the server jar rather than from a
    /// file on disk.
    pub fn assumed(pack_format: u32) -> Self {
        Self {
            pack_format,
            supported: FormatRange::exactly(pack_format),
            description: None,
            features: Vec::new(),
            overlays: Vec::new(),
            has_filter: false,
        }
    }

    /// Read a `pack.mcmeta`.
    ///
    /// `None` means the file could not be understood well enough to say what
    /// the pack is; the findings say why. Findings are also produced for things
    /// that parsed and are **not acted on** — `features`, `filter` and
    /// `overlays` — because a pack author who wrote a filter and got no
    /// filtering has to be told, and a loader that quietly ignores a section is
    /// the silent-no-op again.
    pub fn parse(bytes: &[u8], pack: &str, file: &str) -> (Option<Self>, Vec<Finding>) {
        let mut findings = Vec::new();
        let value = match json::parse(bytes, pack, file) {
            Ok(value) => value,
            Err(finding) => return (None, vec![finding]),
        };
        let Value::Object(root) = value else {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "is {}, but pack.mcmeta must be an object with a `pack` section.",
                    json::kind_of(&value)
                ),
            ));
            return (None, findings);
        };

        findings.extend(json::unknown_keys(&root, ROOT_KEYS, pack, file, ""));

        let Some(section) = json::optional_object(&root, "pack", pack, file, &mut findings) else {
            findings.push(Finding::error(
                pack,
                file,
                "has no `pack` section, so there is nothing saying what format \
                 this pack was written for. Add \
                 `\"pack\": {\"pack_format\": 48, \"description\": \"…\"}`.",
            ));
            return (None, findings);
        };
        findings.extend(json::unknown_keys(section, PACK_KEYS, pack, file, "pack"));

        let Some(pack_format) = format_number(section.get("pack_format")) else {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "has no usable `pack.pack_format`. It must be a whole number; \
                     Dust reads format {DUST_PACK_FORMAT}."
                ),
            ));
            return (None, findings);
        };

        let supported = supported_formats(section, pack_format, pack, file, &mut findings);

        let description = match section.get("description") {
            None | Some(Value::Null) => {
                findings.push(Finding::warning(
                    pack,
                    file,
                    "has no `pack.description`. Dust will start, but the pack \
                     will be listed with no name of its own.",
                ));
                None
            }
            Some(value) => Some(Description(value.clone())),
        };

        let features = feature_flags(&root, pack, file, &mut findings);
        let overlays = overlays(&root, pack, file, &mut findings);
        let has_filter = root.contains_key("filter");
        if has_filter {
            findings.push(Finding::warning(
                pack,
                file,
                "has a `filter` section. Dust does not apply pack filters yet, \
                 so resources this pack expected to hide from packs below it \
                 will still be present.",
            ));
        }

        (
            Some(Self {
                pack_format,
                supported,
                description,
                features,
                overlays,
                has_filter,
            }),
            findings,
        )
    }

    /// Whether this pack claims to work with the format Dust reads.
    pub fn is_compatible_with(&self, format: u32) -> bool {
        self.supported.contains(format)
    }
}

fn format_number(value: Option<&Value>) -> Option<u32> {
    value?.as_u64()?.try_into().ok()
}

fn supported_formats(
    section: &Map<String, Value>,
    pack_format: u32,
    pack: &str,
    file: &str,
    findings: &mut Vec<Finding>,
) -> FormatRange {
    let fallback = FormatRange::exactly(pack_format);
    let Some(value) = section.get("supported_formats") else {
        return fallback;
    };

    let range = match value {
        Value::Number(_) => format_number(Some(value)).map(FormatRange::exactly),
        Value::Array(items) if items.len() == 2 => {
            match (format_number(items.first()), format_number(items.get(1))) {
                (Some(min), Some(max)) => Some(FormatRange { min, max }),
                _ => None,
            }
        }
        Value::Object(object) => {
            findings.extend(json::unknown_keys(
                object,
                &["min_inclusive", "max_inclusive"],
                pack,
                file,
                "pack.supported_formats",
            ));
            match (
                format_number(object.get("min_inclusive")),
                format_number(object.get("max_inclusive")),
            ) {
                (Some(min), Some(max)) => Some(FormatRange { min, max }),
                _ => None,
            }
        }
        _ => None,
    };

    match range {
        Some(range) if range.min <= range.max => range,
        Some(range) => {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "has `pack.supported_formats` running backwards ({} down to {}), \
                     which no format satisfies. Treating it as {pack_format} alone.",
                    range.min, range.max
                ),
            ));
            fallback
        }
        None => {
            findings.push(Finding::error(
                pack,
                file,
                format!(
                    "has a `pack.supported_formats` Dust cannot read. It must be a \
                     number, a two-element list `[min, max]`, or \
                     `{{\"min_inclusive\": n, \"max_inclusive\": n}}`. Treating it \
                     as {pack_format} alone."
                ),
            ));
            fallback
        }
    }
}

fn feature_flags(
    root: &Map<String, Value>,
    pack: &str,
    file: &str,
    findings: &mut Vec<Finding>,
) -> Vec<String> {
    let Some(section) = json::optional_object(root, "features", pack, file, findings) else {
        return Vec::new();
    };
    findings.extend(json::unknown_keys(
        section,
        &["enabled"],
        pack,
        file,
        "features",
    ));
    let flags: Vec<String> = section
        .get("enabled")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if !flags.is_empty() {
        findings.push(Finding::warning(
            pack,
            file,
            format!(
                "needs the experimental feature flag(s) {}. Dust does not gate \
                 anything on feature flags yet, so this pack's data is loaded \
                 unconditionally.",
                flags.join(", ")
            ),
        ));
    }
    flags
}

fn overlays(
    root: &Map<String, Value>,
    pack: &str,
    file: &str,
    findings: &mut Vec<Finding>,
) -> Vec<Overlay> {
    let Some(section) = json::optional_object(root, "overlays", pack, file, findings) else {
        return Vec::new();
    };
    findings.extend(json::unknown_keys(
        section,
        &["entries"],
        pack,
        file,
        "overlays",
    ));
    let mut parsed = Vec::new();
    for entry in section
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(directory) = object.get("directory").and_then(Value::as_str) else {
            continue;
        };
        let formats = match object.get("formats") {
            Some(Value::Number(_)) => {
                format_number(object.get("formats")).map(FormatRange::exactly)
            }
            Some(Value::Array(items)) if items.len() == 2 => {
                match (format_number(items.first()), format_number(items.get(1))) {
                    (Some(min), Some(max)) => Some(FormatRange { min, max }),
                    _ => None,
                }
            }
            Some(Value::Object(inner)) => match (
                format_number(inner.get("min_inclusive")),
                format_number(inner.get("max_inclusive")),
            ) {
                (Some(min), Some(max)) => Some(FormatRange { min, max }),
                _ => None,
            },
            _ => None,
        };
        parsed.push(Overlay {
            directory: directory.to_owned(),
            formats: formats.unwrap_or(FormatRange { min: 0, max: 0 }),
        });
    }
    if !parsed.is_empty() {
        findings.push(Finding::warning(
            pack,
            file,
            format!(
                "declares {} overlay director{}. Dust does not apply overlays yet, \
                 so only the pack's own `data/` directory is read and {} ignored.",
                parsed.len(),
                if parsed.len() == 1 { "y" } else { "ies" },
                parsed
                    .iter()
                    .map(|o| o.directory.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Option<PackMeta>, Vec<Finding>) {
        PackMeta::parse(text.as_bytes(), "p", "pack.mcmeta")
    }

    #[test]
    fn the_minimum_usable_file_parses() {
        let (meta, findings) = parse(r#"{"pack":{"pack_format":48,"description":"hi"}}"#);
        let meta = meta.expect("parses");
        assert_eq!(meta.pack_format, 48);
        assert_eq!(meta.supported, FormatRange::exactly(48));
        assert_eq!(meta.description.unwrap().plain_text(), "hi");
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_translated_description_comes_back_as_its_key_and_not_as_nothing() {
        // Both packs Mojang ships inside the vanilla data tree write it this
        // way. A loader that expects a string prints an empty pack name.
        let (meta, _) = parse(
            r#"{"pack":{"pack_format":48,
               "description":{"translate":"dataPack.bundle.description"}}}"#,
        );
        assert_eq!(
            meta.unwrap().description.unwrap().plain_text(),
            "{dataPack.bundle.description}"
        );
    }

    #[test]
    fn a_component_with_extra_parts_is_joined() {
        let (meta, _) = parse(
            r#"{"pack":{"pack_format":48,
               "description":{"text":"Pack ","extra":[{"text":"one"}]}}}"#,
        );
        assert_eq!(meta.unwrap().description.unwrap().plain_text(), "Pack one");
    }

    #[test]
    fn all_three_spellings_of_supported_formats_are_read() {
        for (written, expected) in [
            ("17", FormatRange { min: 17, max: 17 }),
            ("[46, 48]", FormatRange { min: 46, max: 48 }),
            (
                r#"{"min_inclusive":46,"max_inclusive":48}"#,
                FormatRange { min: 46, max: 48 },
            ),
        ] {
            let (meta, findings) = parse(&format!(
                r#"{{"pack":{{"pack_format":48,"description":"d","supported_formats":{written}}}}}"#
            ));
            assert_eq!(meta.expect(written).supported, expected, "{written}");
            assert!(findings.is_empty(), "{written}: {findings:?}");
        }
    }

    #[test]
    fn a_supported_formats_dust_cannot_read_is_reported_and_not_guessed_at() {
        let (meta, findings) =
            parse(r#"{"pack":{"pack_format":48,"description":"d","supported_formats":"48"}}"#);
        assert_eq!(
            meta.expect("still usable").supported,
            FormatRange::exactly(48)
        );
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_backwards_range_is_reported_rather_than_silently_matching_nothing() {
        let (meta, findings) =
            parse(r#"{"pack":{"pack_format":48,"description":"d","supported_formats":[48,46]}}"#);
        assert!(meta.is_some());
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_missing_pack_section_is_an_error_that_says_what_to_write() {
        let (meta, findings) = parse("{}");
        assert!(meta.is_none());
        assert!(
            findings[0].message.contains("pack_format"),
            "{:?}",
            findings
        );
    }

    #[test]
    fn a_missing_pack_format_is_an_error() {
        let (meta, findings) = parse(r#"{"pack":{"description":"d"}}"#);
        assert!(meta.is_none());
        assert_eq!(crate::finding::error_count(&findings), 1, "{findings:?}");
    }

    #[test]
    fn a_missing_description_is_a_warning_and_the_pack_still_loads() {
        let (meta, findings) = parse(r#"{"pack":{"pack_format":48}}"#);
        assert!(meta.is_some());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, crate::Severity::Warning);
    }

    #[test]
    fn a_section_dust_does_not_apply_says_so_rather_than_going_quiet() {
        for (written, expected) in [
            (r#""features":{"enabled":["minecraft:bundle"]}"#, "feature"),
            (r#""filter":{"block":[]}"#, "filter"),
            (
                r#""overlays":{"entries":[{"formats":[46,47],"directory":"old"}]}"#,
                "overlay",
            ),
        ] {
            let (meta, findings) = parse(&format!(
                r#"{{"pack":{{"pack_format":48,"description":"d"}},{written}}}"#
            ));
            assert!(meta.is_some(), "{written}");
            assert!(
                findings.iter().any(|f| f.message.contains(expected)),
                "{written}: {findings:?}"
            );
        }
    }

    #[test]
    fn an_unknown_root_key_is_a_warning_with_a_suggestion() {
        let (_, findings) = parse(r#"{"pack":{"pack_format":48,"description":"d"},"packs":{}}"#);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("Did you mean `pack`?")),
            "{findings:?}"
        );
    }

    #[test]
    fn compatibility_uses_the_declared_range_and_not_just_the_format() {
        let (meta, _) =
            parse(r#"{"pack":{"pack_format":46,"description":"d","supported_formats":[46,48]}}"#);
        let meta = meta.unwrap();
        assert_ne!(meta.pack_format, DUST_PACK_FORMAT);
        assert!(meta.is_compatible_with(DUST_PACK_FORMAT));
    }

    #[test]
    fn a_pack_from_a_different_era_is_not_compatible() {
        let (meta, _) = parse(r#"{"pack":{"pack_format":15,"description":"d"}}"#);
        assert!(!meta.unwrap().is_compatible_with(DUST_PACK_FORMAT));
    }
}
