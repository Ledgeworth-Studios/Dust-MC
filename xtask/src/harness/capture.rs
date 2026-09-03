//! `harness capture` — boot vanilla, pregenerate a world, fingerprint it.
//!
//! One run is one supervised server process and one digest set:
//!
//! 1. **Boot.** `java` is pointed at the provisioned run directory with the
//!    flags from [`jvm_flags`] and its stdout/stderr tapped line by line.
//!    Startup is complete when vanilla prints its own readiness line —
//!    [`startup_complete`] — rather than on a timer, because timers measure
//!    the slowest machine that ever runs this, badly.
//! 2. **Pregenerate.** With no player to drag chunks in, generation is forced
//!    with `/forceload add` over exactly the block box covering the expected
//!    chunk square ([`forceload_box`] derives it from that same list, so the
//!    forced area and the scanned area cannot drift apart), then the region
//!    directory is polled ([`pending_chunks`]) until every expected chunk has
//!    been written. Presence in the region header is the poll's criterion;
//!    *completeness* is verified later per chunk by [`digest::scan`], which
//!    refuses anything whose `Status` is below `full`.
//! 3. **Settle and stop.** `save-all flush`, then `/stop` sent without
//!    waiting for its reply ([`rcon::Client::send_and_move_on`]), then a wait
//!    on the process with a kill fallback. Reading begins only after exit:
//!    disk state is settled exactly once the writer is gone.
//! 4. **Digest.** The region files are read directly — anvil layout,
//!    decompression, the harness-local NBT walk — and reduced to a
//!    [`digest::DigestSet`], written as `chunks.bin` plus a human `chunks.tsv`
//!    under the cache, keyed by version, seed and radius.
//!
//! # What the JVM flags do and do not promise
//!
//! They make runs short and boring: a fixed heap and the serial collector.
//! They do not pretend to make Java deterministic — JIT, GC timing and thread
//! scheduling stay uncontrolled, and none of it reaches the hashed bytes. The
//! determinism argument lives in the module docs on [`super`]; these flags are
//! hygiene, not the argument.

use std::collections::{HashMap, VecDeque};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{cache, digest, properties, rcon};

/// How long boot + pregeneration + stop + scan may take in total, unless
/// `--timeout` says otherwise. A radius that needs longer deserves an explicit
/// budget, not a silent change of mind here.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// The largest radius accepted. 32 means 65×65 = 4225 chunks — minutes of
/// generation and tens of kilobytes of digests. Beyond that the operator
/// should be splitting captures, not widening one.
const MAX_RADIUS: i32 = 32;

/// Blocks per chunk edge; the conversion between chunk and block coordinates.
const CHUNK_EDGE_BLOCKS: i32 = 16;

#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub seed: i64,
    pub radius: i32,
    /// The chunks the squares are centred on. Not a convenience: two biomes in
    /// a 9x9 is the multi-noise field being smooth at that scale, so a biome
    /// source cannot be *scored* on one square wherever it is put. Several
    /// small squares far apart reach climate a wide square never would, at a
    /// cost linear in chunks rather than in the square of the radius — and one
    /// boot rather than one boot per square, which is the difference between
    /// two minutes of vanilla and twelve.
    pub centres: Vec<(i32, i32)>,
    /// A jar the operator has already obtained, instead of downloading.
    pub jar: Option<PathBuf>,
    /// Whole-run budget: boot, pregeneration, stop and scan together.
    pub timeout: Duration,
}

/// Parse the `harness capture` argument list.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut version = None;
    let mut seed = None;
    let mut radius = None;
    let mut centres: Vec<(i32, i32)> = Vec::new();
    let mut jar = None;
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--version" => {
                at = super::take_value(&mut seen, "--version", args, at + 1)?;
                version = Some(seen.last().expect("just stored").1.clone());
            }
            "--seed" => {
                at = super::take_value(&mut seen, "--seed", args, at + 1)?;
                seed = Some(
                    seen.last()
                        .expect("just stored")
                        .1
                        .parse()
                        .map_err(|_| "--seed needs a signed 64-bit integer")?,
                );
            }
            "--radius" => {
                at = super::take_value(&mut seen, "--radius", args, at + 1)?;
                radius = Some(
                    seen.last()
                        .expect("just stored")
                        .1
                        .parse()
                        .map_err(|_| "--radius needs a chunk count")?,
                );
            }
            "--at" => {
                at = super::take_value(&mut seen, "--at", args, at + 1)?;
                let value = seen.last().expect("just stored").1.clone();
                let (x, z) = value
                    .split_once(',')
                    .ok_or("--at needs two chunk coordinates, as `x,z`")?;
                centres.push((
                    x.trim()
                        .parse()
                        .map_err(|_| "--at's x is not a whole number")?,
                    z.trim()
                        .parse()
                        .map_err(|_| "--at's z is not a whole number")?,
                ));
            }
            "--jar" => {
                at = super::take_value(&mut seen, "--jar", args, at + 1)?;
                jar = Some(PathBuf::from(seen.last().expect("just stored").1.clone()));
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
            other => {
                return Err(format!(
                    "unknown capture option `{other}`\n\n{}",
                    super::USAGE
                ));
            }
        }
    }
    let version = version.ok_or_else(|| format!("capture needs --version\n\n{}", super::USAGE))?;
    let seed = seed.ok_or_else(|| format!("capture needs --seed\n\n{}", super::USAGE))?;
    let radius = radius.ok_or_else(|| format!("capture needs --radius\n\n{}", super::USAGE))?;
    if !(0..=MAX_RADIUS).contains(&radius) {
        return Err(format!(
            "--radius must be between 0 and {MAX_RADIUS} chunks, got {radius}"
        ));
    }
    Ok(Options {
        version,
        seed,
        radius,
        centres: if centres.is_empty() {
            vec![(0, 0)]
        } else {
            centres
        },
        jar,
        timeout,
    })
}

/// Run one capture end to end, printing progress as it goes.
pub fn run(options: &Options) -> Result<(), String> {
    let started = Instant::now();
    let layout = cache::Layout::resolve()?;
    let dir = layout.server_dir(&options.version, options.seed);
    if !dir.is_dir() {
        return Err(format!(
            "{} does not exist; run `cargo xtask harness provision --version {} --seed {}` \
             first",
            dir.display(),
            options.version,
            options.seed
        ));
    }
    if !properties::eula_accepted(&dir)? {
        return Err(format!(
            "the EULA is not accepted in {}; read it, then accept it with \
             `harness provision --yes`",
            dir.display()
        ));
    }

    let jar = match &options.jar {
        Some(path) => path.clone(),
        None => crate::extract::download::server_jar(&options.version, &layout.jars)?,
    };

    let label = capture_label(
        &options.version,
        options.seed,
        options.radius,
        &options.centres,
    );
    capture_from(options, &jar, &dir, &label, &layout, started)
}

/// Boot a run directory, force what is missing, and fingerprint what is on
/// disk when it stops.
///
/// Split out of [`run`] because `rewrite` needs exactly this over a directory
/// that is *not* `Layout::server_dir(version, seed)` — a copy of a world that
/// Dust has rewritten. Sharing the function rather than the reasoning is the
/// point: a second boot-and-scan written beside this one would be a second
/// answer to "what counts as settled disk state", and the two would drift.
///
/// A run directory that already holds every expected chunk is booted anyway.
/// That is not waste — for `rewrite` it *is* the test, because loading a world
/// is the thing being checked, and vanilla resaving what it loaded is how the
/// result becomes comparable.
pub(super) fn capture_from(
    options: &Options,
    jar: &Path,
    dir: &Path,
    label: &str,
    layout: &cache::Layout,
    started: Instant,
) -> Result<(), String> {
    let expected = digest::expected_chunks_over(options.radius, &options.centres);
    println!(
        "capturing {} seed {} from {}: {} chunks within radius {} of {}, budget {}s",
        options.version,
        options.seed,
        dir.display(),
        expected.len(),
        options.radius,
        describe_centres(&options.centres),
        options.timeout.as_secs()
    );

    let region_dir = dir.join("world/region");
    let transcript = supervise_run(options, jar, dir, &region_dir, &expected)?;

    let set = digest::scan(&region_dir, &expected, options.seed)?;
    let out_dir = layout.capture_dir(label);
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
    // Everything vanilla said, kept beside the digests. A digest answers "is
    // this the same world"; the transcript answers "did the server that read it
    // have anything to say about it", and the two are different questions. See
    // `rewrite`, where the second one is the half the digest cannot reach.
    let log = out_dir.join("server.log");
    std::fs::write(&log, transcript.join("\n"))
        .map_err(|e| format!("could not write {}: {e}", log.display()))?;
    let bin = out_dir.join("chunks.bin");
    let tsv = out_dir.join("chunks.tsv");
    digest::write_bin(&set, &bin)?;
    digest::write_tsv(&set, &tsv)?;

    println!(
        "\ncaptured {} chunks (data version {}) in {:.1}s",
        set.chunks.len(),
        set.data_version,
        started.elapsed().as_secs_f64()
    );
    println!("digest: {}", bin.display());
    println!("table:  {}", tsv.display());
    println!("log:    {}", log.display());
    Ok(())
}

/// Boot, pregenerate, settle, stop — everything between having a run
/// directory and holding settled world files.
///
/// The two output pipe types differ only in name, so both are erased to
/// `Box<dyn Read>` before the reader threads are spawned.
fn supervise_run(
    options: &Options,
    jar: &Path,
    dir: &Path,
    region_dir: &Path,
    expected: &[(i32, i32)],
) -> Result<Vec<String>, String> {
    fn boxed(stream: impl std::io::Read + Send + 'static) -> Box<dyn std::io::Read + Send> {
        Box::new(stream)
    }

    let deadline = Instant::now() + options.timeout;
    let (program, args) = java_command(jar);
    let mut child = std::process::Command::new(program)
        .args(&args)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "java was not found on PATH; the harness needs a JDK 21 or newer".to_owned()
            } else {
                format!("could not start java: {e}")
            }
        })?;

    // Both output pipes are drained continuously from here on — a pipe nobody
    // reads fills after a few kilobytes and the server blocks mid-write,
    // which looks exactly like a hang and is not one.
    let (tx, rx) = mpsc::channel::<String>();
    // Recorded as well as forwarded. The channel is consumed by whichever
    // phase happens to be waiting, so a line that arrives during
    // pregeneration is read by nobody — and that is exactly the window a
    // complaint about a chunk would arrive in. The transcript is written to
    // by the reader threads themselves so that nothing downstream has to
    // remember to drain.
    let transcript = Arc::new(Mutex::new(Vec::<String>::new()));
    for stream in [
        child.stdout.take().map(boxed).expect("stdout was piped"),
        child.stderr.take().map(boxed).expect("stderr was piped"),
    ] {
        let tx = tx.clone();
        let transcript = Arc::clone(&transcript);
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(mut lines) = transcript.lock() {
                    lines.push(line.clone());
                }
                if tx.send(line).is_err() {
                    return; // supervisor gone; nothing left to tell
                }
            }
        });
    }
    drop(tx);

    // Every failure path below leaves a running server behind unless this
    // owns the shutdown: whatever goes wrong, the process does not outlive
    // the command that started it.
    let outcome = run_phases(
        &rx,
        &mut child,
        region_dir,
        expected,
        options.radius,
        &options.centres,
        deadline,
    );
    if outcome.is_err() && child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // The reader threads end when their pipes close, which the process exiting
    // does. Draining the channel here rather than joining them: a thread
    // blocked on a pipe that never closes would hold the run open, and the
    // transcript is worth having even if it is a line short.
    while rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
    let lines = transcript
        .lock()
        .map(|lines| lines.clone())
        .unwrap_or_default();
    outcome.map(|()| lines)
}

/// Boot to readiness, pregenerate, settle, stop, confirm exit.
fn run_phases(
    rx: &mpsc::Receiver<String>,
    child: &mut std::process::Child,
    region_dir: &Path,
    expected: &[(i32, i32)],
    radius: i32,
    squares: &[(i32, i32)],
    deadline: Instant,
) -> Result<(), String> {
    wait_until_ready(rx, child, deadline)?;
    pregenerate(expected, radius, squares, region_dir, deadline)?;
    flush_and_stop(deadline)?;
    wait_for_exit(child, deadline)?;

    let pending = pending_chunks(region_dir, expected);
    if !pending.is_empty() {
        return Err(format!(
            "the server exited but {} expected chunk(s) never reached disk, starting with \
             {:?}; the pregeneration did not complete",
            pending.len(),
            &pending[..pending.len().min(5)]
        ));
    }
    Ok(())
}

/// Drain the boot log until vanilla announces itself ready.
///
/// Every line is echoed prefixed, so the operator watches the same boot they
/// would watching the server by hand. If the process dies first, the tail of
/// its own log is the error message — there is no better description of why a
/// server refused to start than the last things it said.
fn wait_until_ready(
    rx: &mpsc::Receiver<String>,
    child: &mut std::process::Child,
    deadline: Instant,
) -> Result<(), String> {
    let mut recent: VecDeque<String> = VecDeque::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                println!("vanilla | {line}");
                recent.push_back(line.clone());
                if recent.len() > 10 {
                    recent.pop_front();
                }
                if startup_complete(&line) {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "the server closed its output before finishing startup:\n{}",
                    tail_of(&recent)
                ));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the server did not finish starting inside the time budget".to_owned());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "the server exited during startup ({status}):\n{}",
                tail_of(&recent)
            ));
        }
    }
}

fn tail_of(recent: &VecDeque<String>) -> String {
    recent
        .iter()
        .map(|l| format!("vanilla | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does this line mean startup finished?
///
/// Vanilla prints `Done (12.345s)! For help, type "help"` once the server is
/// accepting commands. Matching two fragments rather than one makes a
/// coincidental match of either alone unlikely; both have been stable across
/// every modern release.
pub(super) fn startup_complete(line: &str) -> bool {
    line.contains("Done (") && line.contains("! For help")
}

/// Force-generate every expected chunk and wait until each is on disk.
///
/// Spawn chunks generate during boot anyway; `forceload add` covers the rest
/// and pins them loaded so view-distance settings cannot thin the square out.
fn pregenerate(
    expected: &[(i32, i32)],
    radius: i32,
    squares: &[(i32, i32)],
    region_dir: &Path,
    deadline: Instant,
) -> Result<(), String> {
    // One command per square, not one over their bounding box. Two squares a
    // thousand chunks apart share a box holding a million chunks, and vanilla
    // would either refuse it or generate the lot; the whole point of scattering
    // the sample is that the space between the squares is never visited.
    let boxes: Vec<((i32, i32), (i32, i32))> = squares
        .iter()
        .map(|&centre| forceload_box(&digest::expected_chunks_at(radius, centre)))
        .collect();
    rcon_with_retries(deadline, &mut |client: &mut rcon::Client| {
        for ((min_x, min_z), (max_x, max_z)) in &boxes {
            client.exec_delimited(&format!("forceload add {min_x} {min_z} {max_x} {max_z}"))?;
        }
        Ok(())
    })?;
    println!(
        "forced load over {} square(s); waiting for {} chunks to reach disk",
        boxes.len(),
        expected.len()
    );

    loop {
        let pending = pending_chunks(region_dir, expected);
        if pending.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} chunk(s) still unwritten as the budget ran out, starting with {:?}",
                pending.len(),
                &pending[..pending.len().min(5)]
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(Duration::from_secs(2).min(remaining));
    }
}

/// The inclusive block box covering every chunk in `expected`.
///
/// Derived from the chunk list rather than from a stored radius so the forced
/// area and the scanned area cannot drift apart as layouts evolve. Chunk
/// coordinates scale by sixteen; the far corner is the last block of the
/// highest chunk, which is why it gets an edge minus one.
fn forceload_box(expected: &[(i32, i32)]) -> ((i32, i32), (i32, i32)) {
    let min_x = expected.iter().map(|&(x, _)| x).min().unwrap_or(0);
    let max_x = expected.iter().map(|&(x, _)| x).max().unwrap_or(0);
    let min_z = expected.iter().map(|&(_, z)| z).min().unwrap_or(0);
    let max_z = expected.iter().map(|&(_, z)| z).max().unwrap_or(0);
    (
        (min_x * CHUNK_EDGE_BLOCKS, min_z * CHUNK_EDGE_BLOCKS),
        (
            max_x * CHUNK_EDGE_BLOCKS + CHUNK_EDGE_BLOCKS - 1,
            max_z * CHUNK_EDGE_BLOCKS + CHUNK_EDGE_BLOCKS - 1,
        ),
    )
}

/// Which expected chunks have not reached the region directory yet.
///
/// Polling treats *any* read problem as "not yet": a file half-written by the
/// running server fails this poll now and, if it never settles, fails the
/// strict scan later with a much better error. This function must be cheap
/// and quiet; judgement belongs to the scan.
fn pending_chunks(region_dir: &Path, expected: &[(i32, i32)]) -> Vec<(i32, i32)> {
    // Keyed by region coordinates: consecutive expected chunks share a file,
    // so this reads each file at most once per poll.
    let mut files: HashMap<(i32, i32), Option<Vec<u8>>> = HashMap::new();
    let mut pending = Vec::new();
    for &(x, z) in expected {
        let key = super::region::region_coords(x, z);
        let contents = files.entry(key).or_insert_with(|| {
            std::fs::read(super::region::region_file_path(region_dir, key.0, key.1)).ok()
        });
        let present = contents
            .as_deref()
            .and_then(|bytes| super::region::read_chunk(bytes, x, z).ok())
            .flatten()
            .is_some();
        if !present {
            pending.push((x, z));
        }
    }
    pending
}

/// Flush the save and ask the server to stop.
pub(super) fn flush_and_stop(deadline: Instant) -> Result<(), String> {
    rcon_with_retries(deadline, &mut |client: &mut rcon::Client| {
        let saved = client.exec_delimited("save-all flush")?;
        println!("save-all: {}", saved.trim());
        client.send_and_move_on("stop")
    })
}

/// Connect, authenticate, and do the work — retrying while RCON comes up.
///
/// Vanilla binds its RCON listener late in boot, in some orderings after the
/// readiness line, so a single attempt right after startup races the socket.
fn rcon_with_retries(
    deadline: Instant,
    work: &mut dyn FnMut(&mut rcon::Client) -> Result<(), String>,
) -> Result<(), String> {
    loop {
        match attempt_rcon(work) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "could not reach the server over RCON on port {}: {e}",
                        properties::RCON_PORT
                    ));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// One connect-auth-work sequence; its error is the caller's retry signal.
fn attempt_rcon(
    work: &mut dyn FnMut(&mut rcon::Client) -> Result<(), String>,
) -> Result<(), String> {
    let mut client = rcon::Client::connect(
        ("127.0.0.1", properties::RCON_PORT),
        Duration::from_secs(10),
    )?;
    client.authenticate(properties::RCON_PASSWORD)?;
    work(&mut client)
}

/// Wait for the process to leave, killing it if it overstays.
pub(super) fn wait_for_exit(
    child: &mut std::process::Child,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return Ok(()),
            Ok(None) => {}
            Err(e) => return Err(format!("could not watch the server process: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the server ignored /stop until the budget ran out; killed".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The JVM invocation: flags first, then the jar, headless.
pub(super) fn java_command(jar: &Path) -> (&'static str, Vec<String>) {
    let mut args: Vec<String> = jvm_flags().iter().map(|s| (*s).to_owned()).collect();
    args.push("-jar".to_owned());
    args.push(jar.display().to_string());
    args.push("nogui".to_owned());
    ("java", args)
}

/// Heap and collector choices, in one place so they can be reasoned about.
///
/// `-Xms` equals `-Xmx` to skip heap-resize pauses mid-run; the serial
/// collector is the least concurrent thing the JVM offers, which keeps thread
/// counts down on CI-sized machines. Nothing here claims to remove
/// nondeterminism — see the module docs — and nothing here changes any byte
/// that reaches the digest.
fn jvm_flags() -> &'static [&'static str] {
    &["-Xms512M", "-Xmx2G", "-XX:+UseSerialGC"]
}

/// The cache label one capture is filed under.
///
/// A square centred on the origin keeps the label it has always had, so the
/// captures already on disk stay findable and `rewrite`'s baseline lookup does
/// not move. Anywhere else the centre is part of the name, because two squares
/// of the same radius on the same seed are different worlds and a shared label
/// would have the second silently overwrite the first.
pub(super) fn capture_label(
    version: &str,
    seed: i64,
    radius: i32,
    centres: &[(i32, i32)],
) -> String {
    let base = format!("{version}-seed-{seed}-radius-{radius}");
    if centres == [(0, 0)] {
        return base;
    }
    let mut sorted = centres.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = base;
    for (x, z) in sorted {
        out.push_str(&format!("-at-{x}_{z}"));
    }
    out
}

/// The centres, for the one line that says what is about to be generated.
fn describe_centres(centres: &[(i32, i32)]) -> String {
    centres
        .iter()
        .map(|(x, z)| format!("{x},{z}"))
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::nbt::writer::{n, root};
    use crate::harness::region::builder::build_region;
    use crate::harness::region::{COMPRESSION_ZLIB, REGION_CHUNKS};
    use crate::harness::testing::scratch_dir;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_readiness_line_is_vanillas_own_announcement() {
        assert!(startup_complete(
            "[12:00:00] [Server thread/INFO]: Done (2.913s)! For help, type \"help\""
        ));
        // Near misses a naive matcher would take.
        assert!(!startup_complete(
            "Starting minecraft server version 1.21.1"
        ));
        assert!(!startup_complete("Preparing level \"world\""));
        assert!(!startup_complete("[INFO]: Done loading something else"),);
        assert!(
            !startup_complete("Done! For help, type \"help\""),
            "no timing paren"
        );
    }

    #[test]
    fn the_java_invocation_points_at_the_jar_headlessly_with_the_flags_first() {
        let (program, args) = java_command(Path::new("/cache/jars/server-1.21.1.jar"));
        assert_eq!(program, "java");
        assert_eq!(args.last().expect("nogui"), "nogui");
        assert_eq!(args[args.len() - 2], "/cache/jars/server-1.21.1.jar");
        assert_eq!(
            args.iter().position(|a| a == "-jar"),
            Some(args.len() - 3),
            "-jar sits between the JVM flags and the jar path"
        );
        assert!(args[..args.len() - 3].iter().any(|a| a.starts_with("-Xmx")));
    }

    #[test]
    fn the_jvm_flags_are_the_documented_boring_set() {
        assert_eq!(jvm_flags(), &["-Xms512M", "-Xmx2G", "-XX:+UseSerialGC"]);
    }

    #[test]
    fn the_forceload_box_covers_exactly_the_expected_chunks() {
        // Corners land on chunk boundaries at the low end and one block short
        // of the next boundary at the high end, which is how an inclusive box
        // spells "these chunks and no others".
        let ((ax, az), (bx, bz)) = forceload_box(&[(-2, -2), (2, 2)]);
        assert_eq!((ax, az), (-32, -32));
        assert_eq!((bx, bz), (47, 47));

        // A single chunk is a single-chunk box.
        assert_eq!(forceload_box(&[(5, -7)]), ((80, -112), (95, -97)));

        // An empty expectation degenerates to the origin chunk rather than an
        // invalid box.
        assert_eq!(
            forceload_box(&[]),
            ((0, 0), (CHUNK_EDGE_BLOCKS - 1, CHUNK_EDGE_BLOCKS - 1))
        );
    }

    /// A minimal but valid saved chunk, compressed the way vanilla frames it
    /// inside a sector payload.
    fn zlib_chunk() -> Vec<u8> {
        let node = root(vec![
            ("DataVersion", n::i(3953)),
            ("Status", n::str("minecraft:full")),
        ]);
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &node).expect("zlib");
        encoder.finish().expect("zlib finish")
    }

    /// Lay synthetic region files holding the given chunks, grouped by their
    /// real region files the way the layout demands.
    fn write_world(world: &Path, chunks: &[(i32, i32)]) {
        type RegionEntries = Vec<(usize, u8, Vec<u8>)>;
        let payload = zlib_chunk();
        let mut files: HashMap<(i32, i32), RegionEntries> = HashMap::new();
        for &(cx, cz) in chunks {
            files
                .entry(crate::harness::region::region_coords(cx, cz))
                .or_default()
                .push((
                    crate::harness::region::local_index(cx, cz),
                    COMPRESSION_ZLIB,
                    payload.clone(),
                ));
        }
        std::fs::create_dir_all(world).expect("mkdir");
        for ((rx, rz), entries) in files {
            assert!(entries.len() <= (REGION_CHUNKS * REGION_CHUNKS) as usize);
            let path = world.join(crate::harness::region::region_file_name(rx, rz));
            std::fs::write(&path, build_region(&entries)).expect("write region");
        }
    }

    #[test]
    fn polling_reports_only_chunks_that_have_not_landed() {
        let world = scratch_dir("capture-pending").join("world/region");
        write_world(&world, &[(0, 0), (1, 0)]);

        let expected = vec![(0, 0), (1, 0), (2, 0), (-1, -1)];
        let pending = pending_chunks(&world, &expected);
        assert_eq!(pending, vec![(2, 0), (-1, -1)], "only the unwritten ones");

        write_world(&world, &expected);
        assert!(pending_chunks(&world, &expected).is_empty());
    }

    #[test]
    fn a_corrupt_region_file_counts_as_pending_rather_than_stopping_the_poll() {
        let world = scratch_dir("capture-pending-corrupt").join("world/region");
        std::fs::create_dir_all(&world).expect("mkdir");
        std::fs::write(world.join("r.0.0.mca"), b"not a region file").expect("junk");
        assert_eq!(pending_chunks(&world, &[(0, 0)]), vec![(0, 0)]);
    }

    #[test]
    fn the_label_keys_a_capture_by_every_input_that_moves_its_digest() {
        assert_eq!(
            capture_label("1.21.1", 0, 2, (0, 0)),
            "1.21.1-seed-0-radius-2"
        );
        assert_eq!(
            capture_label("1.21.1", 0, 2, (-400, 900)),
            "1.21.1-seed-0-radius-2-at--400-900"
        );
        assert_ne!(
            capture_label("1.21.1", 0, 2, (0, 0)),
            capture_label("1.21.1", 1, 2, (0, 0)),
            "seeds must not share a capture"
        );
        assert_ne!(
            capture_label("1.21.1", 0, 2, (0, 0)),
            capture_label("1.21.1", 0, 3, (0, 0)),
            "radii must not share a capture"
        );
    }

    #[test]
    fn absurd_radii_are_refused_before_any_server_is_touched() {
        let err = parse(&args(&[
            "--version",
            "1.21.1",
            "--seed",
            "0",
            "--radius",
            "-1",
        ]))
        .expect_err("negative refused");
        assert!(err.contains("--radius"), "{err}");

        let err = parse(&args(&[
            "--version",
            "1.21.1",
            "--seed",
            "0",
            "--radius",
            &(MAX_RADIUS + 1).to_string(),
        ]))
        .expect_err("over-large refused");
        assert!(err.contains("--radius"), "{err}");
    }

    #[test]
    fn parsing_requires_every_input_that_names_a_world() {
        for missing in [
            vec!["--seed", "0", "--radius", "2"],
            vec!["--version", "1.21.1", "--radius", "2"],
            vec!["--version", "1.21.1", "--seed", "0"],
        ] {
            assert!(
                parse(&args(&missing)).is_err(),
                "missing one of version/seed/radius: {missing:?}"
            );
        }
        let ok = parse(&args(&[
            "--version",
            "1.21.1",
            "--seed",
            "7",
            "--radius",
            "2",
            "--timeout",
            "60",
        ]))
        .expect("complete parse");
        assert_eq!(ok.timeout, Duration::from_secs(60));
        assert_eq!(ok.radius, 2);
        assert_eq!(ok.seed, 7);
    }

    // ------------------------------------------------------------------
    // Smoke gate: exercises the real readers against a real captured world,
    // but only when one exists in the operator's cache. Without it, the test
    // says so loudly and passes — CI has no vanilla server, and pretending
    // otherwise would be a lie told in green.
    // ------------------------------------------------------------------

    #[test]
    fn a_real_cached_world_scans_if_one_exists() {
        let Ok(layout) = cache::Layout::resolve() else {
            println!("SKIP: the harness cache could not be created; no real-world smoke");
            return;
        };
        let Some((dir, seed)) = find_provisioned_world(&layout.servers) else {
            println!(
                "SKIP: no provisioned world under {} yet; run `cargo xtask harness \
                 provision --yes` plus one capture to enable this smoke",
                layout.servers.display()
            );
            return;
        };

        let region_dir = dir.join("world/region");
        let Some(listed) = list_any_region_chunks(&region_dir).filter(|l| !l.is_empty()) else {
            println!(
                "SKIP: {} holds no generated chunks yet; capture first",
                region_dir.display()
            );
            return;
        };
        // **What a region file lists is not what was captured.** Generating a
        // `full` chunk needs its neighbours as far as `structure_starts`, and
        // vanilla writes those into the same file, so every capture leaves a
        // ring of unfinished chunks around the square it was asked for. Taking
        // the first twenty-five *listed* was a coin flip on whichever radius
        // somebody last ran — green at radius 2 and red at radius 4, on a
        // world neither run had anything wrong with.
        //
        // So both halves are asserted instead: the finished chunks scan, and a
        // chunk from the ring is *refused*. The second half is the one the
        // original spelling was reaching for and could not state.
        let mut full: Vec<(i32, i32)> = Vec::new();
        let mut ring: Vec<(i32, i32)> = Vec::new();
        for pos in listed.into_iter().take(400) {
            if digest::scan(&region_dir, &[pos], seed).is_ok() {
                full.push(pos);
            } else {
                ring.push(pos);
            }
            if full.len() >= 25 && !ring.is_empty() {
                break;
            }
        }
        assert!(
            !full.is_empty(),
            "a captured world has finished chunks in it"
        );
        let set =
            digest::scan(&region_dir, &full, seed).expect("a real vanilla world must scan cleanly");
        assert!(!set.chunks.is_empty());
        for chunk in &set.chunks {
            assert_eq!(chunk.status, "full", "a saved chunk must be complete");
        }
        if let Some(&edge) = ring.first() {
            let err = digest::scan(&region_dir, &[edge], seed)
                .expect_err("a chunk vanilla never finished must be refused, not fingerprinted");
            assert!(
                err.contains("the pregeneration did not finish"),
                "the refusal must say what is wrong with it: {err}"
            );
        }
        println!(
            "smoke: fingerprinted {} real chunk(s) from {}",
            set.chunks.len(),
            region_dir.display()
        );
    }

    /// Any one provisioned world in the cache, with its seed parsed back out
    /// of the directory name [`super::cache::Layout::server_dir`] wrote.
    fn find_provisioned_world(servers: &Path) -> Option<(PathBuf, i64)> {
        let version = std::fs::read_dir(servers).ok()?.next()?.ok()?.file_name();
        let seed_dir = std::fs::read_dir(servers.join(&version))
            .ok()?
            .next()?
            .ok()?
            .file_name();
        let seed_text = seed_dir.to_string_lossy().strip_prefix("seed-")?.to_owned();
        let seed = seed_text.parse().ok()?;
        Some((servers.join(version).join(seed_dir), seed))
    }

    /// Every chunk listed in any region file under a region directory.
    fn list_any_region_chunks(region_dir: &Path) -> Option<Vec<(i32, i32)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(region_dir).ok()?.flatten() {
            let rest = entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("r.")?
                .to_owned();
            let mut parts = rest.split('.');
            let rx: i32 = parts.next()?.parse().ok()?;
            let rz: i32 = parts.next()?.parse().ok()?;
            let bytes = std::fs::read(entry.path()).ok()?;
            found.extend(crate::harness::region::listed_chunks(&bytes, rx, rz).ok()?);
        }
        Some(found)
    }
}
