//! The connection driver: one socket, framed in both directions, with the
//! liveness and backpressure policy wrapped around the frame codec.
//!
//! Everything before this module is pure: codecs that transform bytes without
//! ever touching a socket. This module is where the socket arrives, and it
//! exists because three questions have no answer until bytes can actually stop
//! flowing:
//!
//! * **What if the peer stops talking?** Every pre-play state except
//!   [`State::Configuration`] and [`State::Play`] is reachable by an
//!   unauthenticated stranger. A peer that connects and then sends nothing —
//!   or one harmless byte every few seconds, forever — must cost the server a
//!   bounded amount of attention. That is the timeout policy below, and it is
//!   two clocks rather than one: an idle timeout for silence, plus a total
//!   wall-clock budget for the unauthenticated phase that activity cannot
//!   refill. Silence alone does not catch the dribbler; only the budget does.
//! * **What if the peer stops listening?** The sending side of a stream is a
//!   buffer someone else drains. Writing into a full buffer blocks, and a
//!   server that answers that by queueing frames without limit has been turned
//!   into a memory leak by a client that reads nothing. [`Conn::send`] waits
//!   for queue room instead: a slow consumer costs the sender latency, not the
//!   server memory.
//! * **How does a connection end?** Two ways, guaranteeing different things.
//!   [`Conn::close`] flushes everything already accepted for sending and then
//!   shuts the stream down; [`Conn::abort`] discards whatever is queued and
//!   stops spending resources immediately, accepting that the peer observes a
//!   truncated stream. Both are needed: the first is how a finished login says
//!   goodbye, the second is how a server defends itself.
//!
//! # What this module does not do
//!
//! It does not know what any packet means, and it does not decide when the
//! state machine moves. The driver hands up [`Frame`]s and takes transitions
//! from above, exactly as the `dust-protocol` seam requires; the one thing it
//! adds is that the *timeout policy reads the state* — the pre-authentication
//! budget applies only while [`State::is_pre_authentication`] says it should.
//!
//! It does pace, but only per connection: the optional inbound bucket from
//! [`crate::limits`] is charged against raw bytes before the codec runs,
//! so a peer's share of the server's attention is whatever its own policy
//! allows and nothing else. What this layer still does not see is *other*
//! connections. A global cap on live connections is a semaphore with a
//! number somebody else chose — [`crate::limits::AdmissionGate`] provides
//! the type, and the accept loop above owns the decision.
//!
//! # Delivery semantics, stated precisely
//!
//! [`send`](Conn::send) returns `Ok` once the frame is **accepted**, meaning
//! ordered behind everything previously accepted and handed to the writer
//! task — not yet necessarily on the wire. This is what makes the outbound
//! bound enforceable: waiting for each frame to be written would leave the
//! queue empty and the bound untested by traffic; accepting up to the bound
//! and then blocking is the shape that survives a consumer that stops.
//! The cost is honest too: a write failure poisons the connection, and the
//! error surfaces on the next operation that touches the sending side. A
//! caller that needs certainty about a particular frame uses
//! [`close`](Conn::close), whose acknowledgement covers everything accepted
//! before it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::crypt::{Cipher, SharedSecret, SHARED_SECRET_LEN};
use crate::frame::{Compress, Frame, FrameDecoder, FrameEncoder, FrameError, Limits, Needed};
use crate::limits::{InboundRate, TokenBucket};
use crate::metrics::{ConnCounters, StatsSnapshot};
use crate::state::{Connection as StateMachine, HandshakeError, IllegalTransition, Intent, State};
use crate::varint::MAX_VAR_INT_LEN;

/// How long a connection may sit silent, and how long it may spend getting
/// through the unauthenticated part of its life.
///
/// Two different clocks, because two different failures:
///
/// * `idle` bounds **silence**. It applies fresh to every read: any byte
///   received restarts it. Its purpose is noticing a peer that has gone away
///   without closing — a laptop that lost its network, a scanner that opened
///   a socket and wandered off — and it applies to authenticated connections
///   just the same, because "in the world" is not "allowed to hold a socket
///   open forever without speaking".
/// * `pre_auth_budget` bounds the **whole unauthenticated phase**, wall clock,
///   regardless of activity. An idle timeout alone is defeated by the client
///   that dribbles one byte every few seconds and gets nowhere; a total
///   budget is not refilled by progress that never finishes. It starts at
///   [`Conn::new`] and stops applying the moment the state machine leaves the
///   pre-authentication states, so a legitimate player on a bad link pays it
///   only until authentication completes, never afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Maximum silence between received bytes. `None` disables it and waits
    /// forever, which exists for tests and closed networks; a public server
    /// wants a value here.
    pub idle: Option<Duration>,
    /// Wall-clock budget covering everything before authentication. `None`
    /// imposes no total limit; see the type docs for why a public server
    /// wants one anyway.
    pub pre_auth_budget: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            idle: Some(Duration::from_secs(10)),
            pre_auth_budget: Some(Duration::from_secs(30)),
        }
    }
}

/// Everything a connection needs configured, in one place.
///
/// Defaults are chosen for a public internet-facing server: vanilla's own
/// frame caps, ten seconds of silence, thirty seconds to authenticate, a
/// modest outbound queue, 8 KiB read chunks. Tests shrink every one of them,
/// which is the point of them being configurable at all.
#[derive(Debug, Clone)]
pub struct ConnConfig {
    /// Frame size bounds, enforced by the codec underneath this driver. See
    /// [`Limits`] and the seven defences of [`crate::frame`].
    pub limits: Limits,
    /// The liveness policy. See [`Timeouts`].
    pub timeouts: Timeouts,
    /// The maximum number of frames queued for sending at once.
    ///
    /// This is the backpressure bound. When the queue holds this many frames
    /// — which means the writer is stuck writing into a stream nobody is
    /// draining — [`Conn::send`] waits until there is room again. The bound
    /// counts queued frames plus the one possibly mid-write; it never grows
    /// with how long the peer stalls.
    pub outbound_capacity: usize,
    /// The largest chunk pulled from the socket per read.
    ///
    /// The decoder's buffer is bounded by one maximum frame plus one read
    /// chunk, so this number appears directly in that bound. See
    /// [`FrameDecoder`].
    pub read_chunk: usize,
    /// The inbound pacing policy, applied to raw socket bytes before the
    /// decoder — and therefore before decompression — ever sees them. See
    /// [`InboundRate`] and [`crate::limits`] for why it lives there.
    ///
    /// `None` disables pacing entirely: every read proceeds at line rate,
    /// and the only bounds on a peer's cost are the frame limits and the
    /// liveness clocks. The default is generous rather than absent, for the
    /// reason given in [`InboundRate::generous`]: an unpaced connection is
    /// exactly as expensive to attack with as to use.
    pub inbound_rate: Option<InboundRate>,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            timeouts: Timeouts::default(),
            outbound_capacity: 64,
            read_chunk: 8 * 1024,
            inbound_rate: Some(InboundRate::generous()),
        }
    }
}

/// Why a connection ended, or why an operation on it failed.
///
/// Every variant here is terminal. After a malformed frame the stream is
/// literally unresynchronizable — there is no way to know where the next
/// frame starts — and after a timeout or a write failure it is practically
/// so. Nothing in this type invites a retry, and callers close.
#[derive(Debug)]
pub enum ConnError {
    /// The socket failed underneath the driver.
    Io(std::io::Error),
    /// The codec refused a frame — inbound, or outbound at encode time. Each
    /// [`FrameError`] variant names the input that produced it.
    Protocol(FrameError),
    /// The layer above asked for a transition the state machine forbids.
    Illegal(IllegalTransition),
    /// The handshake's next-state field named no intent at all (see
    /// [`UnknownIntent`](crate::state::UnknownIntent)). Kept distinct from
    /// [`ConnError::Illegal`] because "the client asked for nothing that
    /// exists" and "the caller broke the table" are different log lines.
    Handshake(HandshakeError),
    /// The connection had already ended when this operation ran. The error
    /// that originally ended it was reported to whoever saw it first; see
    /// the delivery-semantics note in the module docs.
    Closed,
    /// No bytes arrived within the idle budget.
    IdleTimeout { limit: Duration },
    /// The wall-clock budget for the unauthenticated phase ran out. Unlike
    /// the idle timeout this can fire *despite steady traffic*; see
    /// [`Timeouts::pre_auth_budget`].
    PreAuthDeadline { budget: Duration },
    /// The peer hung up with part of a frame still missing. Whether the
    /// missing bytes were lost, delayed or never sent is unknowable, and the
    /// framing cannot resume either way.
    TruncatedFrame { pending: usize },
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "the socket failed: {error}"),
            Self::Protocol(error) => write!(f, "the connection carried a bad frame: {error}"),
            Self::Illegal(error) => write!(f, "{error}"),
            Self::Handshake(error) => write!(f, "{error}"),
            Self::Closed => f.write_str("the connection has already ended"),
            Self::IdleTimeout { limit } => write!(
                f,
                "the peer sent nothing for {limit:?}; the idle timeout ended the connection"
            ),
            Self::PreAuthDeadline { budget } => write!(
                f,
                "the connection spent more than its whole {budget:?} budget before \
                 authenticating"
            ),
            Self::TruncatedFrame { pending } => write!(
                f,
                "the peer hung up with a frame {pending} byte(s) short of complete"
            ),
        }
    }
}

impl std::error::Error for ConnError {}

impl From<FrameError> for ConnError {
    fn from(error: FrameError) -> Self {
        Self::Protocol(error)
    }
}

impl From<IllegalTransition> for ConnError {
    fn from(error: IllegalTransition) -> Self {
        Self::Illegal(error)
    }
}

impl From<HandshakeError> for ConnError {
    fn from(error: HandshakeError) -> Self {
        match error {
            HandshakeError::Illegal(inner) => Self::Illegal(inner),
            other => Self::Handshake(other),
        }
    }
}

impl From<std::io::Error> for ConnError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// One end's view of a framed connection over any byte stream.
///
/// Generic rather than tied to TCP on purpose: the same driver runs a real
/// socket in production and an in-memory duplex in the tests that prove what
/// happens when a peer stalls, lies or stops listening. Nothing in the driver
/// branches on which kind it has, which is the argument that what those tests
/// show is a fact about production and not about the test harness.
///
/// Internally the driver is split. A writer task owns the sending half and is
/// fed by a bounded queue of encoded frames; whoever holds the [`Conn`] drives
/// the reading half through [`next_frame`](Self::next_frame). The two mode
/// changes land in exactly the right place in the outbound byte stream, by
/// construction rather than by discipline: compression is applied when a frame
/// is *accepted*, so its bytes are already frozen before they can be queued
/// behind anything else, and encryption is applied when a frame is *written*,
/// as a queued command whose position in the FIFO is the protocol's own
/// position for it. The packet that switches either mode goes out in the old
/// mode because it was accepted first, not because anyone remembered to
/// flush at the right moment.
pub struct Conn<W: AsyncRead + AsyncWrite + Unpin + Send + 'static> {
    reader: ReadHalf<W>,
    decoder: FrameDecoder,
    encoder: FrameEncoder,
    cipher_in: Cipher,
    machine: StateMachine,
    timeouts: Timeouts,
    started_at: Instant,
    scratch: Vec<u8>,
    outbound: mpsc::Sender<Command>,
    /// Frames accepted but not yet taken by the writer. The bounded channel
    /// enforces the cap; this only measures it, because `mpsc::Sender` offers
    /// no length query from the producing side.
    queued: Arc<AtomicUsize>,
    abort_flag: watch::Sender<bool>,
    /// Where the writer records a write failure for the next operation that
    /// cares to look. Shared with the writer task; see `poisoned`.
    failure: Arc<Mutex<Option<ConnError>>>,
    /// The connection's counters, shared with the writer task so both halves
    /// of the stream report into the same totals. See [`crate::metrics`].
    stats: Arc<ConnCounters>,
    /// The inbound pacer, if a policy was configured. Charged against raw
    /// wire bytes in [`fill`](Self::fill), before the decoder runs.
    pacer: Option<TokenBucket>,
    /// Kept so a panic in the writer is noticed rather than detached into
    /// silence; nothing joins this handle in the normal path.
    _writer: JoinHandle<()>,
    ended: bool,
}

/// What the writer task does, strictly in arrival order.
///
/// Frames arrive already encoded. That is what makes the mode switches work
/// without any coordination: a frame's bytes are frozen in whichever mode was
/// current when it was accepted, so the queue cannot scramble modes no matter
/// how it is drained. What remains for the writer is encryption — which must
/// be a queued concern, because the switch happens mid-stream by protocol
/// position, not per frame — and the write itself.
enum Command {
    Frame(Vec<u8>),
    EnableEncryption {
        secret: [u8; SHARED_SECRET_LEN],
    },
    /// Flush everything queued ahead, shut the stream down, report, exit.
    Finish {
        done: oneshot::Sender<Result<(), ConnError>>,
    },
}

impl<W: AsyncRead + AsyncWrite + Unpin + Send + 'static> Conn<W> {
    /// Split `io`, start the writer task, and start both timeout clocks.
    pub fn new(io: W, config: ConnConfig) -> Self {
        let (reader, writer) = split(io);
        let capacity = config.outbound_capacity.max(1);
        let (outbound_tx, outbound_rx) = mpsc::channel(capacity);
        let (abort_flag, abort_rx) = watch::channel(false);
        let limits = config.limits;
        let failure: Arc<Mutex<Option<ConnError>>> = Arc::default();
        let queued: Arc<AtomicUsize> = Arc::default();
        let stats: Arc<ConnCounters> = Arc::new(ConnCounters::new());
        let handle = tokio::spawn(write_loop(
            writer,
            outbound_rx,
            abort_rx,
            Arc::clone(&failure),
            Arc::clone(&queued),
            Arc::clone(&stats),
        ));
        Self {
            reader,
            decoder: FrameDecoder::new(limits),
            encoder: FrameEncoder::new(limits),
            cipher_in: Cipher::disabled(),
            machine: StateMachine::new(),
            timeouts: config.timeouts,
            started_at: Instant::now(),
            scratch: vec![0u8; config.read_chunk.max(1)],
            pacer: config
                .inbound_rate
                .map(|rate| TokenBucket::new(rate, Instant::now())),
            outbound: outbound_tx,
            queued,
            abort_flag,
            failure,
            stats,
            _writer: handle,
            ended: false,
        }
    }

    // -- The state machine, delegated -------------------------------------

    /// Where the connection is in the protocol. See [`State`].
    pub fn state(&self) -> State {
        self.machine.state()
    }

    /// What the handshake asked for, once it has been applied. See [`Intent`].
    pub fn intent(&self) -> Option<Intent> {
        self.machine.intent()
    }

    /// How many times this connection has entered configuration. See
    /// [`StateMachine::configuration_count`].
    pub fn configuration_count(&self) -> u32 {
        self.machine.configuration_count()
    }

    /// Apply the handshake's next-state field. See
    /// [`StateMachine::handshake`].
    pub fn handshake(&mut self, next_state: i32) -> Result<Intent, HandshakeError> {
        self.machine.handshake(next_state)
    }

    /// Move to another protocol state, checked against the table. See
    /// [`StateMachine::transition`].
    pub fn transition(&mut self, to: State) -> Result<(), IllegalTransition> {
        self.machine.transition(to)
    }

    /// Mark the connection disconnected in the state machine.
    ///
    /// This changes nothing about the transport; ending that is
    /// [`close`](Self::close) or [`abort`](Self::abort). They are separate
    /// on purpose: a caller often wants the state recorded before deciding
    /// whether it owes this peer a flush.
    pub fn disconnect(&mut self) {
        self.machine.disconnect();
    }

    /// How many frames are currently queued for sending, give or take the
    /// instant between the writer taking one and saying so.
    ///
    /// A gauge, not a control: the bound itself lives in
    /// [`ConnConfig::outbound_capacity`] and is enforced by the queue being a
    /// bounded channel — a sender cannot get an acknowledgement past a full
    /// queue no matter what this counter says. Under concurrent producers the
    /// count can briefly read one high, because "taken" is recorded a breath
    /// after the take; treat it as telemetry, never as permission.
    pub fn outbound_queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// A snapshot of this connection's counters: frames and bytes in each
    /// direction, and terminal failures by kind. See [`crate::metrics`] for
    /// what each count means and where it is taken.
    ///
    /// The numbers are exact at the instants their events happened; the
    /// snapshot itself may already be behind a connection that kept moving.
    /// Take another to see later.
    pub fn stats(&self) -> StatsSnapshot {
        let c = &*self.stats;
        StatsSnapshot {
            frames_in: c.frames_in.load(Ordering::Relaxed),
            frames_out: c.frames_out.load(Ordering::Relaxed),
            bytes_in: c.bytes_in.load(Ordering::Relaxed),
            bytes_out: c.bytes_out.load(Ordering::Relaxed),
            protocol_errors: c.protocol_errors.load(Ordering::Relaxed),
            io_errors: c.io_errors.load(Ordering::Relaxed),
            truncated_frames: c.truncated_frames.load(Ordering::Relaxed),
            idle_timeouts: c.idle_timeouts.load(Ordering::Relaxed),
            pre_auth_deadlines: c.pre_auth_deadlines.load(Ordering::Relaxed),
        }
    }

    /// Whether the transport has ended — cleanly, badly, or by local close.
    /// Afterwards every operation fails with [`ConnError::Closed`].
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    // -- Mode switches ------------------------------------------------------

    /// Turn compression on or off, in both directions.
    ///
    /// Outbound, frames are encoded at accept time, so the switch takes
    /// effect on everything accepted from here on and cannot touch anything
    /// already queued: its bytes were frozen in the old mode when they were
    /// accepted. That is exactly the protocol's requirement — including for
    /// the very Set Compression packet that caused this call, which therefore
    /// goes out uncompressed however busy the queue was — with no flush to
    /// remember. Inbound, it takes effect on the next frame decoded, for the
    /// same reason it is correct in [`FrameDecoder::set_compression`]: the
    /// packet announcing the change travels in the old mode.
    ///
    /// This is synchronous because it has nothing to wait for; the queue is
    /// never involved in a mode decision.
    pub fn set_compression(&mut self, compression: Compress) {
        self.encoder.set_compression(compression);
        self.decoder.set_compression(compression);
    }

    /// Start encrypting both directions with the session's shared secret.
    ///
    /// Outbound, the switch is a queued command: every frame accepted before
    /// this call reaches the wire plaintext even if it was still sitting in
    /// the queue, and everything after is encrypted. That is the protocol's
    /// own rule — the Encryption Response carrying the secret is itself the
    /// last plaintext frame either way — and it holds by queue position, so
    /// no flush or ordering discipline is asked of the caller. Inbound,
    /// decryption begins with the next byte handed to
    /// [`next_frame`](Self::next_frame); CFB8 has no block alignment, so
    /// "next byte" is the whole story, and it is safe only because the reader
    /// never decrypts past the frame it is assembling. See [`crate::crypt`]
    /// for why that discipline exists.
    ///
    /// The call waits only for queue room, exactly as [`send`](Self::send)
    /// does; a jammed queue delays the switch, which is the same backpressure
    /// a burst of frames would meet.
    pub async fn enable_encryption(&mut self, secret: &SharedSecret) -> Result<(), ConnError> {
        if self.ended {
            return Err(self.take_failure().unwrap_or(ConnError::Closed));
        }
        self.cipher_in.enable(secret);
        if let Err(error) = self
            .dispatch(Command::EnableEncryption {
                secret: *secret.as_bytes(),
            })
            .await
        {
            self.stats.note_error(&error);
            return Err(error);
        }
        Ok(())
    }

    // -- Reading -------------------------------------------------------------

    /// The next complete frame from the peer, waiting as long as the timeout
    /// policy allows.
    ///
    /// `Ok(None)` is the peer hanging up cleanly between frames: the honest
    /// end of a status ping. `Ok(Some(frame))` is a frame whose length prefix
    /// passed every defence in [`crate::frame`], decompressed if need be,
    /// decrypted if need be. Anything else is an error naming what went
    /// wrong, and the connection is finished — a stream whose framing is in
    /// doubt has no way to resynchronize.
    ///
    /// Each wait is bounded by whichever clock expires first: the idle
    /// timeout, or — while [`is_pre_authentication`](State::is_pre_authentication)
    /// holds — the total unauthenticated budget. Which one fired is part of
    /// the error, because "the peer went quiet" and "the peer stalled us out
    /// on purpose" deserve different log lines.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>, ConnError> {
        if self.ended {
            return Err(self.take_failure().unwrap_or(ConnError::Closed));
        }
        // Every error is terminal, and the type says so to later callers:
        // after a timeout or a bad frame there is no operation this driver
        // can still perform honestly.
        match self.pull_until_frame().await {
            Ok(Some(frame)) => {
                self.stats.frames_in.fetch_add(1, Ordering::Relaxed);
                Ok(Some(frame))
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.ended = true;
                // Counted here, once, at the place the failure became an
                // error; every later call returns Closed, which is not an
                // event and is not counted.
                self.stats.note_error(&error);
                Err(error)
            }
        }
    }

    /// The read loop proper: decode what is buffered, pull what is missing.
    async fn pull_until_frame(&mut self) -> Result<Option<Frame>, ConnError> {
        loop {
            if let Some(frame) = self.decoder.next_frame()? {
                return Ok(Some(frame));
            }
            if !self.fill().await? {
                self.ended = true;
                let pending = self.decoder.buffered();
                if pending > 0 {
                    return Err(ConnError::TruncatedFrame { pending });
                }
                return Ok(None);
            }
        }
    }

    /// Pull bytes from the socket into the decoder. `false` means EOF.
    ///
    /// Reads never cross the frame boundary they are serving, in *either*
    /// mode: the pull is capped at what the current frame still needs, and
    /// while the length prefix is incomplete that cap is **one byte**, plain
    /// connection or not. Under encryption this is obviously load-bearing —
    /// decrypting speculative bytes advances the cipher through frames nobody
    /// has sized yet, the exact "bytes encrypted twice" bug described in
    /// [`crate::crypt`] — but the same discipline is what makes the switch
    /// itself survivable: a peer that pipelines encrypted frames into the
    /// same segment as its Encryption Response finds them still unread,
    /// waiting in the socket, when the cipher turns on. A reader that had
    /// gulped them pre-switch would have committed them as plaintext, and no
    /// amount of switching afterwards undoes that.
    ///
    /// The prefix is five bytes at most, so the price is a handful of
    /// single-byte reads per frame, paid on the straggling head of the
    /// stream only. See [`Needed`].
    ///
    /// When a pacing policy is configured, the pull is capped again to what
    /// the bucket will pay for, and charged after the bytes arrive — before
    /// the decoder, and therefore before decompression, sees any of them.
    async fn fill(&mut self) -> Result<bool, ConnError> {
        let mut wanted = match self.decoder.needed() {
            Needed::Unknown => 1,
            // Zero means the decoder already holds a verdict — a frame or an
            // error — and reading anything would be reading past it.
            Needed::Exactly(0) => return Ok(true),
            Needed::Exactly(more) => more.min(self.scratch.len()).max(1),
        };
        wanted = self.pace(wanted).await?;

        let read = self.reader.read(&mut self.scratch[..wanted]);
        let allowance = liveness::budget(
            self.timeouts.idle,
            self.timeouts.pre_auth_budget,
            self.started_at,
            self.machine.state(),
        );
        let n = match allowance {
            Some((window, kind)) => match tokio::time::timeout(window, read).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(match kind {
                        liveness::Kind::PreAuth => ConnError::PreAuthDeadline {
                            budget: self.timeouts.pre_auth_budget.unwrap_or(window),
                        },
                        liveness::Kind::Idle => ConnError::IdleTimeout { limit: window },
                    });
                }
            },
            None => read.await?,
        };

        if n == 0 {
            return Ok(false);
        }
        // Charged for what actually arrived: a short read costs what it
        // used, not what it was offered. The charge lands before decode or
        // decompress, which is the whole point of the placement.
        if let Some(pacer) = self.pacer.as_mut() {
            pacer.take(n, Instant::now());
        }
        // Counted beside the charge: this is the wire cost the peer chose
        // to impose, whatever decryption and decompression turn it into.
        self.stats.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
        let arrived = &mut self.scratch[..n];
        self.cipher_in.decrypt(arrived);
        self.decoder.feed(arrived);
        Ok(true)
    }

    /// Cap a read length to what the pacer will currently pay for, waiting
    /// — within the liveness budget, never beyond it — until at least one
    /// byte is affordable.
    ///
    /// The wait shares the connection's clocks rather than running its own,
    /// so a flooder cannot convert "slow down" into "stay forever": when the
    /// idle timeout or the pre-authentication budget expires mid-wait, the
    /// same errors fire that a silent peer would have produced. Without a
    /// policy configured this passes its argument straight through.
    async fn pace(&mut self, wanted: usize) -> Result<usize, ConnError> {
        if self.pacer.is_none() {
            return Ok(wanted);
        }
        loop {
            let now = Instant::now();
            let affordable = self.pacer.as_mut().expect("checked above").available(now);
            if affordable >= 1 {
                return Ok((wanted as u64).min(affordable) as usize);
            }

            let wait = self
                .pacer
                .as_mut()
                .expect("checked above")
                .wait_for_one(now);
            let allowance = liveness::budget(
                self.timeouts.idle,
                self.timeouts.pre_auth_budget,
                self.started_at,
                self.machine.state(),
            );
            match allowance {
                Some((window, kind)) => {
                    let nap = tokio::time::sleep(wait);
                    if tokio::time::timeout(window, nap).await.is_err() {
                        return Err(match kind {
                            liveness::Kind::PreAuth => ConnError::PreAuthDeadline {
                                budget: self.timeouts.pre_auth_budget.unwrap_or(window),
                            },
                            liveness::Kind::Idle => ConnError::IdleTimeout { limit: window },
                        });
                    }
                    // Whether another byte became affordable is re-decided
                    // at the top of the loop; waking early converges too.
                }
                None => tokio::time::sleep(wait).await,
            }
        }
    }

    // -- Writing -------------------------------------------------------------

    /// Accept a frame for sending, waiting first for queue room.
    ///
    /// Two waits happen inside a burst of sends, and they mean different
    /// things. Waiting for queue room is backpressure: the peer is not
    /// draining fast enough, and the cure is this caller slowing down, not
    /// the queue growing past [`ConnConfig::outbound_capacity`]. Delivery
    /// itself is asynchronous from here; see the module docs for what `Ok`
    /// promises and what it deliberately does not. An oversized frame is
    /// refused immediately with [`FrameError::Oversize`] — a caller bug
    /// fails here, loudly, instead of poisoning the queue for everyone
    /// behind it.
    pub async fn send(&mut self, frame: Frame) -> Result<(), ConnError> {
        if self.ended {
            return Err(self.take_failure().unwrap_or(ConnError::Closed));
        }
        if let Some(failure) = self.take_failure() {
            self.ended = true;
            self.stats.note_error(&failure);
            return Err(failure);
        }
        // Encoding happens before anything is queued, so an oversized frame
        // is refused here — immediately, whether or not the writer is wedged,
        // and without occupying a queue slot a well-formed frame could use.
        // The connection itself stays healthy: this was a caller bug, not a
        // stream problem.
        let mut wire = Vec::with_capacity(frame.payload_len() + MAX_VAR_INT_LEN);
        self.encoder.encode(&frame, &mut wire)?;
        match self.outbound.send(Command::Frame(wire)).await {
            Ok(()) => {
                self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
                self.queued.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => {
                self.ended = true;
                let error = self.take_failure().unwrap_or(ConnError::Closed);
                self.stats.note_error(&error);
                Err(error)
            }
        }
    }

    /// Queue a command, waiting for queue room.
    async fn dispatch(&mut self, command: Command) -> Result<(), ConnError> {
        match self.outbound.send(command).await {
            Ok(()) => Ok(()),
            Err(_) => {
                self.ended = true;
                Err(self.take_failure().unwrap_or(ConnError::Closed))
            }
        }
    }

    /// Take the recorded write failure, if the writer recorded one.
    fn take_failure(&self) -> Option<ConnError> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    // -- Ending ----------------------------------------------------------------

    /// End the connection gracefully: everything
    /// [accepted](Self::send) beforehand is written and flushed, the stream
    /// is shut down, and only then does this return.
    ///
    /// The precise guarantee: a frame whose `send` returned `Ok` before
    /// `close` was called is delivered to the socket — or the failure is this
    /// call's return value. Graceful means *willing to wait*: if the peer has
    /// stopped reading, flushing can take arbitrarily long, because a TCP FIN
    /// queues behind the data it follows. A caller that wants "flush, but not
    /// past a deadline" wraps this in its own timeout and falls back to
    /// [`abort`](Self::abort); the driver does not guess the deadline.
    pub async fn close(mut self) -> Result<(), ConnError> {
        if self.ended {
            return Err(self.take_failure().unwrap_or(ConnError::Closed));
        }
        let (done_tx, done_rx) = oneshot::channel();
        if let Err(error) = self.dispatch(Command::Finish { done: done_tx }).await {
            self.stats.note_error(&error);
            return Err(error);
        }
        let outcome = done_rx.await.unwrap_or(Err(ConnError::Closed));
        self.ended = true;
        if let Err(error) = &outcome {
            self.stats.note_error(error);
        }
        outcome
    }

    /// End the connection now: discard everything queued, stop the writer
    /// even mid-write, shut the stream down, and return without waiting for
    /// anything.
    ///
    /// The guarantee is the complement of [`close`](Self::close)'s: nothing
    /// queued-but-unwritten is promised, the peer observes a truncated stream
    /// if there was a backlog, and the call is prompt even when the writer is
    /// wedged on a consumer that reads nothing — that is what the abort flag
    /// is checked against between commands and around every write. Use it
    /// when the alternative is spending one more second on a connection that
    /// owes the server nothing.
    pub fn abort(mut self) {
        self.abort_flag.send_replace(true);
        self.ended = true;
    }
}

impl<W: AsyncRead + AsyncWrite + Unpin + Send + 'static> Drop for Conn<W> {
    fn drop(&mut self) {
        // Dropping without an explicit ending is an abort, not a graceful
        // close: a caller that wanted the flush guarantees had `close` for
        // it. The flag is synchronous and infallible, so this works even
        // with the command queue jammed full.
        self.abort_flag.send_replace(true);
    }
}

// Debug is manual because the socket half has nothing worth printing and the
// reader-side cipher must never reach a log line.
impl<W: AsyncRead + AsyncWrite + Unpin + Send + 'static> std::fmt::Debug for Conn<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("state", &self.machine.state())
            .field("compression", &self.encoder.compression())
            .field("encryption", &self.cipher_in.is_enabled())
            .field("outbound_queued", &self.queued.load(Ordering::Relaxed))
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

/// The writer task: owns the sending half of the socket, drains commands in
/// arrival order, and stops on the abort flag, a broken stream, a closed
/// queue or a graceful finish — whichever comes first.
///
/// The abort flag is checked between commands *and around every write*. That
/// second check is what makes [`Conn::abort`] prompt: without it, an abort
/// would wait behind a `write_all` wedged on the very peer the abort exists
/// to stop waiting for. The interrupted write is dropped with its partial
/// frame, which is the documented cost of aborting.
async fn write_loop<W: AsyncWrite + Unpin + Send + 'static>(
    mut writer: WriteHalf<W>,
    mut inbound: mpsc::Receiver<Command>,
    mut abort: watch::Receiver<bool>,
    failure: Arc<Mutex<Option<ConnError>>>,
    queued: Arc<AtomicUsize>,
    stats: Arc<ConnCounters>,
) {
    let mut cipher_out = Cipher::disabled();

    loop {
        // Biased so an abort wins against a command that arrived in the same
        // wakeup. Any resolution of `changed()` ends the writer: the flag
        // only ever moves to `true`, and an `Err` means every sender is gone.
        let command = tokio::select! {
            biased;
            _ = abort.changed() => break,
            command = inbound.recv() => match command {
                Some(command) => command,
                None => break,
            },
        };

        match command {
            Command::EnableEncryption { secret } => {
                cipher_out.enable(&SharedSecret::from_bytes(secret));
            }
            Command::Frame(mut wire) => {
                queued.fetch_sub(1, Ordering::Relaxed);
                // Encryption is applied here, at the last moment before the
                // bytes move, because that is what makes its position in the
                // stream exact: everything enqueued before the switch goes
                // out plaintext even if it was still queued when the switch
                // arrived.
                cipher_out.encrypt(&mut wire);
                let written = writer.write_all(&wire);
                // The abort flag is checked around the write as well as
                // between commands: this is what makes `abort` prompt while
                // the writer is wedged on a peer that reads nothing. The
                // interrupted write drops with its partial frame, which is
                // the documented cost of aborting.
                tokio::select! {
                    biased;
                    _ = abort.changed() => break,
                    outcome = written => {
                        if let Err(error) = outcome {
                            // A failed write poisons the connection: the
                            // stream may have taken half a frame, so
                            // everything after it is noise.
                            record_failure(&failure, Some(ConnError::Io(error)));
                            break;
                        }
                        // Counted here rather than at accept time so that
                        // `bytes_out` is what the socket actually took, the
                        // honest counterpart to `bytes_in`'s wire cost. The
                        // two counters answer different questions on a
                        // stalled connection, which is the point.
                        stats.bytes_out.fetch_add(wire.len() as u64, Ordering::Relaxed);
                    }
                }
            }
            Command::Finish { done } => {
                // FIFO ordering has already flushed every frame queued ahead
                // of this command; all that remains is the shutdown itself.
                let outcome = writer.shutdown().await.map_err(ConnError::Io);
                let _ = done.send(outcome);
                break;
            }
        }
    }

    // Every exit path ends the same way: shut the stream down so the peer
    // observes EOF rather than a reset. On the abort path the queued frames
    // are already gone; on every other path they were written first.
    let _ = writer.shutdown().await;
}

/// Record a writer failure for the next operation that looks.
///
/// The slot holds one error; the first caller to look takes it and later
/// callers see [`ConnError::Closed`]. Losing the second-hand detail is fine —
/// the cause was reported once, exactly where somebody was waiting on it.
fn record_failure(slot: &Mutex<Option<ConnError>>, failure: Option<ConnError>) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = failure;
}

/// The liveness arithmetic, kept separate from the I/O so it can be tested
/// without a runtime.
mod liveness {
    use std::time::{Duration, Instant};

    use crate::state::State;

    /// Which clock produced a window, so a timeout can say which.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// Plain silence.
        Idle,
        /// The unauthenticated-phase budget.
        PreAuth,
    }

    /// How long the next read may wait, and under which clock.
    ///
    /// The rule, in order: no idle policy means no bound at all; outside the
    /// pre-authentication states only idle applies; inside them the budget's
    /// remaining time applies when it is *tighter* than idle, and idle wins
    /// otherwise. "Tighter" is the whole subtlety — the budget does not
    /// shorten waits that idle already bounds harder, it only ever cuts in
    /// as the connection approaches spending its total allotment.
    pub fn budget(
        idle: Option<Duration>,
        pre_auth_budget: Option<Duration>,
        started_at: Instant,
        state: State,
    ) -> Option<(Duration, Kind)> {
        let idle = idle?;
        if state.is_pre_authentication() {
            if let Some(total) = pre_auth_budget {
                let remaining = total.saturating_sub(started_at.elapsed());
                if remaining < idle {
                    return Some((remaining, Kind::PreAuth));
                }
            }
        }
        Some((idle, Kind::Idle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::MAX_FRAME_LEN;
    use std::time::Duration;

    fn budget_for(
        idle: Option<Duration>,
        pre_auth: Option<Duration>,
        elapsed: Duration,
        state: State,
    ) -> Option<(Duration, liveness::Kind)> {
        // `Instant::now() - elapsed` reconstructs a start time so the helper
        // can be driven without sleeping.
        liveness::budget(idle, pre_auth, Instant::now() - elapsed, state)
    }

    #[test]
    fn the_idle_clock_bounds_every_state_that_has_one() {
        for state in [
            State::Handshaking,
            State::Status,
            State::Login,
            State::Configuration,
            State::Play,
        ] {
            assert_eq!(
                budget_for(Some(Duration::from_secs(5)), None, Duration::ZERO, state),
                Some((Duration::from_secs(5), liveness::Kind::Idle)),
                "{state}"
            );
        }
    }

    #[test]
    fn no_idle_policy_means_no_bound_at_all() {
        assert_eq!(
            budget_for(
                None,
                Some(Duration::from_secs(5)),
                Duration::ZERO,
                State::Login
            ),
            None,
            "a budget cannot bound what idle does not"
        );
        assert_eq!(
            budget_for(None, None, Duration::ZERO, State::Play),
            None,
            "neither clock configured"
        );
    }

    #[test]
    fn the_pre_auth_budget_applies_only_before_authentication() {
        let idle = Duration::from_secs(10);
        // Two seconds into a five-second budget, with idle at ten: the
        // budget is tighter, so it is the clock that fires. The remaining
        // window is checked as an interval rather than an equality, because
        // a few microseconds pass between building the start instant and
        // asking the question.
        let (window, kind) = budget_for(
            Some(idle),
            Some(Duration::from_secs(5)),
            Duration::from_secs(2),
            State::Login,
        )
        .expect("a clock must be configured");
        assert_eq!(kind, liveness::Kind::PreAuth);
        // Three seconds unspent, within a ten-millisecond slack that absorbs
        // the microseconds of real time passing inside the call.
        let expected = Duration::from_secs(3);
        assert!(
            window <= expected && window + Duration::from_millis(10) >= expected,
            "the window should be the unspent budget, not {window:?}"
        );
        // The same instant after authentication: the budget no longer
        // applies, however little of it was left unspent.
        assert_eq!(
            budget_for(
                Some(idle),
                Some(Duration::from_secs(5)),
                Duration::from_secs(2),
                State::Configuration
            ),
            Some((idle, liveness::Kind::Idle))
        );
        assert_eq!(
            budget_for(
                Some(idle),
                Some(Duration::from_secs(5)),
                Duration::from_secs(2),
                State::Play
            ),
            Some((idle, liveness::Kind::Idle))
        );
    }

    #[test]
    fn an_expired_budget_reports_zero_rather_than_a_negative_window() {
        // Six seconds into a five-second budget: saturating subtraction
        // lands on zero, so the next read times out immediately instead of
        // panicking on a negative duration or silently getting idle's value.
        assert_eq!(
            budget_for(
                Some(Duration::from_secs(10)),
                Some(Duration::from_secs(5)),
                Duration::from_secs(6),
                State::Handshaking
            ),
            Some((Duration::ZERO, liveness::Kind::PreAuth))
        );
    }

    #[test]
    fn the_idle_timeout_wins_while_it_is_the_tighter_clock() {
        // Idle 1s, budget 30s, one second elapsed: the remaining budget is
        // far away, so idle governs the window. Without this arm a fresh
        // connection's reads would all be bounded by thirty seconds instead
        // of one, and a dead peer would take thirty seconds to notice.
        assert_eq!(
            budget_for(
                Some(Duration::from_secs(1)),
                Some(Duration::from_secs(30)),
                Duration::from_secs(1),
                State::Status
            ),
            Some((Duration::from_secs(1), liveness::Kind::Idle))
        );
    }

    #[test]
    fn config_defaults_are_public_server_sized_and_nonzero() {
        let config = ConnConfig::default();
        assert!(config.outbound_capacity > 0);
        assert!(config.read_chunk > 0);
        assert_eq!(config.limits.max_frame_len, MAX_FRAME_LEN);
        let timeouts = Timeouts::default();
        assert!(matches!(timeouts.idle, Some(d) if d.as_secs() >= 1));
        assert!(matches!(timeouts.pre_auth_budget, Some(d) if d.as_secs() >= 1));
    }
}
