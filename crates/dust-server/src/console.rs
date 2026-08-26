//! The operator's line into a running server.
//!
//! # Why this is a thread and not a tick participant
//!
//! Reading a line from a terminal blocks until somebody presses return, and
//! there is no portable way to ask whether one is waiting. A tick participant
//! that read the console would stall the world for as long as nobody typed.
//! So the console owns a thread, and what it produces are [`Command`]s handed
//! to a handler that runs on that same thread.
//!
//! The consequence is worth stating: **a console command does not run inside a
//! tick.** Everything one can currently do — stop, list players, say something
//! — is either an atomic or a lock the network already takes from its own
//! threads, so that is safe today. A command that mutated the world would need
//! to be queued for the tick loop rather than run here, and the day one exists
//! this is where the queue goes.
//!
//! # Why the thread is not joined
//!
//! A blocking read on stdin cannot be cancelled. Asking the thread to stop
//! would mean waiting for the operator to press return, which is exactly the
//! wrong thing to do during a shutdown — so the thread is detached and the
//! process exits out from under it. That is safe because it holds nothing the
//! shutdown needs and writes nothing that has to be flushed.

use std::io::BufRead;
use std::sync::Arc;

use crate::logging::Logger;

/// What the operator typed, once it has been understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Shut the server down, the same as ctrl-C.
    Stop,
    /// Who is connected.
    List,
    /// Send a line to everybody's chat.
    Say(String),
    /// Something that is not a command. The text is kept so the reply can name
    /// it — "unknown command" without the word is a reply that helps nobody.
    Unknown(String),
}

/// Parse one line.
///
/// Vanilla's console takes commands without a leading slash and Dust does the
/// same, because that is what an operator's fingers already do. A leading slash
/// is accepted too rather than being a mistake with a lecture attached.
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim();
    let line = line.strip_prefix('/').unwrap_or(line);
    if line.is_empty() {
        // A bare return is not a command and not an error. Vanilla prints
        // nothing; so does this.
        return None;
    }
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    };
    Some(match word.to_ascii_lowercase().as_str() {
        "stop" => Command::Stop,
        "list" => Command::List,
        "say" if !rest.is_empty() => Command::Say(rest.to_owned()),
        // `say` with nothing after it is the unknown case rather than an empty
        // message: sending a blank line to everybody is never what was meant.
        _ => Command::Unknown(line.to_owned()),
    })
}

/// Read commands from standard input until it ends, handing each to `run`.
///
/// Returns when stdin closes — a server started with its input redirected from
/// `/dev/null`, or one whose terminal went away. That is not a reason to stop
/// serving, so the caller is expected to let the thread end quietly.
pub fn read_from(input: impl BufRead, logger: &Logger, mut run: impl FnMut(Command)) {
    for line in input.lines() {
        let Ok(line) = line else {
            // A read error on stdin ends the console and nothing else. A
            // server whose terminal broke is still a server.
            logger.debug("dust::console", "standard input ended");
            return;
        };
        if let Some(command) = parse(&line) {
            run(command);
        }
    }
    logger.debug("dust::console", "standard input closed");
}

/// Start the console on its own detached thread.
pub fn spawn(logger: Logger, run: impl FnMut(Command) + Send + 'static) {
    let mut run = run;
    let reporter = logger.clone();
    std::thread::Builder::new()
        .name("dust-console".to_owned())
        .spawn(move || {
            let stdin = std::io::stdin();
            read_from(stdin.lock(), &logger, &mut run);
        })
        // A server that could not start a console thread is still a server;
        // ctrl-C still stops it.
        .map(|_| ())
        .unwrap_or_else(|e| {
            reporter.warn(
                "dust::console",
                format!("no console: the reader thread would not start ({e})"),
            );
        });
}

/// Everything a console command is allowed to reach.
///
/// Named as a type rather than passed as four arguments so that what the
/// console can touch is a list somebody can read — and so that adding to it is
/// a visible decision rather than one more parameter.
#[derive(Debug)]
pub struct Console {
    pub stop: crate::stop::StopHandle,
    pub roster: Arc<crate::net::players::Roster>,
    pub logger: Logger,
}

impl Console {
    /// Do what the command says, and say what was done.
    pub fn run(&self, command: Command) {
        match command {
            Command::Stop => {
                self.logger.info("dust::console", "stopping");
                self.stop.request_stop();
            }
            Command::List => {
                let players = self.roster.snapshot();
                let names: Vec<&str> = players.iter().map(|p| p.name.as_str()).collect();
                self.logger.info(
                    "dust::console",
                    if names.is_empty() {
                        "nobody is connected".to_owned()
                    } else {
                        format!("{} connected: {}", names.len(), names.join(", "))
                    },
                );
            }
            Command::Say(message) => {
                // Announced to everybody and echoed to the console, because an
                // operator who sees no confirmation types it again.
                self.roster.say(0, crate::net::chat::server_said(&message));
                self.logger
                    .info("dust::console", format!("[Server] {message}"));
            }
            Command::Unknown(text) => {
                self.logger.info(
                    "dust::console",
                    format!("unknown command: {text:?} — try stop, list or say"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_that_exist_are_recognised_with_or_without_a_slash() {
        assert_eq!(parse("stop"), Some(Command::Stop));
        assert_eq!(parse("/stop"), Some(Command::Stop));
        assert_eq!(parse("  STOP  "), Some(Command::Stop), "and case-folded");
        assert_eq!(parse("list"), Some(Command::List));
        assert_eq!(
            parse("say hello there"),
            Some(Command::Say("hello there".to_owned()))
        );
    }

    #[test]
    fn a_blank_line_is_not_a_command_and_not_an_error() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse("/"), None);
        assert_eq!(parse("\t\n"), None);
    }

    #[test]
    fn say_with_nothing_to_say_is_unknown_rather_than_an_empty_message() {
        // Sending a blank line to every player is never what was meant, and
        // silently doing nothing would leave an operator wondering.
        assert_eq!(parse("say"), Some(Command::Unknown("say".to_owned())));
        assert_eq!(parse("say   "), Some(Command::Unknown("say".to_owned())));
    }

    #[test]
    fn an_unknown_command_keeps_its_text_so_the_reply_can_name_it() {
        assert_eq!(
            parse("teleport me somewhere"),
            Some(Command::Unknown("teleport me somewhere".to_owned()))
        );
    }

    #[test]
    fn the_message_of_a_say_keeps_its_own_spacing_and_slashes() {
        // Only the leading slash of the *command* is stripped. A message that
        // mentions a path or a command should arrive as typed.
        assert_eq!(
            parse("say try /stop"),
            Some(Command::Say("try /stop".to_owned()))
        );
    }

    #[test]
    fn reading_stops_when_the_input_does_and_runs_what_it_read() {
        let input = "list\n\nsay hello\nnonsense\nstop\n";
        let logger = Logger::to_stdout(
            crate::logging::Level::Error,
            std::sync::Arc::new(crate::clock::ManualClock::new()),
        );
        let mut seen = Vec::new();
        read_from(std::io::Cursor::new(input), &logger, |command| {
            seen.push(command);
        });
        assert_eq!(
            seen,
            vec![
                Command::List,
                Command::Say("hello".to_owned()),
                Command::Unknown("nonsense".to_owned()),
                Command::Stop,
            ],
            "the blank line produced nothing and everything else ran in order"
        );
    }
}
