//! The world's own spawn point, which lives beside the region files rather
//! than in them.
//!
//! # Why this is a separate read
//!
//! `[server].world_source` names a directory of `.mca` files, and everything
//! else Dust serves comes out of those files. A world's spawn point does not:
//! it is in `level.dat`, one level up, alongside the region directory rather
//! than inside it.
//!
//! So a server that reads only the region files knows every block of a world
//! and not where its owner is meant to stand in it. Until this existed, Dust
//! spawned every player at x 0, z 0 — which on Minecraft's own seed 1 is 176
//! blocks from the spawn the world was generated with, in open ocean, and on
//! seed 0 is 32 blocks off. Both worlds look, to a player joining them, like a
//! server that lost the world and generated a different one.
//!
//! # What is read and what is refused
//!
//! Three integers and a float from `Data`: `SpawnX`, `SpawnZ`, `SpawnAngle`.
//!
//! `SpawnY` is deliberately not among them. A stored y is a claim about what
//! the world was when it was saved, and the block at that column may have been
//! dug out since; the y Dust uses comes from the column's own heightmap, which
//! is a fact about the world being served right now. See
//! [`spawn_at`](super::world::spawn_at) for why that matters more than it
//! sounds.
//!
//! **A missing `level.dat` is not an error and a broken one is.** A region
//! directory with no world file beside it is a legitimate arrangement — it is
//! what `harness rewrite` produces, and what an operator pointing at a
//! directory of chunks they extracted has — and the answer there is the origin,
//! same as before. A `level.dat` that exists and cannot be read is different:
//! it is a world that has a spawn point which this server would then ignore,
//! and a player put at the origin of a world whose spawn is somewhere else is
//! the silent kind of wrong. It refuses to start, for the same reason the save
//! file beside it does.

use std::path::Path;

/// Where a world says its players belong.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldSpawn {
    /// The spawn column's x, in blocks.
    pub x: i32,
    /// The spawn column's z, in blocks.
    pub z: i32,
    /// The yaw a player faces on arriving, in degrees.
    pub angle: f32,
}

/// The file name Minecraft gives a world's own record of itself.
const LEVEL_DAT: &str = "level.dat";

/// The seed of the world whose region directory this is.
///
/// `None` when there is no `level.dat`, when it cannot be read, or when it
/// does not carry a seed — all three of which are the same answer to the
/// caller, and none of them an error.
///
/// **That is deliberately softer than [`spawn_beside`], and the difference is
/// what each one is for.** A spawn point that exists and is ignored puts every
/// player in the wrong place in a world that is otherwise right, so an
/// unreadable one stops the server. A seed is only ever used to generate the
/// columns *off the edge* of the world file; not having one costs a plain
/// there instead of terrain, which is what Dust served everywhere until this
/// existed. Refusing to start over it would be refusing to serve a world this
/// server can serve.
pub fn seed_beside(region_directory: &Path) -> Option<i64> {
    let world_directory = region_directory.parent()?;
    let bytes = std::fs::read(world_directory.join(LEVEL_DAT)).ok()?;
    read_seed(&bytes)
}

/// The seed inside a `level.dat`'s bytes, compressed or not.
fn read_seed(bytes: &[u8]) -> Option<i64> {
    let plain = dust_nbt::compression::decompress_detected(bytes, LEVEL_DAT_LIMIT).ok()?;
    let document = dust_nbt::read::from_bytes(&plain).ok()?;
    let dust_nbt::Tag::Compound(root) = &document.tag else {
        return None;
    };
    let dust_nbt::Tag::Compound(data) = root.get("Data")? else {
        return None;
    };
    // 1.16 moved it here from `Data.RandomSeed`, and both spellings are read:
    // an operator with an older save is serving an older world, not a broken
    // one.
    if let Some(dust_nbt::Tag::Compound(settings)) = data.get("WorldGenSettings") {
        if let Some(dust_nbt::Tag::Long(seed)) = settings.get("seed") {
            return Some(*seed);
        }
    }
    match data.get("RandomSeed") {
        Some(dust_nbt::Tag::Long(seed)) => Some(*seed),
        _ => None,
    }
}

/// Read the spawn point of the world whose region directory this is.
///
/// `Ok(None)` when there is no `level.dat` beside the directory. `Err` when
/// there is one and it does not answer the question — see the module note for
/// why those two are not the same answer.
///
/// # Errors
///
/// The file exists but cannot be read, is not NBT, has no `Data` compound, or
/// is missing one of the spawn keys.
pub fn spawn_beside(region_directory: &Path) -> Result<Option<WorldSpawn>, String> {
    // The region directory's parent, which is the world directory. A path with
    // no parent — a bare relative name — has no world beside it to read.
    let Some(world_directory) = region_directory.parent() else {
        return Ok(None);
    };
    let path = world_directory.join(LEVEL_DAT);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    read_spawn(&bytes).map(Some).map_err(|why| {
        format!(
            "{} is a world file this server cannot read its spawn point out of: {why}. \
             Starting anyway would put every player at x 0, z 0 in a world whose spawn \
             is somewhere else.",
            path.display()
        )
    })
}

/// The spawn point inside a `level.dat`'s bytes, compressed or not.
///
/// Split from [`spawn_beside`] so the parsing has tests that need no
/// directory: everything below the file system is a pure function of bytes.
fn read_spawn(bytes: &[u8]) -> Result<WorldSpawn, String> {
    let plain = dust_nbt::compression::decompress_detected(bytes, LEVEL_DAT_LIMIT)
        .map_err(|e| format!("it did not decompress: {e}"))?;
    let document = dust_nbt::read::from_bytes(&plain).map_err(|e| format!("it is not NBT: {e}"))?;
    let dust_nbt::Tag::Compound(root) = &document.tag else {
        return Err("its root is not a compound".to_owned());
    };
    let Some(dust_nbt::Tag::Compound(data)) = root.get("Data") else {
        return Err("it has no `Data` compound".to_owned());
    };

    let int = |name: &str| match data.get(name) {
        Some(dust_nbt::Tag::Int(value)) => Ok(*value),
        Some(other) => Err(format!(
            "`Data.{name}` is {:?} rather than a TAG_Int",
            other.tag_type()
        )),
        None => Err(format!("it has no `Data.{name}`")),
    };

    Ok(WorldSpawn {
        x: int("SpawnX")?,
        z: int("SpawnZ")?,
        // The one optional key. A world written before angles were stored has
        // no opinion about which way a player faces, and facing due south is
        // the answer vanilla gives when the field is absent — a default that
        // is a real behaviour rather than a stand-in for a missing number.
        angle: match data.get("SpawnAngle") {
            Some(dust_nbt::Tag::Float(value)) => *value,
            Some(other) => {
                return Err(format!(
                    "`Data.SpawnAngle` is {:?} rather than a TAG_Float",
                    other.tag_type()
                ))
            }
            None => 0.0,
        },
    })
}

/// How much `level.dat` is allowed to decompress to.
///
/// A world file is a few kilobytes; vanilla's own is under two. The library's
/// 32 MiB file default is sized for chunk data and is far more headroom than
/// anything here needs, and this path reads a file named by configuration.
const LEVEL_DAT_LIMIT: usize = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use dust_nbt::{Compound, Tag};

    /// A directory of this test's own, named after it.
    ///
    /// The same shape as `save`'s: process id and a counter, so two tests in
    /// one binary and two binaries at once never share one. A dependency for
    /// this would be a dependency in the licence gate for eight lines.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dust-level-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("region")).expect("temp dir");
        dir
    }

    /// A `level.dat` holding the keys this module reads, written the way
    /// Minecraft writes one: gzip around a file-form document whose root is
    /// unnamed and holds a single `Data`.
    fn level_dat(data: Compound) -> Vec<u8> {
        let mut root = Compound::new();
        root.insert("Data", Tag::Compound(data));
        let plain = dust_nbt::write::to_vec("", &Tag::Compound(root)).expect("writable");
        dust_nbt::compression::compress(&plain, dust_nbt::Compression::Gzip).expect("compressible")
    }

    fn spawn_of(x: i32, z: i32) -> Compound {
        let mut data = Compound::new();
        data.insert("SpawnX", Tag::Int(x));
        data.insert("SpawnY", Tag::Int(67));
        data.insert("SpawnZ", Tag::Int(z));
        data
    }

    #[test]
    fn the_seed_comes_out_of_either_place_a_world_has_kept_it() {
        // 1.16 moved the seed from `Data.RandomSeed` into
        // `Data.WorldGenSettings.seed`, and a server is asked to serve both.
        let mut modern = spawn_of(0, 0);
        let mut settings = Compound::new();
        settings.insert("seed", Tag::Long(-4172144997902289642));
        modern.insert("WorldGenSettings", Tag::Compound(settings));
        assert_eq!(read_seed(&level_dat(modern)), Some(-4172144997902289642));

        let mut ancient = spawn_of(0, 0);
        ancient.insert("RandomSeed", Tag::Long(7));
        assert_eq!(read_seed(&level_dat(ancient)), Some(7));
    }

    #[test]
    fn a_world_file_with_no_seed_in_it_is_not_an_error() {
        // Softer than the spawn read on purpose, and this is the test that
        // says so: a missing seed costs a plain off the edge of the disc,
        // which is what Dust served everywhere before there was a generator.
        // A missing spawn point puts every player in the wrong place in a
        // world that is otherwise right, and that one refuses to start.
        assert_eq!(read_seed(&level_dat(spawn_of(0, 0))), None);
        assert_eq!(read_seed(b"not a world file at all"), None);
        assert!(read_spawn(b"not a world file at all").is_err());
    }

    #[test]
    fn there_is_no_seed_beside_a_directory_with_no_world_file() {
        let dir = temp_dir("no-seed");
        assert_eq!(seed_beside(&dir.join("region")), None);
    }

    #[test]
    fn the_three_numbers_come_back_out() {
        let mut data = spawn_of(-32, 0);
        data.insert("SpawnAngle", Tag::Float(90.0));
        let spawn = read_spawn(&level_dat(data)).expect("readable");
        assert_eq!(
            spawn,
            WorldSpawn {
                x: -32,
                z: 0,
                angle: 90.0
            }
        );
    }

    #[test]
    fn an_uncompressed_world_file_reads_the_same() {
        // `Compression::detect` is what makes this work, and it is worth a
        // test because a world file that has been through a tool which
        // decompressed it is a real thing an operator will have.
        let mut root = Compound::new();
        root.insert("Data", Tag::Compound(spawn_of(7, 9)));
        let plain = dust_nbt::write::to_vec("", &Tag::Compound(root)).expect("writable");
        let spawn = read_spawn(&plain).expect("readable");
        assert_eq!((spawn.x, spawn.z), (7, 9));
    }

    #[test]
    fn an_absent_angle_faces_south_rather_than_failing() {
        let spawn = read_spawn(&level_dat(spawn_of(0, 0))).expect("readable");
        assert_eq!(spawn.angle, 0.0);
    }

    #[test]
    fn a_missing_spawn_key_is_named_rather_than_defaulted() {
        // Defaulting a missing `SpawnZ` to zero would put a player on the
        // right meridian of the wrong world and say nothing. The message names
        // the key so an operator can look at the file.
        let mut data = spawn_of(-32, 0);
        data.remove("SpawnZ");
        let why = read_spawn(&level_dat(data)).expect_err("refused");
        assert!(why.contains("SpawnZ"), "{why}");
    }

    #[test]
    fn a_spawn_key_of_the_wrong_type_is_refused_by_type() {
        // NBT has six number types and reading a `TAG_Long` as an int is a
        // choice about which half of it to keep. There is no right half.
        let mut data = spawn_of(-32, 0);
        data.insert("SpawnX", Tag::Long(-32));
        let why = read_spawn(&level_dat(data)).expect_err("refused");
        assert!(why.contains("SpawnX") && why.contains("TAG_Int"), "{why}");
    }

    #[test]
    fn bytes_that_are_not_nbt_are_refused() {
        let why = read_spawn(b"this is not a world").expect_err("refused");
        assert!(
            why.contains("not NBT") || why.contains("decompress"),
            "{why}"
        );
    }

    #[test]
    fn a_region_directory_with_no_world_file_beside_it_answers_none() {
        // The `harness rewrite` case, and the case of an operator who copied
        // out a directory of chunks. Not an error: there is no spawn point to
        // be wrong about.
        let dir = temp_dir("no-world-file");
        assert_eq!(spawn_beside(&dir.join("region")), Ok(None));
    }

    #[test]
    fn a_world_file_that_cannot_be_read_stops_the_server() {
        // The whole reason absent and broken are different answers. A player
        // put at the origin of a world whose spawn is elsewhere sees a server
        // that lost the world, and nothing in the log would say otherwise.
        let dir = temp_dir("broken-world-file");
        std::fs::write(dir.join(LEVEL_DAT), b"not a world").expect("writable");
        let why = spawn_beside(&dir.join("region")).expect_err("refused");
        assert!(why.contains("level.dat"), "{why}");
        assert!(
            why.contains("x 0, z 0"),
            "the message says what it prevents"
        );
    }

    #[test]
    fn a_world_file_beside_the_region_directory_is_found() {
        let dir = temp_dir("found");
        std::fs::write(dir.join(LEVEL_DAT), level_dat(spawn_of(112, 176))).expect("writable");
        assert_eq!(
            spawn_beside(&dir.join("region")),
            Ok(Some(WorldSpawn {
                x: 112,
                z: 176,
                angle: 0.0
            }))
        );
    }
}
