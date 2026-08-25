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
//! over every block state in `dust-registry` and every packet id in
//! `dust-protocol`.

mod blocks;
mod codegen;
mod download;
mod packets;
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

    let generated = workspace_root.join("crates/dust-registry/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("blocks.rs");
    std::fs::write(&path, codegen::blocks(&parsed, version, &parsed.reported))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    extract_packets(&reports, version, workspace_root)?;

    // The generated files are committed as `cargo fmt` leaves them, because
    // they are ordinary members of their crates and `just verify` starts by
    // checking formatting. Emitting them already formatted would mean
    // reimplementing rustfmt here, so the operator is told instead.
    println!(
        "\nRun `just fmt`, then `cargo test --workspace` — the round-trip over all {} block \
         states and every packet id is what says this worked.",
        parsed.state_count
    );
    Ok(())
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
