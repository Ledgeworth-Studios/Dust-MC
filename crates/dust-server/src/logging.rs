//! A structured line logger with no dependencies, because the dependency
//! graph had none to offer.
//!
//! The workspace carried no `log`, `tracing` or anything similar when this
//! crate was built (checked against `Cargo.lock`, not memory), and Phase 3's
//! needs are modest: level filtering, timestamps, target prefixes, one line
//! per event. Adopting a framework here would have added a dependency for
//! formatting the crate could do itself; writing a logging *framework* is a
//! different project entirely and this module is careful not to start it.
//!
//! The line format is deliberately boring and machine-greppable:
//!
//! ```text
//! 2026-08-25T12:34:56.789Z [INFO ] [dust::server] listening placeholder ready
//! ```
//!
//! Two properties matter more than prettiness. First, **timestamps come from
//! the injected [`Clock`]** in nanoseconds-since-epoch terms, so tests can
//! assert on exact output without freezing the real time — the wall-clock
//! conversion happens only at the formatting step. Second, **a write failure
//! is swallowed**: a logger that panics because stdout closed takes the
//! server down with it, which is the tail wagging the dog.

use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::clock::Clock;

/// Log severity. Lower discriminant = more severe, so "at least as important
/// as the filter" is an integer comparison rather than a match statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    /// The fixed-width spelling used in log lines.
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display shows the trimmed name for contexts where padding would be
        // noise; the padded form lives in `label` for column alignment.
        let trimmed = self.label().trim_end();
        f.write_str(trimmed)
    }
}

/// Where rendered lines go.
type Sink = Arc<Mutex<dyn Write + Send>>;

/// The logger every phase and participant receives.
///
/// It is cheap to clone by arc and safe to share across threads; the mutex is
/// only ever held for one line's worth of writing so interleaving cannot tear
/// a line in half.
///
/// Timestamps are `[Clock] reading + epoch anchor`. The anchor exists because
/// the production clock counts nanoseconds since process start — the right
/// thing for deadlines, the wrong thing for a calendar — so [`to_stdout`]
/// anchors once to the wall at construction and every line shows true UTC
/// from there on. Virtual-time setups leave the anchor at zero, where the
/// epoch *is* time zero and tests can assert on exact output. The anchor is
/// never re-read: a log line that jumps backwards when NTP corrects the host
/// is worse than one that drifts by milliseconds over a month.
#[derive(Clone)]
pub struct Logger {
    sink: Sink,
    filter: Level,
    clock: Arc<dyn Clock>,
    epoch_ns: u64,
}

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Logger")
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// A logger writing to `sink`, showing levels at or above `filter`
    /// severity, stamping lines from `clock` with no epoch anchor: a zero
    /// reading formats as 1970, which is exactly what virtual time wants.
    pub fn new(sink: Sink, filter: Level, clock: Arc<dyn Clock>) -> Self {
        Self {
            sink,
            filter,
            clock,
            epoch_ns: 0,
        }
    }

    /// A logger writing to standard output.
    pub fn to_stdout(filter: Level, clock: Arc<dyn Clock>) -> Self {
        Self::new(Arc::new(Mutex::new(std::io::stdout())), filter, clock)
    }

    /// Anchor the timestamps to the wall: a zero clock reading now formats as
    /// the moment this was called, and everything after is real UTC.
    ///
    /// Production runs this once, at construction. The anchor is deliberately
    /// never re-read — a log that jumps when NTP corrects the host is harder
    /// to grep than one that drifts by milliseconds over a month — and
    /// virtual-time tests leave it at zero so they can assert exact output.
    pub fn anchored_to_unix_now(self) -> Self {
        Self {
            epoch_ns: Self::unix_epoch_ns_now(),
            ..self
        }
    }

    /// The same anchoring with the epoch named explicitly, which is what
    /// tests use: an assertion on rendered output must not depend on when the
    /// test ran.
    pub fn anchored_to_unix_now_at(self, epoch_ns: u64) -> Self {
        Self { epoch_ns, ..self }
    }

    /// Nanoseconds since the Unix epoch, read once.
    pub fn unix_epoch_ns_now() -> u64 {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(since_epoch.as_nanos()).unwrap_or(u64::MAX)
    }

    /// The same logger, showing levels at or above `filter` instead.
    ///
    /// This is how configuration reaches the sink: the server builds its
    /// logger before it has read any configuration, then re-filters once the
    /// file says how loud to be. Sinks and clocks are shared, so lines already
    /// written stay where they are and only the gate moves.
    pub fn with_filter(&self, filter: Level) -> Self {
        Self {
            filter,
            ..self.clone()
        }
    }

    /// Whether `level` passes the current filter.
    pub fn enabled(&self, level: Level) -> bool {
        level <= self.filter
    }

    /// Emit one line if `level` passes the filter.
    pub fn log(&self, level: Level, target: &str, message: impl fmt::Display) {
        if !self.enabled(level) {
            return;
        }
        let line = format!(
            "{} [{}] [{}] {message}\n",
            iso8601_utc(self.epoch_ns.saturating_add(self.clock.now_ns())),
            level.label(),
            target,
        );
        // Best-effort by design; see the module docs for why a failed write
        // must not become a crash.
        let _ = self.sink.lock().map(|mut w| w.write_all(line.as_bytes()));
    }

    pub fn error(&self, target: &str, message: impl fmt::Display) {
        self.log(Level::Error, target, message);
    }

    pub fn warn(&self, target: &str, message: impl fmt::Display) {
        self.log(Level::Warn, target, message);
    }

    pub fn info(&self, target: &str, message: impl fmt::Display) {
        self.log(Level::Info, target, message);
    }

    pub fn debug(&self, target: &str, message: impl fmt::Display) {
        self.log(Level::Debug, target, message);
    }

    pub fn trace(&self, target: &str, message: impl fmt::Display) {
        self.log(Level::Trace, target, message);
    }
}

/// Render nanoseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
///
/// The civil-date conversion is Howard Hinnant's `civil_from_days` algorithm:
/// days since 1970-01-01 in, year/month/day out, valid across the whole range
/// a u64 of nanoseconds can express. Doing it by hand keeps this crate at
/// zero dependencies; keeping it in one tested function keeps the arithmetic
/// from leaking into call sites that only wanted a timestamp.
fn iso8601_utc(ns_since_epoch: u64) -> String {
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    const SECS_PER_DAY: u64 = 86_400;
    const DAYS_TO_MAR_1970: i64 = 719_468;

    let secs = ns_since_epoch / NANOS_PER_SEC;
    let millis = (ns_since_epoch % NANOS_PER_SEC) / 1_000_000;
    let days = i64::try_from(secs / SECS_PER_DAY).unwrap_or(i64::MAX);
    let secs_of_day = secs % SECS_PER_DAY;

    // Shift the epoch to the start of March in the year 0 CE, where leap-day
    // rules are uniform: a year is leap iff divisible by 4, except centuries
    // unless divisible by 400.
    let z = days + DAYS_TO_MAR_1970;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 {
        yoe + era * 400 + 1
    } else {
        yoe + era * 400
    };

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    /// A tiny in-memory sink shared by these tests.
    #[derive(Default)]
    struct Buffer {
        bytes: Vec<u8>,
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.clone()).expect("tests write UTF-8")
        }
    }

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn logger_at(filter: Level) -> (Logger, Arc<ManualClock>, Arc<Mutex<Buffer>>) {
        let buffer: Arc<Mutex<Buffer>> = Arc::default();
        let clock = Arc::new(ManualClock::new());
        let sink: Sink = Arc::clone(&buffer) as Sink;
        (
            Logger::new(sink, filter, Arc::clone(&clock) as Arc<dyn Clock>),
            clock,
            buffer,
        )
    }

    #[test]
    fn a_line_carries_timestamp_level_target_and_message() {
        let (logger, clock, buffer) = logger_at(Level::Info);
        clock.set_ns(1_787_661_296_789_000_000); // 2026-08-25T12:34:56.789Z
        logger.info("dust::server", "boot complete");
        assert_eq!(
            buffer.lock().unwrap().text(),
            "2026-08-25T12:34:56.789Z [INFO ] [dust::server] boot complete\n"
        );
    }

    #[test]
    fn the_filter_hides_everything_less_severe_than_itself() {
        let (logger, _, buffer) = logger_at(Level::Warn);
        logger.trace("t", "hidden");
        logger.debug("t", "hidden");
        logger.info("t", "hidden");
        logger.warn("t", "shown");
        logger.error("t", "shown");
        let text = buffer.lock().unwrap().text();
        assert_eq!(text.matches("shown\n").count(), 2);
        assert!(!text.contains("hidden"));
    }

    #[test]
    fn timestamps_follow_the_injected_clock_not_the_wall() {
        let (logger, clock, buffer) = logger_at(Level::Trace);
        clock.set_ns(0);
        logger.info("t", "epoch");
        assert!(buffer
            .lock()
            .unwrap()
            .text()
            .starts_with("1970-01-01T00:00:00.000Z"));
    }

    #[test]
    fn an_anchored_logger_shows_wall_time_offset_by_the_virtual_reading() {
        let (unanchored, clock, buffer) = logger_at(Level::Info);
        // The production shape: a clock reading from an arbitrary origin,
        // anchored once so a zero reading displays as the moment of anchor.
        let anchor_ns = 1_787_661_296_789_000_000;
        let logger = unanchored.anchored_to_unix_now_at(anchor_ns);
        clock.set_ns(0);
        logger.info("t", "anchor moment");
        assert_eq!(
            buffer.lock().unwrap().text(),
            "2026-08-25T12:34:56.789Z [INFO ] [t] anchor moment\n",
            "a zero reading displays as the anchor, not as the epoch"
        );
        // Half a second later on the injected clock is half a second later in
        // the rendered world: the anchor moves with the readings.
        clock.advance_ns(500_000_000);
        logger.info("t", "half past");
        assert!(
            buffer
                .lock()
                .unwrap()
                .text()
                .contains("2026-08-25T12:34:57.289Z"),
            "{}",
            buffer.lock().unwrap().text()
        );
    }

    #[test]
    fn the_epoch_formats_as_the_first_second_of_1970() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso8601_utc(1_787_661_296_789_000_000),
            "2026-08-25T12:34:56.789Z"
        );
        assert_eq!(
            iso8601_utc(951_782_400_000_000_000),
            "2000-02-29T00:00:00.000Z",
            "the century leap rule survives the conversion"
        );
    }

    #[test]
    fn a_failing_sink_never_panics_the_caller() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let clock = Arc::new(ManualClock::new());
        let logger = Logger::new(
            Arc::new(Mutex::new(Closed)),
            Level::Info,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        logger.info("t", "dropped on the floor");
    }
}
