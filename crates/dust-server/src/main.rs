//! The `dust` binary: a thin shell over the library.
//!
//! Everything interesting lives behind [`dust_server`]; this file parses the
//! command line, installs the stop-signal handler and turns results into exit
//! codes. Its one policy decision is written down where it is made: signal
//! registration failure is a warning, not an abort, because a server that
//! cannot hear ctrl-C still serves players — the operator just has to stop it
//! another way.
//!
//! **SIGTERM stops the server the same way ctrl-C does**, which is the whole
//! reason `ctrlc`'s `termination` feature is on. Nothing at a keyboard sends
//! SIGTERM; `systemctl stop`, `docker stop` and a supervisor restart all do,
//! and those are how a real server is stopped. Without it the default
//! disposition killed the process outright and the world went back to its
//! last save: a furnace armed with 1,512 ticks of coal and stopped with
//! `kill` came back cold, 3 of 9 restart checks passing, and the same `kill
//! -INT` passed 9 of 9. Priority 1 — a player who logs in after an operator
//! restart and finds the last hour undone has lost real work.

use std::io::Write;

use dust_server::cli::{self, Command, EXIT_CONFIG_INVALID, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use dust_server::server::{Server, ServerOptions};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("{e}\n\n{}", cli::usage_text());
            return EXIT_USAGE;
        }
    };

    let invocation = match command {
        Command::Help => {
            println!("{}", cli::usage_text());
            return EXIT_OK;
        }
        Command::Version => {
            println!("{}", cli::version_text());
            return EXIT_OK;
        }
        Command::Server(invocation) => invocation,
    };

    if invocation.dry_run {
        return dry_run(&invocation.config_path);
    }

    let options = ServerOptions {
        config_path: invocation.config_path,
        ..ServerOptions::default()
    };
    let server = Server::new(options);
    let stop = server.stop_handle();
    // Fires on SIGINT (ctrl-C), SIGTERM and SIGHUP; see the module note.
    match ctrlc::set_handler(move || {
        stop.request_stop();
    }) {
        Ok(()) => {}
        Err(e) => {
            // Without the handler, ctrl-C keeps its default disposition: the
            // next keypress kills the process outright. Say so loudly and
            // keep serving.
            eprintln!(
                "[dust] warning: could not install the stop-signal handler ({e}); \
                 ctrl-C and SIGTERM will terminate abruptly, losing anything \
                 not yet saved"
            );
        }
    }

    match server.run() {
        Ok(report) => {
            for entry in &report.transcript {
                println!("[lifecycle] {entry}");
            }
            println!(
                "[report] {} tick(s) in {:.3}s of uptime, {} thread(s) joined",
                report.ticks_run,
                report.uptime_ns as f64 / 1_000_000_000.0,
                report.threads_joined,
            );
            if !report.thread_panics.is_empty() {
                eprintln!(
                    "[dust] thread(s) panicked during shutdown: {}",
                    report.thread_panics.join(", ")
                );
                return EXIT_FAILURE;
            }
            EXIT_OK
        }
        Err(e) => {
            eprintln!("[dust] startup failed: {e}");
            for entry in e.transcript() {
                eprintln!("[lifecycle] {entry}");
            }
            match e {
                dust_server::ServerError::Config(_) => EXIT_CONFIG_INVALID,
                _ => EXIT_FAILURE,
            }
        }
    }
}

/// Load, validate, describe, leave. Nothing here starts phases or threads.
fn dry_run(config_path: &std::path::Path) -> i32 {
    match dust_config::DustConfig::load(config_path) {
        Ok(config) => {
            let summary = cli::render_summary(&config);
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(summary.as_bytes());
            let _ = out.flush();
            EXIT_OK
        }
        Err(e) => {
            eprintln!("[dust] {e}");
            EXIT_CONFIG_INVALID
        }
    }
}
