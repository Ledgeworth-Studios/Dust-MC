//! `harness registries` — does Dust tell a client what Minecraft tells it?
//!
//! # The question, and why it needs two servers
//!
//! A client that acknowledges no data packs is sent the *contents* of the
//! synced registries and the whole tag set. Dust builds both — the registry
//! contents from a schema over the operator's data, the tags from a flattening
//! walk over the extracted baseline — and every test either of those has is a
//! test written from the same understanding as the code. Such a test agrees
//! with the code, not with Minecraft.
//!
//! So this boots a real 1.21.1 server, asks it, boots Dust, asks it the same
//! way, and compares. One client, written by hand in [`super::wire`], used
//! against both: whatever it gets wrong it gets wrong twice, and a difference
//! it reports is a difference between the servers.
//!
//! # What is compared, and what is deliberately not
//!
//! Registries are compared **as trees** and tags **as sets**, not as bytes.
//! Vanilla writes an NBT compound in its own map's order and emits tags in
//! that map's order too; Dust writes both sorted. The client builds a map and
//! a set either way, so an ordering difference is not a difference the client
//! can see — and demanding byte equality would fail on a distinction with no
//! consequence while saying nothing extra about the ones that have.
//!
//! What *is* compared exactly: which registries were sent, which entries each
//! held and in what order (an entry's position is its id, so order is content
//! here), whether each entry carried data, every key and value inside that
//! data, which tag registries were sent, which tags each held, and the exact
//! set of ids in every tag.
//!
//! # Exit codes
//!
//! `0` if the two agree, `1` if they do not, `2` if the run itself failed.
//! Like `compare` and `rewrite`, a difference is a finding to be read rather
//! than an error to report in one line with the table thrown away.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{cache, capture, nbt, properties, rcon, wire};

const USAGE: &str = "\
harness registries --version <v> [--data <dir>] [--timeout <secs>]

Boots Minecraft, boots Dust, and asks both what they tell a client that
acknowledges no data packs. Compares the synced registries and the tag set.

  --version <v>   Minecraft version, e.g. 1.21.1. A provisioned server for it
                  must already exist; run `harness provision` first.
  --data <dir>    The directory Dust reads registry contents from — the one
                  holding `minecraft/`. Defaults to the extractor's own
                  unpacked data for this version.
  --timeout <s>   Whole-run budget. Default 300.
";

/// One run's inputs.
#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub data: Option<PathBuf>,
    pub timeout: Duration,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut version = None;
    let mut data = None;
    let mut timeout = Duration::from_secs(300);
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--version" => {
                at = super::take_value(&mut seen, "--version", args, at + 1)?;
                version = Some(seen.last().expect("just stored").1.clone());
            }
            "--data" => {
                at = super::take_value(&mut seen, "--data", args, at + 1)?;
                data = Some(PathBuf::from(seen.last().expect("just stored").1.clone()));
            }
            "--timeout" => {
                at = super::take_value(&mut seen, "--timeout", args, at + 1)?;
                timeout = Duration::from_secs(
                    seen.last()
                        .expect("just stored")
                        .1
                        .parse()
                        .map_err(|_| "--timeout needs seconds")?,
                );
            }
            other => return Err(format!("unknown registries option `{other}`\n\n{USAGE}")),
        }
    }
    Ok(Options {
        version: version.ok_or_else(|| {
            format!("registries needs --version, e.g. `--version 1.21.1`\n\n{USAGE}")
        })?,
        data,
        timeout,
    })
}

pub fn run(options: &Options) -> ExitCode {
    match compare(options) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("harness registries: {e}");
            ExitCode::from(2)
        }
    }
}

fn compare(options: &Options) -> Result<bool, String> {
    let deadline = Instant::now() + options.timeout;
    let dirs = cache::Layout::resolve()?;
    let jar = dirs.jars.join(format!("server-{}.jar", options.version));
    if !jar.is_file() {
        return Err(format!(
            "no server jar at {}; run `cargo xtask harness provision --version {}` first",
            jar.display(),
            options.version
        ));
    }
    let run_dir = dirs.server_dir(&options.version, 0);
    if !run_dir.is_dir() {
        return Err(format!(
            "no provisioned server at {}; run `cargo xtask harness provision --version {}` \
             first",
            run_dir.display(),
            options.version
        ));
    }

    println!("== Minecraft {} ==", options.version);
    let vanilla = ask_vanilla(&jar, &run_dir, deadline)?;
    report("Minecraft", &vanilla);

    println!("\n== Dust ==");
    let data = match &options.data {
        Some(path) => path.clone(),
        None => default_data_dir(&options.version)?,
    };
    let dust = ask_dust(&data, deadline)?;
    report("Dust", &dust);

    // What Dust declares it cannot serve, before anything is compared. This
    // is not a difference: a registry with no schema is one the server says,
    // in code, that it will not send — and the alternative to saying so is
    // sending a list of names to a client with no definitions for them, which
    // is the failure the whole design avoids.
    let declared: Vec<&str> = vanilla
        .registries
        .iter()
        .map(|r| r.name.as_str())
        .filter(|name| dust_server::registries::schema::by_name(name).is_none())
        .collect();
    if !declared.is_empty() {
        println!("\n== stated omissions ==");
        for name in &declared {
            println!("  {name}: Dust has no schema for it and does not send it");
        }
    }

    println!("\n== differences ==");
    let findings = differences(&vanilla, &dust);
    if findings.is_empty() {
        let ids: usize = dust
            .tags
            .iter()
            .flatten()
            .flat_map(|r| &r.tags)
            .map(|(_, ids)| ids.len())
            .sum();
        println!(
            "none. {} registries agree entry for entry and field for field, and {} tag \n\
             registries agree over {ids} ids.",
            dust.registries.len(),
            dust.tags.as_ref().map_or(0, Vec::len),
        );
        return Ok(true);
    }
    for finding in &findings {
        println!("  {finding}");
    }
    println!("\n{} difference(s).", findings.len());
    Ok(false)
}

/// The extractor's unpacked data for a version, which is where a developer's
/// copy of Minecraft's data already is.
///
/// From the workspace root and not from the harness cache: the two are
/// different caches on purpose — `.dust-extract/` holds what the extractor
/// unpacked from a jar, and the harness cache holds servers it ran.
fn default_data_dir(version: &str) -> Result<PathBuf, String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "xtask lives one level below the workspace root".to_owned())?;
    let data = workspace
        .join(".dust-extract")
        .join(format!("data-{version}"))
        .join("data");
    if !data.join("minecraft").is_dir() {
        return Err(format!(
            "no unpacked data at {}; run `cargo xtask extract --version {version} --only synced` \
             first, or pass --data",
            data.display()
        ));
    }
    Ok(data)
}

/// Boot vanilla, ask it, stop it.
fn ask_vanilla(jar: &Path, dir: &Path, deadline: Instant) -> Result<wire::Configuration, String> {
    let ports = BorrowedPorts::take(dir)?;
    println!("  on port {} (rcon {})", ports.game, ports.rcon);
    let (program, args) = capture::java_command(jar);
    let mut child = std::process::Command::new(program)
        .args(&args)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start java: {e}"))?;

    // Both pipes drained from here on: a pipe nobody reads fills after a few
    // kilobytes and the server blocks mid-write, which looks like a hang.
    let (tx, rx) = mpsc::channel::<String>();
    let transcript = Arc::new(Mutex::new(Vec::<String>::new()));
    for stream in [
        Box::new(child.stdout.take().expect("piped")) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().expect("piped")),
    ] {
        let tx = tx.clone();
        let transcript = Arc::clone(&transcript);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                if let Ok(mut lines) = transcript.lock() {
                    lines.push(line.clone());
                }
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
    }
    drop(tx);

    let outcome = (|| {
        loop {
            if Instant::now() >= deadline {
                return Err("vanilla did not finish starting inside the budget".to_owned());
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    println!("vanilla | {line}");
                    if capture::startup_complete(&line) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("vanilla ended before it was ready".to_owned())
                }
            }
        }
        wire::configuration_of(
            SocketAddr::from(([127, 0, 0, 1], ports.game)),
            "Compare",
            Duration::from_secs(30),
        )
    })();

    // Whatever happened, the server does not outlive the command.
    let _ = stop_over_rcon(ports.rcon, deadline);
    if capture::wait_for_exit(&mut child, deadline).is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    while rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
    outcome
}

/// Boot Dust in this process, ask it, stop it.
///
/// In process rather than as a subprocess: `xtask` already depends on
/// `dust-server` for `harness rewrite`, and a subprocess would need a build of
/// the binary to exist and would answer a slightly different question — "does
/// the `dust` on disk agree" rather than "does this tree agree".
fn ask_dust(data: &Path, deadline: Instant) -> Result<wire::Configuration, String> {
    let port = free_port()?;
    let dir = std::env::temp_dir().join(format!("dust-harness-registries-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let config_path = dir.join("dust.toml");
    std::fs::write(
        &config_path,
        format!(
            "[server]\nbind = \"127.0.0.1:{port}\"\nonline_mode = false\n\n\
             [jvm]\nenabled = false\n\n[data]\npath = {:?}\n",
            data.display().to_string()
        ),
    )
    .map_err(|e| format!("could not write {}: {e}", config_path.display()))?;

    let options = dust_server::ServerOptions {
        config_path,
        world_dir: dir.join("world"),
        ..dust_server::ServerOptions::default()
    };
    let server = dust_server::Server::new(options);
    let stop = server.stop_handle();
    let metrics = server.metrics();
    let worker = std::thread::spawn(move || server.run());

    let outcome = (|| {
        // Wait for the listener, watching the worker: a server that failed to
        // start ends its thread, and waiting on a port it will never bind is
        // a timeout that names nothing.
        let address: SocketAddr = loop {
            if let Some(bound) = metrics.bound_addr() {
                break bound;
            }
            if worker.is_finished() {
                return Err("Dust stopped before it bound a listener".to_owned());
            }
            if Instant::now() >= deadline {
                return Err("Dust did not bind a listener inside the budget".to_owned());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        wire::configuration_of(address, "Compare", Duration::from_secs(30))
    })();

    stop.request_stop();
    let ended = worker.join();
    let _ = std::fs::remove_dir_all(&dir);
    match ended {
        Ok(Ok(_)) | Err(_) => outcome,
        Ok(Err(e)) => Err(format!("Dust failed: {e}")),
    }
}

/// Point a provisioned run directory at free ports for one run, and put the
/// file back afterwards.
///
/// **The provisioned `server.properties` names 25565 and 25575, and this
/// command must not take them.** A developer's machine runs a Minecraft server
/// or a container on 25565 more often than not, and two `harness registries`
/// runs on one machine would collide with each other. The failure is a JVM
/// that exits during boot with a line that scrolls past, which is why the same
/// mistake is worth fixing here rather than detecting.
///
/// The file is restored on the way out, whatever happened, because `capture`
/// reads the same directory and expects the provisioned values.
struct BorrowedPorts {
    path: PathBuf,
    original: String,
    game: u16,
    rcon: u16,
}

impl BorrowedPorts {
    fn take(server_dir: &Path) -> Result<Self, String> {
        let path = server_dir.join("server.properties");
        let original = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let game = free_port()?;
        let rcon = free_port()?;
        let rewritten = original
            .lines()
            .map(|line| match line.split('=').next().map(str::trim) {
                Some("server-port") => format!("server-port={game}"),
                Some("rcon.port") => format!("rcon.port={rcon}"),
                _ => line.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{rewritten}\n"))
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        Ok(Self {
            path,
            original,
            game,
            rcon,
        })
    }
}

impl Drop for BorrowedPorts {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.original);
    }
}

/// Ask a server to stop, over RCON, retrying while its listener comes up.
///
/// Vanilla binds RCON late in boot, in some orderings after the readiness
/// line, so a single attempt right after startup races the socket.
fn stop_over_rcon(port: u16, deadline: Instant) -> Result<(), String> {
    loop {
        let attempt = rcon::Client::connect(
            SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(5),
        )
        .and_then(|mut client| {
            client.authenticate(properties::RCON_PASSWORD)?;
            client.send_and_move_on("stop")
        });
        match attempt {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A port the operating system says is free.
///
/// Bound and immediately dropped, which is a race — something else may take it
/// in the gap. It is the same race every test in this workspace runs and it
/// has not been lost yet; the alternative is a fixed port, which loses the
/// same race against a Minecraft server on the same machine every single time.
fn free_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not find a free port: {e}"))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("could not read the bound port: {e}"))
}

fn report(who: &str, configuration: &wire::Configuration) {
    for registry in &configuration.registries {
        let with_data = registry.entries.iter().filter(|e| e.data.is_some()).count();
        println!(
            "  {:<34} {:>4} entries, {:>4} with contents",
            registry.name,
            registry.entries.len(),
            with_data
        );
    }
    match &configuration.tags {
        None => println!("  {who} sent no tags"),
        Some(tags) => {
            let total: usize = tags.iter().map(|r| r.tags.len()).sum();
            let ids: usize = tags
                .iter()
                .flat_map(|r| &r.tags)
                .map(|(_, ids)| ids.len())
                .sum();
            println!("  tags: {} registries, {total} tags, {ids} ids", tags.len());
        }
    }
}

/// Every way the two answers differ, in the order a reader would want them.
fn differences(vanilla: &wire::Configuration, dust: &wire::Configuration) -> Vec<String> {
    let mut out = Vec::new();

    let van: BTreeMap<&str, &wire::Registry> = vanilla
        .registries
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    let ours: BTreeMap<&str, &wire::Registry> = dust
        .registries
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();

    for name in van.keys() {
        if ours.contains_key(name) {
            continue;
        }
        // A registry Dust has no schema for is one it says it cannot serve —
        // and saying so is the rule "all of a registry or none of it", not a
        // gap in the comparison. It is reported as a note above rather than as
        // a difference here. Add it to `schema::SERVED` and get it wrong, and
        // this is red.
        if dust_server::registries::schema::by_name(name).is_none() {
            continue;
        }
        out.push(format!("Dust has a schema for {name} and did not send it"));
    }
    for name in ours.keys() {
        if !van.contains_key(name) {
            out.push(format!("Dust sent {name}, which Minecraft did not"));
        }
    }

    for (name, expected) in &van {
        let Some(found) = ours.get(name) else {
            continue;
        };
        // Entry order is content: a position in this list is the id every
        // later packet uses for that entry.
        let expected_names: Vec<&str> = expected.entries.iter().map(|e| e.name.as_str()).collect();
        let found_names: Vec<&str> = found.entries.iter().map(|e| e.name.as_str()).collect();
        if expected_names != found_names {
            let missing: Vec<&&str> = expected_names
                .iter()
                .filter(|n| !found_names.contains(n))
                .collect();
            let extra: Vec<&&str> = found_names
                .iter()
                .filter(|n| !expected_names.contains(n))
                .collect();
            if missing.is_empty() && extra.is_empty() {
                out.push(format!("{name}: the same entries in a different order"));
            } else {
                out.push(format!(
                    "{name}: {} missing {missing:?}, {} unexpected {extra:?}",
                    missing.len(),
                    extra.len()
                ));
            }
            continue;
        }
        for (want, got) in expected.entries.iter().zip(&found.entries) {
            match (&want.data, &got.data) {
                (None, None) => {}
                (Some(_), None) => {
                    out.push(format!(
                        "{name}/{}: Minecraft sent contents, Dust did not",
                        want.name
                    ));
                }
                (None, Some(_)) => {
                    out.push(format!(
                        "{name}/{}: Dust sent contents, Minecraft did not",
                        want.name
                    ));
                }
                (Some(a), Some(b)) => {
                    let mut inner = Vec::new();
                    compare_nodes(a, b, "", &mut inner);
                    for line in inner {
                        out.push(format!("{name}/{}: {line}", want.name));
                    }
                }
            }
        }
    }

    match (&vanilla.tags, &dust.tags) {
        (Some(_), None) => out.push("Dust sent no tags at all".to_owned()),
        (None, Some(_)) => out.push("Dust sent tags and Minecraft did not".to_owned()),
        (None, None) => {}
        (Some(expected), Some(found)) => {
            let van: BTreeMap<&str, &wire::TagRegistry> =
                expected.iter().map(|r| (r.name.as_str(), r)).collect();
            let ours: BTreeMap<&str, &wire::TagRegistry> =
                found.iter().map(|r| (r.name.as_str(), r)).collect();
            for name in van.keys() {
                if !ours.contains_key(name) {
                    out.push(format!("Dust sent no tags for {name}"));
                }
            }
            for name in ours.keys() {
                if !van.contains_key(name) {
                    out.push(format!(
                        "Dust sent tags for {name}, which Minecraft did not"
                    ));
                }
            }
            for (name, expected) in &van {
                let Some(found) = ours.get(name) else {
                    continue;
                };
                // Tags as a map and ids as a set: the client builds both, so
                // the order either server chose is not a difference it sees.
                let van_tags: BTreeMap<&str, BTreeSet<i32>> = expected
                    .tags
                    .iter()
                    .map(|(id, ids)| (id.as_str(), ids.iter().copied().collect()))
                    .collect();
                let our_tags: BTreeMap<&str, BTreeSet<i32>> = found
                    .tags
                    .iter()
                    .map(|(id, ids)| (id.as_str(), ids.iter().copied().collect()))
                    .collect();
                for tag in van_tags.keys() {
                    if !our_tags.contains_key(tag) {
                        out.push(format!("{name}: Dust is missing the tag {tag}"));
                    }
                }
                for tag in our_tags.keys() {
                    if !van_tags.contains_key(tag) {
                        out.push(format!("{name}: Dust has an extra tag {tag}"));
                    }
                }
                for (tag, want) in &van_tags {
                    let Some(got) = our_tags.get(tag) else {
                        continue;
                    };
                    if want != got {
                        let missing = want.difference(got).count();
                        let extra = got.difference(want).count();
                        out.push(format!(
                            "{name} {tag}: {missing} id(s) missing, {extra} unexpected"
                        ));
                    }
                }
            }
        }
    }

    out
}

/// Walk two NBT trees, naming every place they differ.
///
/// Compounds are compared as maps: NBT has no order and the two servers write
/// their keys in different ones. Lists *are* ordered, and are compared as
/// written.
fn compare_nodes(want: &nbt::Node, got: &nbt::Node, path: &str, out: &mut Vec<String>) {
    let at = |path: &str| {
        if path.is_empty() {
            "the entry".to_owned()
        } else {
            format!("`{path}`")
        }
    };
    match (want, got) {
        (nbt::Node::Compound(a), nbt::Node::Compound(b)) => {
            let a: BTreeMap<&str, &nbt::Node> = a.iter().map(|(k, v)| (k.as_str(), v)).collect();
            let b: BTreeMap<&str, &nbt::Node> = b.iter().map(|(k, v)| (k.as_str(), v)).collect();
            for key in a.keys() {
                if !b.contains_key(key) {
                    out.push(format!("{} is missing {key}", at(path)));
                }
            }
            for key in b.keys() {
                if !a.contains_key(key) {
                    out.push(format!("{} has an extra {key}", at(path)));
                }
            }
            for (key, want) in &a {
                if let Some(got) = b.get(key) {
                    let child = if path.is_empty() {
                        (*key).to_owned()
                    } else {
                        format!("{path}.{key}")
                    };
                    compare_nodes(want, got, &child, out);
                }
            }
        }
        (nbt::Node::List(a), nbt::Node::List(b)) => {
            if a.len() != b.len() {
                out.push(format!(
                    "{} has {} items against {}",
                    at(path),
                    b.len(),
                    a.len()
                ));
                return;
            }
            for (index, (want, got)) in a.iter().zip(b).enumerate() {
                compare_nodes(want, got, &format!("{path}[{index}]"), out);
            }
        }
        (a, b) if a == b => {}
        (a, b) => out.push(format!("{} is {b:?}, not {a:?}", at(path))),
    }
}
