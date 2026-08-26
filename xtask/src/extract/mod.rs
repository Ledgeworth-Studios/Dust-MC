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
//! over every block state and every registry entry in `dust-registry` — and,
//! beside them, the golden samples, which are the only part that can tell the
//! tables apart from a self-consistent wrong answer.

mod blocks;
mod codegen;
mod commands;
mod download;
mod items;
mod numbers;
mod registries;
mod sha1;

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

    let reports = generate_reports(&jar, &cache.join(format!("reports-{version}")))?;

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

    // Emitted unformatted on purpose: rustfmt is the one authority on how the
    // committed file looks, and a generator that lays code out itself will
    // disagree with it eventually.
    println!("\nRun `just fmt` — these are committed as rustfmt leaves them.");
    println!(
        "Then `cargo test -p dust-registry` — the round-trip over all {} states and \
         {} registry entries, and the golden samples beside it, are what say this worked.",
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

/// Run the server jar's own data generators.
fn generate_reports(jar: &Path, output: &Path) -> Result<PathBuf, String> {
    if output.join("reports/blocks.json").exists() {
        println!("using the cached reports in {}", output.display());
        return Ok(output.to_path_buf());
    }

    println!("running Minecraft's data generators (this takes a minute)");
    let status = std::process::Command::new("java")
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(jar)
        .arg("--reports")
        .arg("--output")
        .arg(output)
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
    Ok(output.to_path_buf())
}
