//! `cargo xtask extract --version 1.21.1`.
//!
//! Minecraft's own data generators emit the block state table, the registries,
//! the recipes and the packet report. Dust needs that information and may not
//! redistribute the files it arrives in, so the pipeline is: download the
//! server jar on this machine, run its generators, read the reports, and commit
//! the *Rust* that results.
//!
//! This runs by hand, a few times per Minecraft release. It is deliberately not
//! part of `just verify` — it needs a network, a JVM and a fifty-megabyte
//! download, and CI has no business doing any of that. What CI checks is the
//! generated code, which is committed, compiles, and has the round-trip tests
//! over every block state and every registry entry in `dust-registry` and
//! every packet id in `dust-protocol` — and, beside them, the golden samples,
//! which are the only part that can tell the tables apart from a
//! self-consistent wrong answer.
//!
//! Two of the jar's generators are needed and they write different trees.
//! `--reports` produces the block state table, the registries and the packet
//! report; `--server` produces the worldgen data, which is where the vanilla
//! ore baseline in `dust-gen` comes from. Each is cached on a path only that
//! generator writes, so having one does not look like having both.

mod blocks;
mod codegen;
mod commands;
mod download;
mod items;
mod numbers;
mod packets;
mod registries;
mod sha1;
mod worldgen;

use std::path::{Path, PathBuf};

/// Where the server jar and the generated reports are cached. Gitignored, and
/// outside `target/` so that `cargo clean` does not throw away a fifty-megabyte
/// download.
const CACHE_DIR: &str = ".dust-extract";

pub struct Options {
    pub version: String,
    /// A server jar the operator has already obtained, instead of downloading.
    pub server_jar: Option<PathBuf>,
}

pub fn run(options: &Options, workspace_root: &Path) -> Result<(), String> {
    let cache = workspace_root.join(CACHE_DIR);
    let version = &options.version;

    let jar = match &options.server_jar {
        Some(path) => {
            if !path.exists() {
                return Err(format!("{} does not exist", path.display()));
            }
            println!("using the server jar at {}", path.display());
            path.clone()
        }
        None => download::server_jar(version, &cache)?,
    };

    let reports = generate(
        &jar,
        &cache.join(format!("reports-{version}")),
        "--reports",
        "reports/blocks.json",
        &cache,
        workspace_root,
    )?;
    let server_data = generate(
        &jar,
        &cache.join(format!("data-{version}")),
        "--server",
        "data/minecraft/worldgen/placed_feature",
        &cache,
        workspace_root,
    )?;

    let block_json = std::fs::read(reports.join("reports/blocks.json"))
        .map_err(|e| format!("could not read the generated block report: {e}"))?;
    let parsed = blocks::parse(&block_json)?;
    println!(
        "read {} blocks and {} states from the block report",
        parsed.blocks.len(),
        parsed.state_count
    );

    let registry_json = std::fs::read(reports.join("reports/registries.json"))
        .map_err(|e| format!("could not read the generated registry report: {e}"))?;
    let flat = registries::parse(&registry_json)?;
    registries::check_block_ids_match_state_order(&flat, &parsed)?;
    report_what_the_registries_said(&flat);

    let item_json = std::fs::read(reports.join("reports/items.json"))
        .map_err(|e| format!("could not read the generated item report: {e}"))?;
    let item_components = items::parse(&item_json, &flat, &parsed)?;
    report_what_the_items_said(&item_components);

    let generated = workspace_root.join("crates/dust-registry/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("blocks.rs");
    std::fs::write(&path, codegen::blocks(&parsed, version, &parsed.reported))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    let path = generated.join("registries.rs");
    std::fs::write(&path, codegen::registries(&flat, version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    let path = generated.join("items.rs");
    std::fs::write(&path, codegen::items(&item_components, version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    extract_packets(&reports, version, workspace_root)?;
    ores(&server_data.join("data"), workspace_root, version)?;

    // Emitted unformatted on purpose: rustfmt is the one authority on how the
    // committed file looks, and a generator that lays code out itself will
    // disagree with it eventually.
    println!("\nRun `just fmt` — these are committed as rustfmt leaves them.");
    println!(
        "Then `cargo test --workspace` — the round-trip over all {} states, {} registry \
         entries and every packet id, the golden samples beside them, and the ore \
         baseline's source-row check are what say this worked.",
        parsed.state_count, flat.entry_count
    );
    Ok(())
}

/// Print what the registry report turned out to say, rather than only that it
/// was accepted.
///
/// The checks in `registries` are all of the shape "refuse if not", which makes
/// a silent success indistinguishable from a check that stopped looking. These
/// are the numbers that would move if it had.
fn report_what_the_registries_said(flat: &registries::Registries) {
    let disagree = flat
        .registries
        .iter()
        .filter(|r| r.name_order_disagrees)
        .count();
    let defaults = flat
        .registries
        .iter()
        .filter(|r| r.default.is_some())
        .count();
    let namespaces: Vec<&str> = flat.namespaces.iter().map(String::as_str).collect();
    println!(
        "read {} flat registries and {} entries from the registry report",
        flat.registries.len(),
        flat.entry_count
    );
    println!(
        "  every registry's protocol ids run 0..n with no gap and no repeat, so the \
         generated tables index by id"
    );
    println!(
        "  {disagree} of {} have a name order that is not their protocol-id order, which is \
         why both index arrays are emitted",
        flat.registries.len()
    );
    println!("  {defaults} carry a default, and each names an entry that exists");
    println!("  entry namespaces: {}", namespaces.join(", "));
    println!(
        "  {} is not emitted here; its {} protocol ids are the order of blocks.rs's base \
         state ids, checked",
        registries::BLOCK_REGISTRY,
        flat.block.entries.len()
    );
}

/// Print what the item report turned out to say.
fn report_what_the_items_said(items: &items::Items) {
    println!(
        "read {} items and {} distinct component maps from the item report",
        items.items.len(),
        items.maps.len()
    );
    println!(
        "  every one of the {} numbers in the file re-prints to its own text, so nothing is \
         being stored at the wrong width",
        items.number_count
    );
    println!(
        "  {} distinct components appear as defaults, all of them entries of the \
         data_component_type registry",
        items.components.len()
    );
    let mut by_count: Vec<(&String, &usize)> = items.components.iter().collect();
    by_count.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    let head: Vec<String> = by_count
        .iter()
        .take(6)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect();
    println!("  the most common are {}", head.join(", "));
    println!(
        "  string values that are not namespaced ids or #tags: {:?} — there is no free text \
         in this report",
        items.non_id_strings
    );
}

/// Read the packet report and regenerate `dust-protocol`'s tables for this
/// version.
///
/// One version at a time, and the others stay where they are: each version's
/// table is its own module, and the index that lists them is rebuilt from the
/// modules on disk. Running this for 1.21.4 later adds a file and a row rather
/// than replacing 1.21.1.
fn extract_packets(reports: &Path, version: &str, workspace_root: &Path) -> Result<(), String> {
    let json = std::fs::read(reports.join("reports/packets.json"))
        .map_err(|e| format!("could not read the generated packet report: {e}"))?;
    let parsed = packets::parse(&json)?;

    println!("\nread {} packets from the packet report:", parsed.total);
    for group in &parsed.groups {
        let state = format!("{}/{}", group.state, group.direction);
        if !group.present {
            // Not a failure: on 1.21.1 the server says nothing during the
            // handshake. Printed anyway, because a pair that vanishes in a
            // later version has to be something somebody sees.
            println!("  {state:<26} absent from the report");
            continue;
        }
        println!(
            "  {state:<26} {:>3} packets, ids 0..{}",
            group.count(),
            group.by_id.len()
        );
        if !group.holes.is_empty() {
            // Loud, and deliberately not fatal: the table encodes the gap as an
            // id that decodes to nothing, which is what the report says. Closing
            // it up would renumber every packet after it.
            println!(
                "  !! {state} leaves protocol id(s) {:?} unclaimed. The ids in this pair are \
                 NOT contiguous, and the generated table has holes where the report does.",
                group.holes
            );
        }
    }

    let module = codegen::module_name(version)?;
    let generated = workspace_root.join("crates/dust-protocol/src/generated");
    let versions = generated.join("packets");
    std::fs::create_dir_all(&versions)
        .map_err(|e| format!("could not create {}: {e}", versions.display()))?;

    let path = versions.join(format!("{module}.rs"));
    std::fs::write(&path, codegen::packets(&parsed, version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    let path = generated.join("packets.rs");
    std::fs::write(&path, codegen::packet_index(&version_modules(&versions)?))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Read the vanilla ore baseline out of the `--server` tree and write it into
/// `dust-gen`.
fn ores(data_root: &Path, workspace_root: &Path, version: &str) -> Result<(), String> {
    let ores = worldgen::parse(data_root)?;
    println!(
        "\nread {} ore placements in {} group(s) across {} dimension(s)",
        ores.placements.len(),
        ores.groups.len(),
        ores.dimensions.len()
    );

    // Which groups the configuration's hand-written vanilla list knows about,
    // said out loud rather than left for someone to work out from the table.
    // The half it does not know about are the stone variants and terrain blobs,
    // and they are knobs too — see xtask/src/extract/worldgen.rs.
    let known: Vec<&str> = ores
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| dust_config::ore::VANILLA_ORE_GROUPS.contains(n))
        .collect();
    let rest: Vec<&str> = ores
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| !dust_config::ore::VANILLA_ORE_GROUPS.contains(n))
        .collect();
    println!("  ores the configuration documents: {}", known.join(", "));
    println!("  other groups the ore feature places: {}", rest.join(", "));
    if ores.ungrouped.is_empty() {
        println!("  every placement was grouped");
    } else {
        println!(
            "  NOT GROUPED, and therefore not in the table — no setting will reach \
             these: {}",
            ores.ungrouped.join(", ")
        );
    }

    let generated = workspace_root.join("crates/dust-gen/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("ores.rs");
    std::fs::write(&path, codegen::ores(&ores, version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The version modules on disk, in file-name order.
///
/// Read from the directory rather than kept in a list, so that the index and
/// the modules cannot disagree: there is one place a version exists, and it is
/// the file.
fn version_modules(directory: &Path) -> Result<Vec<String>, String> {
    let mut modules = Vec::new();
    let entries = std::fs::read_dir(directory)
        .map_err(|e| format!("could not read {}: {e}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        match name.strip_suffix(".rs") {
            Some(stem) => modules.push(stem.to_owned()),
            None => {
                return Err(format!(
                    "{} holds {name}, which is not a generated version module. The index is \
                     built from whatever is in that directory, so it may hold nothing else.",
                    directory.display()
                ))
            }
        }
    }
    modules.sort();
    Ok(modules)
}

/// An absolute form of `path`, for handing to a process running somewhere else.
///
/// `std::path::absolute` rather than `canonicalize`, because the output
/// directory does not exist yet and canonicalising a path that is not there
/// fails.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| {
        format!(
            "could not resolve {} to an absolute path: {e}",
            path.display()
        )
    })
}

/// Run one of the server jar's own data generators.
///
/// `marker` is a path inside the output that only exists once that generator
/// has run, so the two trees cache independently: `--reports` and `--server`
/// write different things, and having one already does not mean having the
/// other.
///
/// `scratch` is the directory java is run *in*, and it has to be the cache
/// rather than wherever the operator happened to be standing. The 1.21.1 server
/// jar is a bundler: before it runs anything it unpacks its libraries and a
/// second copy of the server jar into the process's working directory, which
/// with no `current_dir` is the workspace root. That left 55 MB of Mojang jars
/// in `libraries/` and `versions/` beside `Cargo.toml`, matched by no
/// `.gitignore` pattern, one `git add -A` away from committing exactly what
/// this project's licensing line exists to keep out. The paths are made
/// absolute first because they are about to be read from somewhere else.
fn generate(
    jar: &Path,
    output: &Path,
    generator: &str,
    marker: &str,
    scratch: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    if output.join(marker).exists() {
        println!(
            "using the cached {generator} output in {}",
            output.display()
        );
        return Ok(output.to_path_buf());
    }

    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("could not create {}: {e}", scratch.display()))?;
    let jar = absolute(jar)?;
    let output = absolute(output)?;

    // Taken before the run so that anything the generators leave behind can be
    // named afterwards. Deliberately a snapshot of the whole directory rather
    // than a check for `libraries/`, `versions/` and `logs/` by name: those are
    // the three 1.21.1 happens to unpack, and a guard that lists them can only
    // fail on the cases whoever wrote it already knew about.
    let before = top_level_entries(workspace_root)?;

    println!("running Minecraft's {generator} data generator (this takes a minute)");
    let status = std::process::Command::new("java")
        .current_dir(scratch)
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(&jar)
        .arg(generator)
        .arg("--output")
        .arg(&output)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "java was not found on PATH. Minecraft 1.21.1's data generators need a JDK of \
                 21 or newer."
                    .to_owned()
            }
            _ => format!("could not run java: {e}"),
        })?;

    if !status.success() {
        return Err(format!(
            "Minecraft's data generators exited with {status}. If the message above mentions \
             a class version, the JDK on PATH is older than 21."
        ));
    }
    let escaped: Vec<String> = top_level_entries(workspace_root)?
        .difference(&before)
        .cloned()
        .collect();
    if !escaped.is_empty() {
        return Err(format!(
            "Minecraft's data generators wrote {} into the workspace root. Nothing Mojang \
             ships may land outside {CACHE_DIR}, which is the one directory .gitignore \
             covers; anywhere else it is one `git add -A` from being committed. Delete \
             those and check that java is still being run with its working directory set \
             to the cache.",
            escaped.join(", ")
        ));
    }

    Ok(output.to_path_buf())
}

/// The names directly inside `directory`, for comparing before and after.
fn top_level_entries(directory: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    std::fs::read_dir(directory)
        .map_err(|e| format!("could not read {}: {e}", directory.display()))?
        .map(|entry| {
            entry
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .map_err(|e| format!("could not read {}: {e}", directory.display()))
        })
        .collect()
}
