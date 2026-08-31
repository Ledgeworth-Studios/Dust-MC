//! Where the contents come from, which is not this repository.
//!
//! # The rule
//!
//! A registry entry's *contents* are Mojang's, and nothing in this repository
//! carries them. The schema beside this file describes their shape, which is a
//! fact about a protocol; the values arrive at run time from a directory the
//! operator points `[data] path` at, in the ordinary `data/<namespace>/…`
//! layout that vanilla, every datapack and `xtask extract` all use. Decision
//! record 0007 is the long form, and the same rule already governs the ore
//! baseline (record 0006).
//!
//! With no such directory, Dust behaves exactly as it did before this existed:
//! it sends names, and a client that acknowledges no data packs is told, in
//! terms, that it cannot be served and why.
//!
//! # Every failure is collected, not raced to
//!
//! Sixty-four biome files with four mistakes between them should produce four
//! messages. Stopping at the first turns one afternoon's work into four, so
//! [`load`] reads every file, converts every document, and returns the whole
//! list of what went wrong. It refuses the directory if anything did — a
//! registry short by one entry shifts nothing here (ids come from the names
//! table, not from this) but it does give a client a biome that resolves to
//! nothing.

use std::fmt;
use std::path::{Path, PathBuf};

use dust_nbt::Compound;

use super::convert;
use super::schema::{Registry, SERVED};

/// One registry's entries, keyed by the name the sync packet uses.
#[derive(Debug, Default, Clone)]
pub struct Contents {
    entries: Vec<(String, Compound)>,
}

impl Contents {
    /// The compound for `entry`, or `None` if this registry has no such entry.
    pub fn get(&self, entry: &str) -> Option<&Compound> {
        self.entries
            .iter()
            .find(|(name, _)| name == entry)
            .map(|(_, compound)| compound)
    }

    /// How many entries were read.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was read.
    ///
    /// Nothing calls it. It is here because `len` is, and a type with a length
    /// and no emptiness test is a lint — and the honest way to satisfy that
    /// lint is the method, not an `allow`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Every registry Dust can serve with contents, loaded.
#[derive(Debug, Default, Clone)]
pub struct Loaded {
    registries: Vec<(&'static str, Contents)>,
}

impl Loaded {
    /// The contents of `registry`, or `None` if it was not loaded.
    pub fn get(&self, registry: &str) -> Option<&Contents> {
        self.registries
            .iter()
            .find(|(name, _)| *name == registry)
            .map(|(_, contents)| contents)
    }

    /// Whether nothing at all was loaded — which is the state a server with no
    /// `[data] path` is in, and the state in which a client that acknowledges
    /// no packs cannot be served.
    pub fn is_empty(&self) -> bool {
        self.registries.is_empty()
    }

    /// The registries loaded, with their entry counts, for the boot log.
    pub fn summary(&self) -> impl Iterator<Item = (&'static str, usize)> + '_ {
        self.registries
            .iter()
            .map(|(name, contents)| (*name, contents.len()))
    }
}

/// Why a data directory could not be used.
#[derive(Debug)]
pub enum LoadError {
    /// The path is not a directory, or is not readable.
    NotADirectory {
        /// The path as configured.
        path: PathBuf,
        /// What the file system said.
        detail: String,
    },
    /// A registry directory the schema needs is absent. Named rather than
    /// skipped: a data path that produced half the registries would leave the
    /// server refusing exactly the clients it was configured to admit, with no
    /// hint as to which half was missing.
    MissingRegistry {
        /// The registry that could not be found.
        registry: &'static str,
        /// Where it was looked for.
        path: PathBuf,
    },
    /// Files were read and some were wrong. Every one of them, not the first.
    Entries(Vec<EntryError>),
}

/// One file that could not be turned into a registry entry.
#[derive(Debug)]
pub struct EntryError {
    /// The file.
    pub path: PathBuf,
    /// What was wrong with it.
    pub detail: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory { path, detail } => {
                write!(
                    f,
                    "{} is not a readable directory: {detail}",
                    path.display()
                )
            }
            Self::MissingRegistry { registry, path } => write!(
                f,
                "{registry} needs the directory {}, which is not there",
                path.display()
            ),
            Self::Entries(errors) => {
                write!(f, "{} registry entries could not be read:", errors.len())?;
                for error in errors {
                    write!(f, "\n  {}: {}", error.path.display(), error.detail)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Read every served registry out of a data directory.
///
/// `root` is the directory that holds namespaces — the one containing
/// `minecraft/`, which is `data/` in a datapack and in `xtask extract`'s
/// output. Only the `minecraft` namespace is read, because these eleven
/// registries are Minecraft's own and an entry in another namespace would need
/// a place in the name table that nothing puts it in yet.
pub fn load(root: impl AsRef<Path>) -> Result<Loaded, LoadError> {
    let root = root.as_ref();
    let namespace = root.join("minecraft");
    if !namespace.is_dir() {
        return Err(LoadError::NotADirectory {
            path: namespace.clone(),
            detail: "no minecraft namespace under it".to_owned(),
        });
    }

    let mut registries = Vec::new();
    let mut errors = Vec::new();
    for registry in SERVED {
        let directory = namespace.join(registry.directory);
        if !directory.is_dir() {
            return Err(LoadError::MissingRegistry {
                registry: registry.name,
                path: directory,
            });
        }
        match read_registry(registry, &directory) {
            Ok(contents) => registries.push((registry.name, contents)),
            Err(mut found) => errors.append(&mut found),
        }
    }

    if errors.is_empty() {
        Ok(Loaded { registries })
    } else {
        Err(LoadError::Entries(errors))
    }
}

fn read_registry(
    registry: &'static Registry,
    directory: &Path,
) -> Result<Contents, Vec<EntryError>> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    collect(directory, directory, &mut files, &mut errors);

    // Sorted by name so the order does not depend on the file system. It is
    // not the order anything is sent in — that is the names table's job — but
    // an order that varies between two machines reading the same directory is
    // a difference nobody would think to look for.
    files.sort();

    let mut entries = Vec::with_capacity(files.len());
    for (name, path) in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                errors.push(EntryError {
                    path,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        let document: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(e) => {
                errors.push(EntryError {
                    path,
                    detail: format!("not JSON: {e}"),
                });
                continue;
            }
        };
        match convert::entry(registry, &document) {
            Ok(compound) => entries.push((name, compound)),
            Err(e) => errors.push(EntryError {
                path,
                detail: e.to_string(),
            }),
        }
    }

    if errors.is_empty() {
        Ok(Contents { entries })
    } else {
        Err(errors)
    }
}

/// Walk a registry directory, gathering `(namespaced name, path)`.
///
/// Recursive because a registry's names may contain slashes —
/// `worldgen/biome` does not, but the walk is the same one every registry
/// directory needs and writing the flat version would be writing something
/// that has to be replaced.
fn collect(root: &Path, at: &Path, out: &mut Vec<(String, PathBuf)>, errors: &mut Vec<EntryError>) {
    let listing = match std::fs::read_dir(at) {
        Ok(listing) => listing,
        Err(e) => {
            errors.push(EntryError {
                path: at.to_path_buf(),
                detail: e.to_string(),
            });
            return;
        }
    };
    for found in listing {
        let Ok(found) = found else { continue };
        let path = found.path();
        if path.is_dir() {
            collect(root, &path, out, errors);
        } else if path.extension().is_some_and(|e| e == "json") {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let name = relative
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((format!("minecraft:{name}"), path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_nbt::Tag;

    /// A data directory holding one invented entry for every served registry.
    ///
    /// Invented on purpose, and one per registry rather than one in total:
    /// what is under test here is the walk and the error collection, but a
    /// fixture that has to satisfy all ten schemas also demonstrates that all
    /// ten accept something. Mojang's own files would prove neither better and
    /// would put content in this repository that decision record 0007 keeps
    /// out of it.
    struct Fixture {
        root: PathBuf,
    }

    /// One minimal entry per registry, written by hand against the schema.
    const ENTRIES: &[(&str, &str)] = &[
        (
            "dimension_type/plain.json",
            r##"{"ambient_light":0.0,"bed_works":true,"coordinate_scale":1.0,
                "effects":"dust:plain","has_ceiling":false,"has_raids":true,
                "has_skylight":true,"height":128,"infiniburn":"#dust:burn",
                "logical_height":128,"min_y":0,"monster_spawn_block_light_limit":0,
                "monster_spawn_light_level":7,"natural":true,"piglin_safe":false,
                "respawn_anchor_works":false,"ultrawarm":false}"##,
        ),
        (
            "worldgen/biome/meadow.json",
            r##"{"has_precipitation":true,"temperature":0.5,"downfall":0.8,
                "effects":{"fog_color":1,"sky_color":2,"water_color":3,
                "water_fog_color":4}}"##,
        ),
        (
            "chat_type/mutter.json",
            r##"{"chat":{"translation_key":"dust.mutter","parameters":["sender","content"]},
                "narration":{"translation_key":"dust.mutter.narrate","parameters":["sender"]}}"##,
        ),
        (
            "damage_type/boredom.json",
            r##"{"message_id":"boredom","scaling":"never","exhaustion":0.0}"##,
        ),
        (
            "banner_pattern/stripe.json",
            r##"{"asset_id":"dust:stripe","translation_key":"dust.banner.stripe"}"##,
        ),
        (
            "painting_variant/wide.json",
            r##"{"asset_id":"dust:wide","width":4,"height":2}"##,
        ),
        (
            "wolf_variant/dusty.json",
            r##"{"wild_texture":"dust:wolf","tame_texture":"dust:wolf_tame",
                "angry_texture":"dust:wolf_angry","biomes":"dust:meadow"}"##,
        ),
        (
            "trim_pattern/notch.json",
            r##"{"asset_id":"dust:notch","template_item":"dust:template",
                "description":{"translate":"dust.trim.notch"},"decal":false}"##,
        ),
        (
            "trim_material/grit.json",
            r##"{"asset_name":"grit","ingredient":"dust:grit","item_model_index":0.5,
                "description":{"translate":"dust.material.grit","color":"#AABBCC"},
                "override_armor_materials":{"dust:tin":"grit_darker"}}"##,
        ),
        (
            "jukebox_song/hum.json",
            r##"{"sound_event":"dust:hum","description":{"translate":"dust.song.hum"},
                "length_in_seconds":12.5,"comparator_output":3}"##,
        ),
    ];

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "dust-registry-source-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let me = Self { root };
            for (relative, body) in ENTRIES {
                me.write(relative, body);
            }
            me
        }

        fn write(&self, relative: &str, body: &str) {
            let path = self.root.join("minecraft").join(relative);
            std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
            std::fs::write(path, body).expect("write");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn every_served_registry_has_a_fixture_entry() {
        // Otherwise a schema could be added with nothing that ever converts
        // under it, and `load` would refuse the fixture directory for a
        // reason that looks like a bug in the walk.
        for registry in crate::registries::schema::SERVED {
            let prefix = format!("{}/", registry.directory);
            assert!(
                ENTRIES.iter().any(|(path, _)| path.starts_with(&prefix)),
                "{} has no fixture entry",
                registry.name
            );
        }
    }

    #[test]
    fn all_ten_registries_load_from_one_directory() {
        let fixture = Fixture::new("all-ten");
        let loaded = load(&fixture.root).expect("loads");
        let counts: Vec<_> = loaded.summary().collect();
        assert_eq!(counts.len(), crate::registries::schema::SERVED.len());
        for (name, count) in counts {
            assert_eq!(count, 1, "{name} read its one entry");
        }
    }

    #[test]
    fn a_directory_of_json_becomes_entries_named_the_way_the_wire_names_them() {
        let fixture = Fixture::new("names");
        let loaded = load(&fixture.root).expect("loads");
        let biomes = loaded
            .get("minecraft:worldgen/biome")
            .expect("the biome registry");
        assert_eq!(biomes.len(), 1);
        let meadow = biomes
            .get("minecraft:meadow")
            .expect("named and namespaced");
        assert_eq!(meadow.get("downfall"), Some(&Tag::Float(0.8)));
    }

    #[test]
    fn a_nested_directory_keeps_the_slash_in_the_name() {
        let fixture = Fixture::new("nested");
        fixture.write(
            "worldgen/biome/deep/trench.json",
            r#"{"has_precipitation":false,"temperature":0.1,"downfall":0.0,
                "effects":{"fog_color":1,"sky_color":2,"water_color":3,
                "water_fog_color":4}}"#,
        );
        let loaded = load(&fixture.root).expect("loads");
        let biomes = loaded.get("minecraft:worldgen/biome").expect("biomes");
        assert!(biomes.get("minecraft:deep/trench").is_some());
    }

    #[test]
    fn every_bad_file_is_reported_and_not_just_the_first() {
        // Four mistakes should cost one reading, not four.
        let fixture = Fixture::new("all-errors");
        fixture.write("worldgen/biome/one.json", "{ not json");
        fixture.write("worldgen/biome/two.json", r#"{"temperature":0.5}"#);
        fixture.write(
            "worldgen/biome/three.json",
            r#"{"has_precipitation":true,"temperature":0.5,"downfall":0.1,
                "effects":{"fog_color":1,"sky_color":2,"water_color":3,
                "water_fog_color":4},"nonsense":1}"#,
        );
        let error = load(&fixture.root).expect_err("three are wrong");
        let LoadError::Entries(errors) = error else {
            panic!("expected entry errors");
        };
        assert_eq!(errors.len(), 3, "one per bad file, not one in total");
    }

    #[test]
    fn a_directory_with_no_minecraft_namespace_is_refused_by_name() {
        let root = std::env::temp_dir().join(format!("dust-empty-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let error = load(&root).expect_err("nothing in it");
        assert!(matches!(error, LoadError::NotADirectory { .. }));
        assert!(error.to_string().contains("minecraft"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_half_populated_directory_names_the_registry_it_is_missing() {
        let root = std::env::temp_dir().join(format!("dust-half-{}", std::process::id()));
        std::fs::create_dir_all(root.join("minecraft/dimension_type")).expect("mkdir");
        let error = load(&root).expect_err("no biomes");
        assert!(
            error.to_string().contains("worldgen/biome"),
            "says which one: {error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
