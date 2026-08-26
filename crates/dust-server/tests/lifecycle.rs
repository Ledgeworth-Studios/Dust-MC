//! End-to-end lifecycle proofs, driven entirely on virtual time.
//!
//! These tests sit outside the crate on purpose: they see exactly what a
//! Phase 3 integrator will see — the public `dust_server` surface, nothing
//! else. A full boot, ten ticks, a simulated ctrl-C and a symmetric shutdown
//! cost microseconds here; the same scenario against a real clock would need
//! half a second of sleeping to prove less.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dust_config::DustConfig;
use dust_server::clock::{Clock, ManualClock};
use dust_server::engine::TICK_NS;
use dust_server::participant::{TickContext, TickParticipant};
use dust_server::server::{
    Direction, LiveMetrics, ParkerFactory, Phase, Server, ServerOptions, ShutdownReport,
    WatchdogSetting,
};
use dust_server::stop::{Parker, StepParker, StopHandle};

/// A unique temp file per call: tests run in parallel in one process.
fn write_config(text: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "dust-server-lifecycle-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::write(&path, text).expect("write the temp config");
    path
}

/// The parker factory for the tick loop: every park advances virtual time by
/// `step_ns`, which turns "let the server run" into arithmetic.
fn stepping(clock: Arc<ManualClock>, step_ns: u64) -> ParkerFactory {
    Arc::new(move |_state, _clock| {
        Box::new(StepParker::new(clock.clone(), step_ns)) as Box<dyn Parker>
    })
}

/// A participant that records every tick index it is handed.
struct Counter {
    log: Arc<Mutex<Vec<u64>>>,
}

impl TickParticipant for Counter {
    fn name(&self) -> &str {
        "counter"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn tick(&mut self, ctx: &TickContext) {
        self.log.lock().unwrap().push(ctx.tick_index);
    }
}

/// A config for a test, with a listener that cannot collide.
///
/// Booting now takes a real port. Loopback so no test opens a port to the
/// network, and port 0 so the operating system picks a free one — these tests
/// run in parallel with each other and with the crate's unit tests, and a
/// fixed port would make the suite pass alone and fail together.
fn with_test_bind(config_text: &str) -> String {
    if config_text.contains("bind") {
        return config_text.to_owned();
    }
    match config_text.strip_prefix("[server]\n") {
        Some(rest) => format!("[server]\nbind = \"127.0.0.1:0\"\n{rest}"),
        None => format!("[server]\nbind = \"127.0.0.1:0\"\n{config_text}"),
    }
}

/// Everything a test needs to watch and stop a running server.
struct Running {
    metrics: LiveMetrics,
    stop: StopHandle,
    worker: Option<std::thread::JoinHandle<Result<ShutdownReport, dust_server::ServerError>>>,
}

/// Boot a virtual-time server around one config file and one tick period of
/// parking per pass. The watchdog runs with an explicit recording policy at a
/// grace far beyond anything these tests spend: what they exercise is the
/// graceful path, and the timeout's *value* reaching the watchdog is asserted
/// by the unit tests inside the crate.
fn start(config_text: &str, extras: Vec<Box<dyn TickParticipant>>) -> Running {
    let clock = Arc::new(ManualClock::new());
    let options = ServerOptions {
        config_path: write_config(&with_test_bind(config_text)),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(clock, TICK_NS),
        watchdog: WatchdogSetting::Custom(dust_server::WatchdogPolicy::custom(
            600_000_000_000,
            |_| {},
        )),
        extra_tasks: extras,
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    Running {
        metrics: server.metrics(),
        stop: server.stop_handle(),
        worker: Some(std::thread::spawn(move || server.run())),
    }
}

/// Wait for progress without sleeping: bounded cooperative spinning, panics
/// if the server stalls short of `minimum`.
fn wait_for(running: &Running, minimum_ticks: u64) {
    for _ in 0..50_000_000 {
        if running.metrics.ticks_observed() >= minimum_ticks {
            return;
        }
        // A server whose boot failed never ticks, and to a spin loop that is
        // indistinguishable from one that is merely slow. The run thread has
        // already finished in that case, and saying so turns "stuck at 0" —
        // which names no cause at all — into the phase error that actually
        // happened.
        if running
            .worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            panic!(
                "the run thread exited before reaching {minimum_ticks} tick(s); \
                 the boot failed rather than stalled"
            );
        }
        std::thread::yield_now();
    }
    panic!(
        "the server never reached {minimum_ticks} tick(s); stuck at {}",
        running.metrics.ticks_observed()
    );
}

fn finish(mut running: Running) -> ShutdownReport {
    running.stop.request_stop();
    running
        .worker
        .take()
        .expect("the worker is only taken once")
        .join()
        .expect("the run thread finishes")
        .expect("the run is clean")
}

#[test]
fn boot_ticks_simulated_ctrl_c_and_shutdown_follow_the_documented_order() {
    let tick_log = Arc::new(Mutex::new(Vec::<u64>::new()));
    let counter: Box<dyn TickParticipant> = Box::new(Counter {
        log: Arc::clone(&tick_log),
    });
    let running = start("[server]\nshutdown_timeout_secs = 600\n", vec![counter]);
    wait_for(&running, 10);

    // A simulated ctrl-C is indistinguishable from the real keypress: the
    // same handle, the same flag, the same between-ticks observation.
    assert!(running.stop.request_stop(), "the first stop request wins");
    let report = finish(running);

    // The phase transcript is the whole point: four starts, four stops,
    // perfectly mirrored.
    assert_eq!(
        report.transcript_pairs(),
        vec![
            (Phase::ConfigLoad, Direction::Start),
            (Phase::WorldDirs, Direction::Start),
            (Phase::NetworkBind, Direction::Start),
            (Phase::TickLoop, Direction::Start),
            (Phase::TickLoop, Direction::Stop),
            (Phase::NetworkBind, Direction::Stop),
            (Phase::WorldDirs, Direction::Stop),
            (Phase::ConfigLoad, Direction::Stop),
        ]
    );
    assert!(report.transcript.iter().all(|e| !e.detail.is_empty()));

    // Clean means all of it: nothing interrupted, nothing panicked, the
    // single spawned thread retired.
    assert!(report.is_clean());
    assert!(!report.interrupted);
    assert!(report.thread_panics.is_empty());
    assert_eq!(report.threads_joined, 1, "only the watchdog was spawned");
    assert!(!report.watchdog_fired);
    assert!(report.uptime_ns > 0);

    // Ticks are deterministic under virtual time: contiguous indexes, every
    // one delivered, none invented.
    let ticks = report.ticks_run;
    assert!(ticks >= 10, "{ticks}");
    assert_eq!(
        *tick_log.lock().unwrap(),
        (0..ticks).collect::<Vec<_>>(),
        "the counter saw exactly ticks 0..{}",
        ticks
    );

    // Configuration reached the registry: the two config-gated placeholders
    // and the injected participant all ran, in priority order.
    assert_eq!(
        report.participants,
        vec!["status-probe", "counter", "jvm-placeholder", "ore-workload"]
    );
}

#[test]
fn a_stalled_loop_honours_the_configured_catchup_cap() {
    // max_catchup_ticks = 3, and every pass parks across thirty seconds of
    // virtual time. Each pass must repay exactly three ticks, surrender the
    // rest, and resynchronise — forever, without falling behind further.
    let clock = Arc::new(ManualClock::new());
    let options = ServerOptions {
        config_path: write_config("[server]\nmax_catchup_ticks = 3\n"),
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        loop_parker: stepping(clock, 600 * TICK_NS),
        watchdog: WatchdogSetting::Disabled,
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    let metrics = server.metrics();
    let stop = server.stop_handle();
    let worker = std::thread::spawn(move || server.run());

    wait_for_ticks(&metrics, 12);
    assert!(stop.request_stop());
    let report = worker.join().expect("finishes").expect("clean");

    assert!(
        report.surrendered_batches >= 4,
        "{:?}",
        report.surrendered_batches
    );
    assert_eq!(
        report.ticks_run,
        report.surrendered_batches * 3,
        "every pass repaid exactly the configured three ticks"
    );
    assert!(report.overall_timing.window_samples > 0, "timing survived");
}

fn wait_for_ticks(metrics: &LiveMetrics, minimum: u64) {
    for _ in 0..50_000_000 {
        if metrics.ticks_observed() >= minimum {
            return;
        }
        std::thread::yield_now();
    }
    panic!("never reached {minimum} ticks");
}

#[test]
fn an_environment_override_reaches_the_operator_visible_surface() {
    // Container operators configure by environment; the dry-run summary is
    // the operator-visible face of configuration, so an override must be
    // indistinguishable there from a typed line in the file. The consumption
    // side — these values becoming engine cap, grace and log filter — is
    // proven by unit tests inside the crate, which can reach the runtime
    // structs without touching process state.
    let config = DustConfig::from_toml_and_env(
        "[server]\nmax_players = 40\n",
        "test",
        [
            ("DUST__SERVER__MAX_CATCHUP_TICKS".to_owned(), "7".to_owned()),
            (
                "DUST__SERVER__SHUTDOWN_TIMEOUT_SECS".to_owned(),
                "42".to_owned(),
            ),
            ("DUST__SERVER__LOG_LEVEL".to_owned(), "debug".to_owned()),
        ],
    )
    .expect("valid");
    let summary = dust_server::cli::render_summary(&config);
    assert!(summary.contains("max_catchup_ticks     7"), "{summary}");
    assert!(summary.contains("shutdown_timeout_secs 42s"), "{summary}");
    assert!(summary.contains("log_level             debug"), "{summary}");
}
