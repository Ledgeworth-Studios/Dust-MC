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
//! `--reports` produces the block state table, the registries, the items, the
//! command graph and the packet report; `--server` produces the data pack —
//! recipes, loot tables, tags and the worldgen trees, which is where the
//! vanilla ore baseline in `dust-gen` comes from. Each is cached on a path only
//! that generator writes, so having one does not look like having both, and
//! only the trees the selected domains read are generated at all.
//!
//! # Domains
//!
//! The work is split into [`Domain`]s so a change to one table does not demand
//! a full run to debug it (`--only tags`). Every domain reads the same two
//! cached trees, prints what it found rather than only that it succeeded, and
//! writes its own file or files; nothing a later domain writes depends on an
//! earlier one having been selected in the same run, because everything they
//! share comes from the reports themselves.
//!
//! **[`Domain::Mappings`] is the exception to every sentence above**, and it is
//! stated here rather than left to be discovered. It reads neither generated
//! tree — it reads the obfuscated-name table Mojang publish beside the jar —
//! and it writes nothing into `crates/`, because what it produces is Mojang's
//! names rather than a fact about a format, and they are of no use to anything
//! but a program running against that exact jar. Its output lands in the
//! extract cache behind the same `.gitignore` line the jar does. Decision
//! record 0008 is why such a domain has to exist at all: a block's opacity and
//! its light emission are constants in Java code, in no report and no data
//! pack, and an oracle reflecting into the jar is the only source for them that
//! is not an invention.

mod blocks;
mod codegen;
mod commands;
mod entities;
mod fluids;
mod items;
mod loot;
mod mappings;
mod numbers;
mod packets;
mod recipes;
mod registries;
mod synced;
mod tags;
mod worldgen;

// Reused by the differential harness (`cargo xtask harness provision`), which
// needs the same jar resolved the same way: manifest lookup, SHA-1 checked on
// every run including cache hits. One resolver means one place that can be
// wrong about either.
pub(crate) mod download;
pub(crate) mod sha1;

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where the server jar and the generated reports are cached. Gitignored, and
/// outside `target/` so that `cargo clean` does not throw away a fifty-megabyte
/// download.
const CACHE_DIR: &str = ".dust-extract";

/// One extractable area of vanilla data, and one unit of `--only`.
///
/// The order here is the execution order when everything runs: blocks first,
/// because its two tables are cross-checked against each other and several
/// later checks quote them; worldgen last, because it is the slowest reader of
/// the largest tree and nothing waits on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// The block state table, plus the flat-registry tables it is checked
    /// against. These two are one domain because neither is emitted until the
    /// two reports have agreed on the block's protocol ids.
    Blocks,
    /// Item default data components.
    Items,
    /// Entity types: the registry's own facts, and its golden rows.
    Entities,
    /// Fluids and their block and bucket relationships.
    Fluids,
    /// The brigadier command graph.
    Commands,
    /// Recipe shapes: the serialiser vocabulary and the keys each takes.
    Recipes,
    /// Loot tables: the inventory and the pool/condition/function vocabulary.
    Loot,
    /// The five tag directories as overlayable baseline data.
    Tags,
    /// The datapack registries a server sends during configuration.
    Synced,
    /// Packet id tables for `dust-protocol`.
    Packets,
    /// Worldgen: the ore baseline in `dust-gen`.
    Worldgen,
    /// The per-block-state constants Minecraft keeps in Java, asked of the
    /// game itself.
    ///
    /// Reads nothing either generator produced. Runs a small Java program on
    /// the jar's own classpath and prints what Minecraft says a block state's
    /// opacity, light emission and occlusion are, and which of the six
    /// heightmaps count it — the values decision records 0008 and 0010 exist
    /// because no report and no data pack carries them.
    Constants,
    /// The obfuscated-name table an oracle needs to call into the jar.
    ///
    /// The odd one out, and it says so: every other domain reads what the data
    /// generators *produced*, and this one reads the mappings published beside
    /// the jar because what it is after was never in a report. Decision record
    /// 0008 is why there has to be such a domain at all.
    Mappings,
}

/// Every domain, in execution order.
pub const ALL_DOMAINS: &[Domain] = &[
    Domain::Blocks,
    Domain::Items,
    Domain::Entities,
    Domain::Fluids,
    Domain::Commands,
    Domain::Recipes,
    Domain::Loot,
    Domain::Tags,
    Domain::Synced,
    Domain::Packets,
    Domain::Worldgen,
    Domain::Mappings,
    Domain::Constants,
];

impl Domain {
    /// The name `--only` spells it with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::Items => "items",
            Self::Entities => "entities",
            Self::Fluids => "fluids",
            Self::Commands => "commands",
            Self::Recipes => "recipes",
            Self::Loot => "loot",
            Self::Tags => "tags",
            Self::Synced => "synced",
            Self::Packets => "packets",
            Self::Worldgen => "worldgen",
            Self::Mappings => "mappings",
            Self::Constants => "constants",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        ALL_DOMAINS.iter().copied().find(|d| d.name() == name)
    }

    /// Whether this domain reads the `--reports` tree. Recipes does because its
    /// shapes are checked against the recipe_serializer registry on the way in.
    fn needs_reports(self) -> bool {
        matches!(
            self,
            Self::Blocks
                | Self::Items
                | Self::Entities
                | Self::Fluids
                | Self::Commands
                | Self::Recipes
                | Self::Loot
                | Self::Tags
                | Self::Packets
        )
    }

    /// Whether this domain reads the `--reports` tree even when
    /// [`Self::needs_reports`] said no — worldgen does, because the
    /// biome-parameter report lives beside blocks.json and friends.
    fn needs_reports_too(self) -> bool {
        matches!(self, Self::Worldgen)
    }

    /// Whether this domain reads the `--server` data pack tree.
    fn needs_data(self) -> bool {
        matches!(
            self,
            Self::Recipes | Self::Loot | Self::Tags | Self::Synced | Self::Worldgen
        )
    }
}

/// Parse an `--only` list into domains, refusing anything unknown.
///
/// Refusing rather than ignoring is the whole point of the flag's error case:
/// `--only recpies` that silently ran everything would be worse than no flag.
pub fn parse_only(list: &str) -> Result<Vec<Domain>, String> {
    let mut out = Vec::new();
    for name in list.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let domain = Domain::from_name(name).ok_or_else(|| {
            format!(
                "`{name}` is not a domain. The domains are: {}.",
                ALL_DOMAINS
                    .iter()
                    .map(|d| d.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if !out.contains(&domain) {
            out.push(domain);
        }
    }
    if out.is_empty() {
        return Err("--only names no domains".to_owned());
    }
    // Execution order regardless of the order the operator listed them in, so
    // output reads the same way whichever way the flag was spelled.
    out.sort_by_key(|d| ALL_DOMAINS.iter().position(|a| a == d).unwrap_or(0));
    Ok(out)
}

pub struct Options {
    pub version: String,
    /// A server jar the operator has already obtained, instead of downloading.
    pub server_jar: Option<PathBuf>,
    /// The domains to extract; every one of them when empty.
    pub only: Vec<Domain>,
}

/// Everything a domain needs, resolved once before any of them run.
struct Context<'a> {
    version: &'a str,
    workspace_root: &'a Path,
    /// The server jar itself. Almost everything is read from what the data
    /// generators *produced*, but the protocol number is not in any report —
    /// it lives in the jar's own `version.json`. See [`protocol_number`].
    jar: &'a Path,
    /// The `--reports` tree, once any report-reading domain has asked for it.
    reports: Option<PathBuf>,
    /// The `--server` data pack tree, likewise.
    data: Option<PathBuf>,
}

impl Context<'_> {
    fn reports(&self) -> Result<&Path, String> {
        self.reports.as_deref().ok_or_else(|| {
            "internal: a domain read the reports tree without asking for it".to_owned()
        })
    }

    fn data(&self) -> Result<&Path, String> {
        self.data.as_deref().ok_or_else(|| {
            "internal: a domain read the data pack tree without asking for it".to_owned()
        })
    }

    fn generated_registry_dir(&self) -> Result<PathBuf, String> {
        let dir = self
            .workspace_root
            .join("crates/dust-registry/src/generated");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        Ok(dir)
    }
}

/// Run the selected domains, printing one timed section per domain.
pub fn run(options: &Options, workspace_root: &Path) -> Result<(), String> {
    let started = Instant::now();
    let cache = workspace_root.join(CACHE_DIR);
    let version = &options.version;
    let selected = if options.only.is_empty() {
        ALL_DOMAINS.to_vec()
    } else {
        options.only.clone()
    };

    println!(
        "extracting {}: Minecraft {}",
        selected
            .iter()
            .map(|d| d.name())
            .collect::<Vec<_>>()
            .join(", "),
        version
    );

    let jar = match &options.server_jar {
        Some(path) => {
            if !path.exists() {
                return Err(format!("{} does not exist", path.display()));
            }
            // No digest to verify against: the jar did not come from Mojang's
            // manifest, and inventing one would be worse than saying so.
            println!(
                "using the server jar at {} (no SHA-1 check: it was not fetched through \
                 the manifest)",
                path.display()
            );
            path.clone()
        }
        None => download::server_jar(version, &cache)?,
    };

    let wants_reports = selected.iter().any(|d| d.needs_reports())
        || selected.iter().any(|d| d.needs_reports_too());
    let wants_data = selected.iter().any(|d| d.needs_data());

    let mut context = Context {
        version,
        workspace_root,
        jar: &jar,
        reports: None,
        data: None,
    };
    if wants_reports {
        context.reports = Some(generate(
            &jar,
            &cache.join(format!("reports-{version}")),
            "--reports",
            "reports/blocks.json",
            &cache,
            workspace_root,
        )?);
    }
    if wants_data {
        context.data = Some(generate(
            &jar,
            &cache.join(format!("data-{version}")),
            "--server",
            "data/minecraft/worldgen/placed_feature",
            &cache,
            workspace_root,
        )?);
    }

    // The two tables half the domains quote. Parsed once whether or not the
    // blocks domain itself was selected: parsing both reports costs
    // milliseconds, and every domain's cross-checks are worth more than the
    // branch it would take to skip them.
    // The datapack registries, for the same reason: the tags domain checks
    // five of its thirteen registries against these names, and a tags run that
    // could not is a tags run that would emit unchecked rows.
    let mut synced_registries = None;
    if wants_data {
        synced_registries = Some(synced::parse(context.data()?)?);
    }

    let mut blocks = None;
    let mut registries = None;
    if wants_reports {
        let registry_json = std::fs::read(context.reports()?.join("reports/registries.json"))
            .map_err(|e| format!("could not read the generated registry report: {e}"))?;
        let flat = registries::parse(&registry_json)?;
        let block_json = std::fs::read(context.reports()?.join("reports/blocks.json"))
            .map_err(|e| format!("could not read the generated block report: {e}"))?;
        let parsed = blocks::parse(&block_json)?;
        registries::check_block_ids_match_state_order(&flat, &parsed)?;
        blocks = Some(parsed);
        registries = Some(flat);
    }

    let mut results = Vec::new();
    for domain in &selected {
        println!("\n== {} ==", domain.name());
        let begun = Instant::now();
        let outcome = match domain {
            Domain::Blocks => blocks_domain(
                blocks.as_ref().expect("parsed above"),
                registries.as_ref().expect("parsed above"),
                &context,
            )?,
            Domain::Items => items_domain(
                blocks.as_ref().expect("parsed above"),
                registries.as_ref().expect("parsed above"),
                &context,
            )?,
            Domain::Entities => {
                entities_domain(registries.as_ref().expect("parsed above"), &context)?
            }
            Domain::Fluids => fluids_domain(
                blocks.as_ref().expect("parsed above"),
                registries.as_ref().expect("parsed above"),
                &context,
            )?,
            Domain::Commands => {
                commands_domain(registries.as_ref().expect("parsed above"), &context)?
            }
            Domain::Recipes => {
                recipes_domain(registries.as_ref().expect("parsed above"), &context)?
            }
            Domain::Loot => loot_domain(registries.as_ref().expect("parsed above"), &context)?,
            Domain::Tags => tags_domain(
                blocks.as_ref().expect("parsed above"),
                registries.as_ref().expect("parsed above"),
                synced_registries.as_ref().expect("parsed above"),
                &context,
            )?,
            Domain::Synced => synced_domain(&context)?,
            Domain::Packets => packets_domain(&context)?,
            Domain::Worldgen => {
                worldgen_domain(registries.as_ref().expect("parsed above"), &context)?
            }
            Domain::Mappings => mappings_domain(&context)?,
            Domain::Constants => constants_domain(&context)?,
        };
        println!("({} in {:.1}s)", outcome, begun.elapsed().as_secs_f64());
        results.push((*domain, begun.elapsed(), outcome));
    }

    println!(
        "\nall {} domains finished in {:.1}s",
        results.len(),
        started.elapsed().as_secs_f64()
    );
    for (domain, elapsed, outcome) in &results {
        println!(
            "  {:<10} {:>6.1}s  {}",
            domain.name(),
            elapsed.as_secs_f64(),
            outcome
        );
    }
    // Emitted unformatted on purpose: rustfmt is the one authority on how the
    // committed file looks, and a generator that lays code out itself will
    // disagree with it eventually.
    println!("\nRun `just fmt` — these are committed as rustfmt leaves them.");
    println!(
        "Then `just verify` — the round-trips over every state and registry entry and every \
         packet id, the golden samples beside them, and the ore baseline's source-row check \
         are what say this worked."
    );
    Ok(())
}

/// What one domain did, in a few words, for the timing table.
type Outcome = String;

fn blocks_domain(
    parsed: &blocks::Blocks,
    flat: &registries::Registries,
    context: &Context,
) -> Result<Outcome, String> {
    println!(
        "read {} blocks and {} states from the block report",
        parsed.blocks.len(),
        parsed.state_count
    );
    report_what_the_registries_said(flat);

    let generated = context.generated_registry_dir()?;
    let path = generated.join("blocks.rs");
    std::fs::write(
        &path,
        codegen::blocks(parsed, context.version, &parsed.reported),
    )
    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    let path = generated.join("registries.rs");
    std::fs::write(&path, codegen::registries(flat, context.version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} blocks, {} states, {} registries",
        parsed.blocks.len(),
        parsed.state_count,
        flat.registries.len()
    ))
}

/// Read the item report and regenerate the item-components table.
fn items_domain(
    parsed_blocks: &blocks::Blocks,
    flat: &registries::Registries,
    context: &Context,
) -> Result<Outcome, String> {
    let json = std::fs::read(context.reports()?.join("reports/items.json"))
        .map_err(|e| format!("could not read the generated item report: {e}"))?;
    let parsed = items::parse(&json, flat, parsed_blocks)?;
    report_what_the_items_said(&parsed);

    let path = context.generated_registry_dir()?.join("items.rs");
    std::fs::write(&path, codegen::items(&parsed, context.version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} items, {} distinct component maps",
        parsed.items.len(),
        parsed.maps.len()
    ))
}

/// Read the command report and regenerate the command-graph table.
///
/// The report's own tree is walked a second time for the golden rows, so the
/// numbers printed here are two readings of one file: the flattened table and
/// the samples that exist to contradict it if the flattening is wrong.
fn commands_domain(flat: &registries::Registries, context: &Context) -> Result<Outcome, String> {
    let json = std::fs::read(context.reports()?.join("reports/commands.json"))
        .map_err(|e| format!("could not read the generated command report: {e}"))?;
    let parsed = commands::parse(&json, flat)?;

    let literals = parsed
        .nodes
        .iter()
        .filter(|n| n.kind == commands::Kind::Literal)
        .count();
    println!(
        "read {} command nodes (1 root, {literals} literals, {} arguments) from the \
         command report",
        parsed.nodes.len(),
        parsed.nodes.len() - literals - 1
    );
    println!(
        "  {} of them can end a command; the deepest path from the root is {} levels",
        parsed.executable_count, parsed.max_depth
    );
    println!(
        "  {} redirects resolve to indices in this same table — 103 of them point back \
         into execute, which is why nothing here builds an owned tree",
        parsed.redirects.len()
    );
    println!(
        "  {} distinct parsers, every one an entry of the command_argument_type registry",
        parsed.parsers.len()
    );
    println!(
        "  every one of the {} numbers in the file is present by value in what was read",
        parsed.number_count
    );
    if parsed.unchecked_registries.is_empty() {
        println!("  every registry a resource argument names was in the registry report");
    } else {
        println!(
            "  registries named by resource arguments that live in the data pack and are \
             NOT checked here: {:?}",
            parsed.unchecked_registries
        );
    }
    let dead: Vec<&str> = parsed
        .unreachable
        .iter()
        .map(|&i| parsed.nodes[i].path.as_str())
        .collect();
    println!(
        "  nodes the report leaves dead (in the game they redirect to the root, which a \
         path cannot spell): {dead:?}"
    );

    let path = context.generated_registry_dir()?.join("commands.rs");
    std::fs::write(&path, codegen::commands(&parsed, context.version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} command nodes, {} redirects",
        parsed.nodes.len(),
        parsed.redirects.len()
    ))
}

/// Read the fluid registry against the block and item reports, and regenerate
/// the fluid-relationships table.
fn fluids_domain(
    parsed_blocks: &blocks::Blocks,
    flat: &registries::Registries,
    context: &Context,
) -> Result<Outcome, String> {
    let parsed = fluids::parse(flat, parsed_blocks)?;

    for fluid in &parsed.fluids {
        let mut facts = Vec::new();
        match (&fluid.block, &fluid.bucket) {
            (Some(block), Some(bucket)) => facts.push(format!("{block}, carried by {bucket}")),
            (Some(block), None) => facts.push(format!("held by {block}")),
            (None, None) => {}
            (None, Some(_)) => unreachable!("a bucket with no block is not derivable"),
        }
        if let Some(still) = &fluid.flowing_of {
            facts.push(format!("the movement of {still}"));
        }
        println!(
            "  {} at protocol id {}: {}",
            fluid.name,
            fluid.protocol_id,
            if facts.is_empty() {
                "no relationships the reports state".to_owned()
            } else {
                facts.join(", ")
            }
        );
    }

    let path = context.generated_registry_dir()?.join("fluids.rs");
    std::fs::write(&path, codegen::fluids(&parsed, context.version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} fluids joined against blocks and items",
        parsed.fluids.len()
    ))
}

/// Read the entity-type slice of the registry report and regenerate its
/// golden rows.
fn entities_domain(flat: &registries::Registries, context: &Context) -> Result<Outcome, String> {
    let parsed = entities::parse(flat)?;
    println!(
        "read {} entity types from the registry report; the default is {}",
        parsed.reported.len(),
        parsed.default.as_deref().unwrap_or("(none)")
    );
    println!(
        "  no per-entity facts exist in this version's generator output — bounding boxes, \
         spawn categories and friends are compiled into the game, so nothing here invents \
         them"
    );

    let path = context.generated_registry_dir()?.join("entity_types.rs");
    std::fs::write(&path, codegen::entities(&parsed, context.version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} entity types, all sampled",
        parsed.reported.len()
    ))
}

/// Walk the recipe data pack and regenerate the recipe-shape catalogue.
fn recipes_domain(flat: &registries::Registries, context: &Context) -> Result<Outcome, String> {
    let parsed = recipes::parse(&context.data()?.join("data"), flat)?;

    println!(
        "read {} recipe files from {} namespace(s) into {} shapes, each one an entry of \
         the recipe_serializer registry",
        parsed.total,
        parsed.namespaces.join(", "),
        parsed.shapes.len()
    );
    for shape in &parsed.shapes {
        let mut line = format!(
            "  {:<44} {:>4} recipes; keys {}",
            shape.serializer,
            shape.uses,
            shape.required.join(", ")
        );
        if !shape.optional.is_empty() {
            line.push_str(&format!(" | optional: {}", shape.optional.join(", ")));
        }
        println!("{line}");
    }
    if parsed.unused_serializers.is_empty() {
        println!("  every registered serialiser is exercised by the data");
    } else {
        println!(
            "  registered but unused by vanilla data (the special, computed recipes): {}",
            parsed.unused_serializers.join(", ")
        );
    }

    let path = context.generated_registry_dir()?.join("recipes.rs");
    std::fs::write(&path, codegen::recipes(&parsed, context.version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} recipe files into {} shapes",
        parsed.total,
        parsed.shapes.len()
    ))
}

/// Read the five tag directories and regenerate the baseline tag table.
fn tags_domain(
    parsed_blocks: &blocks::Blocks,
    flat: &registries::Registries,
    synced: &[synced::SyncedRegistry],
    context: &Context,
) -> Result<Outcome, String> {
    let parsed = tags::parse(&context.data()?.join("data"), flat, parsed_blocks, synced)?;

    let per_registry = |registry: &str| -> usize {
        parsed
            .tags
            .iter()
            .filter(|t| t.registry == registry)
            .count()
    };
    println!(
        "read {} tags across the thirteen registries:",
        parsed.tags.len()
    );
    for (registry, expected) in tags::CAPTURED {
        println!(
            "  {registry:<34} {:>4}   (a real 1.21.1 server sent {expected})",
            per_registry(registry)
        );
    }
    println!(
        "  every one of the {} plain memberships was checked against its registry's \
         extracted table",
        parsed.memberships - parsed.references
    );
    if parsed.duplicates_collapsed > 0 {
        println!(
            "  {} duplicate members collapsed — vanilla's own files repeat themselves, \
             and a tag is a set",
            parsed.duplicates_collapsed
        );
    }
    println!(
        "  all {} `#` references resolve inside this dataset; nothing dangles",
        parsed.references
    );
    if parsed.skipped_directories.is_empty() {
        println!("  no other tag directories were present");
    } else {
        println!(
            "  directories seen but NOT taken — no extracted table to check their \
             members against: {:?}",
            parsed.skipped_directories
        );
    }

    let path = context.generated_registry_dir()?.join("tags.rs");
    std::fs::write(&path, codegen::tags(&parsed, context.version)?)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} tags, {} memberships",
        parsed.tags.len(),
        parsed.memberships
    ))
}

/// Walk every loot table and regenerate the inventory plus the vocabulary.
fn loot_domain(flat: &registries::Registries, context: &Context) -> Result<Outcome, String> {
    let parsed = loot::parse(&context.data()?.join("data"), flat)?;

    println!(
        "read {} loot tables across {} categories from the data pack",
        parsed.tables.len(),
        parsed.categories.len()
    );
    for (category, count) in &parsed.categories {
        println!("  {category:<28} {count}");
    }
    let by_kind =
        |kind: loot::Kind| -> usize { parsed.vocabulary.iter().filter(|u| u.kind == kind).count() };
    println!(
        "  vocabulary in use: {} condition type(s), {} function type(s), {} entry \
         type(s), each checked against its registry in the report",
        by_kind(loot::Kind::Condition),
        by_kind(loot::Kind::Function),
        by_kind(loot::Kind::Entry)
    );
    println!(
        "  the same condition and function counts were re-tallied by a second pass that \
         reads strings without structure: {} rows, compared table-against-table by \
         dust-registry's tests",
        parsed.source.len()
    );

    let path = context.generated_registry_dir()?.join("loot.rs");
    std::fs::write(&path, codegen::loot(&parsed, context.version))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "{} tables, {} vocabulary items",
        parsed.tables.len(),
        parsed.vocabulary.len()
    ))
}

fn synced_domain(context: &Context) -> Result<Outcome, String> {
    let registries = synced::parse(context.data()?)?;
    let total: usize = registries.iter().map(|r| r.entries.len()).sum();
    for registry in &registries {
        println!(
            "  {:<32} {:>3} entries",
            registry.name,
            registry.entries.len()
        );
    }

    let generated = context
        .workspace_root
        .join("crates/dust-registry/src/generated");
    std::fs::create_dir_all(&generated)
        .map_err(|e| format!("could not create {}: {e}", generated.display()))?;
    let path = generated.join("synced.rs");
    std::fs::write(
        &path,
        codegen::synced_registries(&registries, context.version),
    )
    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    Ok(format!(
        "{} datapack registries, {total} entries, names only",
        registries.len()
    ))
}

/// Resolve the names an oracle needs and write them where the oracle looks.
///
/// # Why this writes a file instead of generating Rust
///
/// Every other domain ends in `crates/.../generated/`, because what it read is
/// a fact about the protocol or the format and may be committed. **These names
/// are Mojang's**, they change wholesale between versions, and they are of no
/// use to anything but a program running against that exact jar. So they land
/// in the extract cache beside the jar they describe, behind the same
/// `.gitignore` line, and the Java side reads them at run time.
///
/// That is also what keeps `xtask/oracle/` free of Minecraft identifiers: the
/// oracle asks for `blockstate.light_emission` and this decides what that is
/// called today. See decision record 0008.
fn mappings_domain(context: &Context) -> Result<Outcome, String> {
    let cache = context.workspace_root.join(CACHE_DIR);
    let out = mappings_table(context, &cache)?;
    let table = std::fs::read_to_string(&out)
        .map_err(|e| format!("could not read {}: {e}", out.display()))?;
    for line in table.lines().filter(|l| !l.starts_with('#')) {
        println!("  {line}");
    }
    Ok(format!(
        "{} name(s) resolved for the block oracle",
        mappings::LIGHT_ORACLE.len()
    ))
}

/// Resolve the oracle's names and write them, returning where they landed.
///
/// Shared by the `mappings` domain, which exists to do exactly this and print
/// it, and by `light`, which needs the file and should not require a reader to
/// have run the other domain first. **`--only constants` on a clean checkout
/// works**, and a domain that failed with "no names.properties" would be one
/// that only works for whoever already knew the order.
fn mappings_table(context: &Context, cache: &Path) -> Result<PathBuf, String> {
    let path = download::server_mappings(context.version, cache)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mappings = mappings::Mappings::parse(&text)?;
    if mappings.is_empty() {
        // A truncated or comment-only file parses cleanly and names nothing.
        // Left to `properties` it would report twelve missing entries, which
        // sends a reader looking for a renamed method in a file that has no
        // methods in it.
        return Err(format!(
            "{} parsed without error and named no classes at all; it is not a \
             mappings file, or it is a truncated one",
            path.display()
        ));
    }
    println!("  {} classes mapped", mappings.len());

    let table = mappings::properties(&mappings, mappings::LIGHT_ORACLE)?;
    let out_dir = cache.join(format!("oracle-{}", context.version));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
    let out = out_dir.join("names.properties");
    std::fs::write(&out, &table).map_err(|e| format!("could not write {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(out)
}

/// Ask the game what a block state's opacity and emission are.
///
/// # Why this runs Java instead of reading a file
///
/// Every other domain reads what Mojang's data generators produced. These two
/// numbers were never in that output, in any version: they are constants in
/// Java code, reachable only by asking the code. Decision record 0008 is the
/// standing account, and this is its option 1.
///
/// # The three steps, and the one that is not obvious
///
/// The jar is a *bundle*: Minecraft's classes and its libraries are jars
/// nested inside it, and a nested jar cannot go on a classpath. The bundler
/// unpacks them, and it has a mode that does only that —
/// `-DbundlerMainClass=` with an empty value prints "Empty main class
/// specified, exiting" and leaves `libraries/` and `versions/` behind. That is
/// step one; the alternative is a second implementation of the unpacking, and
/// Mojang's own is right here.
///
/// **What does not work, and it is worth writing down because it looks like it
/// should**: passing our class as `-DbundlerMainClass` and putting it on the
/// `-cp`. The bundler builds a `URLClassLoader` over the jars it unpacked and
/// nothing else, so the class it is told to start is not visible to it —
/// `ClassNotFoundException` on our own class, from a classpath that plainly
/// contains it.
///
/// So: unpack, then compile the oracle, then run it with a classpath we build.
fn constants_domain(context: &Context) -> Result<Outcome, String> {
    let cache = context.workspace_root.join(CACHE_DIR);
    let names = mappings_table(context, &cache)?;
    let jar = absolute(context.jar)?;

    let repo = cache.join(format!("oracle-{}/jvm", context.version));
    std::fs::create_dir_all(&repo)
        .map_err(|e| format!("could not create {}: {e}", repo.display()))?;

    // Step one. Cheap to repeat and safe to skip, but skipping it on a
    // half-unpacked directory would fail later with a missing class rather
    // than here with a missing jar, so it runs whenever the marker is absent.
    if !repo.join("versions").is_dir() {
        println!("unpacking the bundled jar (Mojang's own unpacker, main class empty)");
        run_java(
            &repo,
            &[
                "-DbundlerMainClass=".to_owned(),
                "-jar".to_owned(),
                jar.display().to_string(),
            ],
            "unpacking the server jar",
        )?;
    }

    // Step two.
    let classes = cache.join(format!("oracle-{}/classes", context.version));
    std::fs::create_dir_all(&classes)
        .map_err(|e| format!("could not create {}: {e}", classes.display()))?;
    let sources = context.workspace_root.join("xtask/oracle/dustoracle");
    let mut javac: Vec<String> = vec!["-d".to_owned(), classes.display().to_string()];
    for name in ["Names.java", "BlockOracle.java"] {
        javac.push(sources.join(name).display().to_string());
    }
    println!("compiling the oracle");
    run_tool("javac", &repo, &javac, "compiling the oracle")?;

    // Step three.
    let mut classpath = jars_under(&repo)?;
    classpath.push(classes.clone());
    let joined = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":");
    let out = cache.join(format!("oracle-{}/constants.tsv", context.version));
    println!(
        "asking Minecraft what every block state does to light, to a heightmap, and sounds like"
    );
    run_java(
        &repo,
        &[
            "-cp".to_owned(),
            joined,
            "dustoracle.BlockOracle".to_owned(),
            names.display().to_string(),
            out.display().to_string(),
        ],
        "running the block oracle",
    )?;

    let table = std::fs::read_to_string(&out)
        .map_err(|e| format!("could not read {}: {e}", out.display()))?;
    let rows = table.lines().filter(|l| !l.starts_with('#')).count();
    if rows == 0 {
        return Err(format!(
            "{} holds no states; the oracle ran and found nothing, which means \
             `Bootstrap` did not populate the registry",
            out.display()
        ));
    }
    println!("wrote {}", out.display());

    let summary = ConstantsSummary::of(&table);
    print!("  opacity:");
    for (value, count) in &summary.opacity {
        print!(" {value}={count}");
    }
    println!();
    println!("  {} state(s) emit light", summary.emitting);
    // The heightmap predicates, one line each. A count per heightmap is what
    // says the columns are the six Minecraft declares and are not six copies
    // of one predicate — the failure a single "6 heightmaps" line would pass.
    for (name, count) in &summary.heightmaps {
        println!("  {name}: {count} state(s)");
    }
    if summary.heightmaps.is_empty() {
        return Err(format!(
            "{} carries no heightmap columns; the oracle resolved \
             `heightmap_types.class` to something with no enum constants",
            out.display()
        ));
    }
    // The sound groups, as a count and the three widest. A hundred and nine
    // lines is not a summary; a count of one would be the whole story.
    println!("  {} distinct block sound group(s)", summary.sounds.len());
    let mut widest: Vec<(&String, &u64)> = summary.sounds.iter().collect();
    widest.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    for (name, count) in widest.iter().take(3) {
        println!("    {name}: {count} state(s)");
    }
    if summary.sounds.len() == 1 {
        return Err(format!(
            "{} gives every block state the same sound group, which no version \
             of Minecraft does; a `soundtype.*` name resolved to the wrong \
             member. Check {}/names.properties against this version's mappings.",
            out.display(),
            out.parent().unwrap_or(&out).display()
        ));
    }
    // The route, printed where somebody who just ran this is looking. The
    // table is read from `[data] path` — decision record 0008 chose that over a
    // new `dust` subcommand, a JDK on the server's boot path and a second
    // release artefact — and a copy is what puts it there. One line, so that
    // "how do I use this" is answered at the moment the question occurs rather
    // than in a document somewhere.
    println!();
    println!("  to serve a world lit the way Minecraft lights it, copy it beside your data:");
    println!("    cp {} <[data] path>/dust-constants.tsv", out.display());

    // Every light level Minecraft has is 0..=15, so a value outside that did
    // not come from the field or method this asked for — it came from a
    // different member that shares an obfuscated letter. That is the exact
    // failure the parameter-keyed lookup in `mappings` exists to prevent, and
    // this is the place it would show up if it ever got through: a table full
    // of plausible-looking integers that are something else entirely.
    if let Some(bad) = summary.opacity.keys().find(|v| **v > 15) {
        return Err(format!(
            "the oracle reported an opacity of {bad}, and light levels are 0..=15.              Something resolved to the wrong member; check the names in              {}/names.properties against this version's mappings.",
            out.parent().unwrap_or(&out).display()
        ));
    }

    Ok(format!(
        "{rows} block states: light, occlusion, {} heightmap(s) and {} sound group(s)",
        summary.heightmaps.len(),
        summary.sounds.len()
    ))
}

/// The distributions in an extracted constants table.
#[derive(Debug, Default, PartialEq, Eq)]
struct ConstantsSummary {
    /// How many states take each opacity. **1.21.1 gives exactly three values
    /// — 0, 1 and 15** — and printing the distribution rather than a total is
    /// what lets a reader see that at a glance, and see it change.
    opacity: std::collections::BTreeMap<u32, u64>,
    /// How many states emit any light at all.
    emitting: u64,
    /// How many states each heightmap counts, by the name in the header.
    ///
    /// **A count per heightmap and not a count of heightmaps.** Six columns
    /// holding six copies of one predicate would print as "6 heightmaps" and
    /// be wrong in a way nothing else here would notice; six different numbers
    /// are six different predicates.
    heightmaps: std::collections::BTreeMap<String, u64>,
    /// How many states take each distinct `(place sound, volume, pitch)`.
    ///
    /// Kept for the same reason and against the same failure: a `SoundType`
    /// field that resolved to the wrong member answers one thing for every
    /// state, and one group covering twenty-six thousand of them is a line a
    /// reader can see. Only the count of groups and the largest few are
    /// printed — a hundred and nine lines is not a summary.
    sounds: std::collections::BTreeMap<String, u64>,
}

impl ConstantsSummary {
    /// Read a table the oracle wrote.
    ///
    /// Malformed rows are skipped rather than refused: this is a summary for a
    /// human, and the row count beside it — taken from the same file by the
    /// caller — is what would disagree if rows were being lost. The flag
    /// columns are found by name out of the header, exactly as
    /// `dust_registry::constants` finds them, because a summary that read them
    /// positionally could report a column the reader will not.
    fn of(table: &str) -> Self {
        let mut summary = Self::default();
        let header: Vec<&str> = table
            .lines()
            .find(|line| line.starts_with('#'))
            .map(|line| {
                line.trim_start_matches('#')
                    .split('\t')
                    .map(str::trim)
                    .collect()
            })
            .unwrap_or_default();
        let named = |wanted: &str| header.iter().position(|name| *name == wanted);
        // Everything that is not one of the named columns. The list has to
        // match `dust_registry::constants`'s or this prints a sound name as a
        // heightmap that counts no states, which is what it did the first time.
        let flags: Vec<(&str, usize)> = header
            .iter()
            .enumerate()
            .filter(|(_, name)| {
                !matches!(
                    **name,
                    "state_id"
                        | "opacity"
                        | "emission"
                        | "occlude"
                        | "place_sound"
                        | "sound_volume"
                        | "sound_pitch"
                )
            })
            .map(|(at, name)| (*name, at))
            .collect();

        for line in table.lines().filter(|l| !l.starts_with('#')) {
            let fields: Vec<&str> = line.split('\t').collect();
            let at = |column: Option<usize>| column.and_then(|c| fields.get(c)).copied();
            let (Some(opacity), Some(emission)) = (
                at(named("opacity").or(Some(1))),
                at(named("emission").or(Some(2))),
            ) else {
                continue;
            };
            if let Ok(opacity) = opacity.parse::<u32>() {
                *summary.opacity.entry(opacity).or_default() += 1;
            }
            if emission.parse::<u32>().is_ok_and(|e| e > 0) {
                summary.emitting += 1;
            }
            for (name, column) in &flags {
                let entry = summary.heightmaps.entry((*name).to_owned()).or_default();
                if fields.get(*column).copied() == Some("1") {
                    *entry += 1;
                }
            }
            if let (Some(sound), Some(volume), Some(pitch)) = (
                at(named("place_sound")),
                at(named("sound_volume")),
                at(named("sound_pitch")),
            ) {
                *summary
                    .sounds
                    .entry(format!("{sound} at {volume}/{pitch}"))
                    .or_default() += 1;
            }
        }
        summary
    }
}

/// Every `.jar` under a directory, in a stable order.
///
/// A walk rather than a read of the bundle's own `classpath-joined`, because
/// every jar the bundler unpacked belongs on the classpath and the order
/// between libraries does not matter here. Sorted so two runs build the same
/// string.
fn jars_under(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("could not read {}: {e}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jar") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Run `java` in `dir`, failing with a message that names what was being done.
fn run_java(dir: &Path, args: &[String], doing: &str) -> Result<(), String> {
    run_tool("java", dir, args, doing)
}

fn run_tool(tool: &str, dir: &Path, args: &[String], doing: &str) -> Result<(), String> {
    let status = std::process::Command::new(tool)
        .current_dir(dir)
        .args(args)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("{tool} was not found on PATH, and {doing} needs a JDK of 21 or newer.")
            }
            _ => format!("could not run {tool}: {e}"),
        })?;
    if !status.success() {
        return Err(format!(
            "{doing} failed ({status}). If the message above mentions a class version, the \
             JDK on PATH is older than 21."
        ));
    }
    Ok(())
}

fn packets_domain(context: &Context) -> Result<Outcome, String> {
    extract_packets(
        context.reports()?,
        context.jar,
        context.version,
        context.workspace_root,
    )?;
    Ok("packet tables regenerated".to_owned())
}

fn worldgen_domain(flat: &registries::Registries, context: &Context) -> Result<Outcome, String> {
    ores(
        &context.data()?.join("data"),
        context.workspace_root,
        context.version,
    )?;

    let parsed = worldgen::vocabulary(
        &context.data()?.join("data/minecraft"),
        &context.reports()?.join("reports"),
        flat,
    )?;
    println!(
        "read the Phase 6 vocabulary: {} density-function type(s) in use, {} noise-router \
         slots identical across every noise setting, biome parameters for {} \
         dimension(s)",
        parsed.density_function_types.len(),
        parsed.noise_router_slots.len(),
        parsed.dimensions.len()
    );
    for dimension in &parsed.dimensions {
        println!(
            "  {}: {} entries over {} biomes, {} range-shaped",
            dimension.dimension,
            dimension.entries,
            dimension.distinct_biomes,
            dimension.ranged_entries
        );
    }
    println!(
        "  the overworld's parameter expansion stays out on purpose — it is world data a \
         server reads from its packs, not a constant to freeze"
    );

    let path = context
        .workspace_root
        .join("crates/dust-gen/src/generated")
        .join("worldgen.rs");
    std::fs::create_dir_all(path.parent().expect("has a parent"))
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    std::fs::write(
        &path,
        codegen::worldgen_vocabulary(&parsed, context.version),
    )
    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(format!(
        "ore baseline + vocabulary over {} density-function types",
        parsed.density_function_types.len()
    ))
}

/// Print what the item report turned out to say.
fn report_what_the_items_said(items: &items::Items) {
    println!(
        "read {} items and {} distinct component maps from the item report",
        items.items.len(),
        items.maps.len()
    );
    println!(
        "  every one of the {} numbers in the file is present by value in what was read, so \
         nothing is being stored at the wrong width",
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

/// Read the packet report and regenerate `dust-protocol`'s tables for this
/// version.
///
/// One version at a time, and the others stay where they are: each version's
/// table is its own module, and the index that lists them is rebuilt from the
/// modules on disk. Running this for 1.21.4 later adds a file and a row rather
/// than replacing 1.21.1.
fn extract_packets(
    reports: &Path,
    jar: &Path,
    version: &str,
    workspace_root: &Path,
) -> Result<(), String> {
    let json = std::fs::read(reports.join("reports/packets.json"))
        .map_err(|e| format!("could not read the generated packet report: {e}"))?;
    let parsed = packets::parse(&json)?;
    let protocol = protocol_number(jar)?;
    println!("the jar declares protocol number {protocol}");

    println!("read {} packets from the packet report:", parsed.total);
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
    std::fs::write(&path, codegen::packets(&parsed, version, protocol))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());

    let path = generated.join("packets.rs");
    std::fs::write(&path, codegen::packet_index(&version_modules(&versions)?))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The protocol number this jar speaks, from its own `version.json`.
///
/// Not derivable from anything else in the extraction. The packet report names
/// packets and ids; it never says which number a client puts in a handshake,
/// and that number is the one the whole connection turns on. Guessing it from
/// the Minecraft version id is not possible either — the two are unrelated
/// sequences, and several Minecraft releases share one protocol number.
///
/// Read through `dust-data`'s zip reader rather than a second one written here:
/// it is the archive reader this workspace already hardened, and a jar is a zip
/// like any other. The refusal is loud because a missing or unreadable
/// `version.json` means this is not the jar anybody thinks it is.
fn protocol_number(jar: &Path) -> Result<i32, String> {
    let bytes = std::fs::read(jar).map_err(|e| format!("could not read {}: {e}", jar.display()))?;
    let archive = dust_data::zip::ZipArchive::open(bytes)
        .map_err(|e| format!("{} is not a readable archive: {e}", jar.display()))?;
    let entry = archive
        .entries()
        .iter()
        .find(|entry| entry.name == "version.json")
        .ok_or_else(|| {
            format!(
                "{} has no version.json; a Minecraft server jar always does, so this is \
                 not one",
                jar.display()
            )
        })?;
    let raw = archive
        .read(entry)
        .map_err(|e| format!("could not read version.json out of {}: {e}", jar.display()))?;
    let parsed: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| format!("version.json in {} is not JSON: {e}", jar.display()))?;
    let number = parsed
        .get("protocol_version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            format!(
                "version.json in {} has no numeric protocol_version",
                jar.display()
            )
        })?;
    i32::try_from(number).map_err(|_| format!("protocol_version {number} does not fit in an i32"))
}

/// Read the vanilla ore baseline out of the `--server` tree and write it into
/// `dust-gen`.
fn ores(data_root: &Path, workspace_root: &Path, version: &str) -> Result<(), String> {
    let ores = worldgen::parse(data_root)?;
    println!(
        "read {} ore placements in {} group(s) across {} dimension(s)",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows in the shape the oracle writes them. Not read from a captured
    /// file: what the oracle produces is Mojang's data, and nothing of theirs
    /// is committed — the same rule the harness's own readers are tested
    /// under.
    const TABLE: &str = "\
# state_id\topacity\temission\tocclude
0\t0\t0\t0
1\t15\t0\t1
2\t1\t0\t0
3\t0\t14\t0
4\t15\t15\t1
";

    #[test]
    fn a_constants_table_summarises_to_its_distributions() {
        let summary = ConstantsSummary::of(TABLE);
        assert_eq!(
            summary.opacity,
            [(0, 2), (1, 1), (15, 2)].into_iter().collect()
        );
        assert_eq!(summary.emitting, 2, "only the two with a non-zero emission");
    }

    #[test]
    fn the_header_is_not_a_state() {
        // It begins with `#`, like every comment in the files this project
        // writes. Counting it would put a state at opacity `opacity`, which
        // parses as nothing and would quietly vanish instead — a row lost
        // rather than a row wrong, which is harder to notice.
        assert_eq!(ConstantsSummary::of(TABLE).opacity.values().sum::<u64>(), 5);
    }

    /// The same shape with the sound columns the oracle now writes. Two
    /// groups, so a summary that collapsed them onto one would fail here
    /// rather than in the check the extractor prints.
    const SOUNDING_TABLE: &str = "\
# state_id	opacity	emission	occlude	place_sound	sound_volume	sound_pitch	MOTION_BLOCKING
0	0	0	0	minecraft:block.stone.place	1.0	1.0	0
1	15	0	1	minecraft:block.stone.place	1.0	1.0	1
2	1	0	0	minecraft:block.wool.place	1.0	1.0	1
";

    #[test]
    fn the_sound_columns_are_summarised_and_are_not_counted_as_heightmaps() {
        // The failure this catches is the one it was written after: the flag
        // list is "every column that is not named", so a summary that did not
        // learn these three names printed `place_sound: 0 state(s)` beside the
        // six real heightmaps and read as a seventh predicate that counts
        // nothing.
        let summary = ConstantsSummary::of(SOUNDING_TABLE);
        assert_eq!(
            summary.heightmaps.keys().collect::<Vec<_>>(),
            vec!["MOTION_BLOCKING"]
        );
        assert_eq!(summary.sounds.len(), 2);
        assert_eq!(
            summary.sounds.get("minecraft:block.stone.place at 1.0/1.0"),
            Some(&2)
        );
    }

    #[test]
    fn a_table_without_the_sound_columns_summarises_no_groups() {
        assert!(ConstantsSummary::of(TABLE).sounds.is_empty());
    }

    #[test]
    fn a_truncated_row_is_skipped_rather_than_guessed_at() {
        let summary =
            ConstantsSummary::of("# state_id\topacity\temission\tocclude\n0\t0\n1\t15\t0\t1\n");
        assert_eq!(summary.opacity, [(15, 1)].into_iter().collect());
    }

    #[test]
    fn jars_are_found_at_any_depth_and_in_a_stable_order() {
        // The bundler nests libraries several directories deep, so a
        // single-level read would build a classpath holding Minecraft and none
        // of what it needs — which fails at run time inside a class the stack
        // trace names by a letter.
        let dir = std::env::temp_dir().join(format!("dust-jars-{}", std::process::id()));
        let deep = dir.join("libraries/org/slf4j/slf4j-api/2.0.9");
        std::fs::create_dir_all(&deep).expect("temp dirs");
        std::fs::create_dir_all(dir.join("versions/1.21.1")).expect("temp dirs");
        std::fs::write(deep.join("slf4j-api-2.0.9.jar"), b"").expect("writable");
        std::fs::write(dir.join("versions/1.21.1/server-1.21.1.jar"), b"").expect("writable");
        std::fs::write(dir.join("not-a-jar.txt"), b"").expect("writable");

        let found = jars_under(&dir).expect("readable");
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.file_name()
                    .expect("a file")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // Sorted by full path, so `libraries/` comes before `versions/`. What
        // matters is that it is the same on every run: an unsorted walk returns
        // directory order, which is the file system's business and not
        // repeatable across machines.
        assert_eq!(names, ["slf4j-api-2.0.9.jar", "server-1.21.1.jar"]);
        assert_eq!(jars_under(&dir).expect("readable"), found, "twice the same");
        assert!(
            !names.iter().any(|n| n.ends_with(".txt")),
            "only jars belong on a classpath"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
