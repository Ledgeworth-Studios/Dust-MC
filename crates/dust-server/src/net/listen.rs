//! The listening socket, bound during boot and served on the network runtime.
//!
//! # Why the bind is not in the async task
//!
//! A [`Listener`] is created by [`Listener::bind`], which is an ordinary
//! blocking function and returns an error the boot phase can fail on. Only
//! afterwards does [`Listener::serve`] hand it to a runtime.
//!
//! The alternative — spawn a task, let it bind, log if it fails — turns
//! "port 25565 is already in use" into a server that boots successfully, ticks
//! forever and never answers. An operator watching the console sees a healthy
//! start. That failure mode is the reason for the ordering, and the test for it
//! binds a port twice and asserts the second attempt fails *before* anything
//! is spawned.
//!
//! # Why the runtime is built here rather than taken
//!
//! The tick loop is synchronous and owns its own thread; the network is
//! asynchronous and owns a pool. Neither should be able to block the other, so
//! the runtime is created next to the listener and shut down with it. It is
//! held in the handle rather than in a task so that dropping the handle is what
//! stops the accept loop — a stop path that cannot be forgotten because it is
//! the destructor.

use std::io;
use std::net::{SocketAddr, TcpListener as StdListener};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::Notify;

use crate::logging::Logger;

use super::session::{serve, Served, SessionContext};

/// A bound, not-yet-serving socket.
///
/// The type exists to make "bound" and "serving" different states with
/// different capabilities, rather than one object with a flag. A `Listener`
/// that is never served still holds the port, which is the point: the boot
/// phase has proven the port is available and nothing can take it in between.
#[derive(Debug)]
pub struct Listener {
    inner: StdListener,
    addr: SocketAddr,
}

impl Listener {
    /// Bind, now, on this thread.
    ///
    /// The socket is set non-blocking here rather than after the handover
    /// because tokio requires it and a listener registered while still blocking
    /// stalls the whole runtime's reactor on the first accept — a failure that
    /// looks like a network problem and is not one.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let inner = StdListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        // Re-read rather than reuse the requested address: a request for port 0
        // means "any free port", and the number the operating system chose is
        // the one worth logging and the one the tests connect to.
        let addr = inner.local_addr()?;
        Ok(Self { inner, addr })
    }

    /// The address actually bound, which is not always the one requested.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Start accepting, on a runtime this call creates and owns.
    ///
    /// Returns once the accept loop is running. Dropping the returned handle
    /// stops it.
    pub fn serve(self, ctx: Arc<SessionContext>, logger: Logger) -> io::Result<ListenerHandle> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            // Named because a stuck thread in a backtrace should say which pool
            // it belongs to. A process with a tick thread, a watchdog thread
            // and an unnamed pool is a process whose stack dumps need a
            // footnote.
            .thread_name("dust-net")
            .build()?;

        let addr = self.addr;
        let counters = Arc::new(Counters::default());
        let shutdown = Arc::new(Notify::new());

        let accept_ctx = Arc::clone(&ctx);
        let accept_counters = Arc::clone(&counters);
        let accept_shutdown = Arc::clone(&shutdown);
        let accept_logger = logger.clone();
        runtime.spawn(async move {
            let listener = match TcpListener::from_std(self.inner) {
                Ok(listener) => listener,
                Err(e) => {
                    // Only reachable if the socket stopped being usable between
                    // the bind and this line, which is not a case any test can
                    // stage. Logged rather than swallowed.
                    accept_logger.error("dust::net", format!("could not adopt the socket: {e}"));
                    return;
                }
            };
            accept_loop(
                listener,
                accept_ctx,
                accept_counters,
                accept_shutdown,
                accept_logger,
            )
            .await;
        });

        logger.info("dust::net", format!("listening on {addr}"));
        Ok(ListenerHandle {
            runtime: Some(runtime),
            addr,
            counters,
            shutdown,
            logger,
        })
    }
}

/// The accept loop's own counters, readable from the tick thread.
///
/// Atomics rather than a lock because every one of them is written by one
/// task and read by anybody, and nothing ever needs two of them to agree with
/// each other at an instant. A snapshot that mixes a count from one moment with
/// a count from the next is fine here; a snapshot that made the accept loop
/// wait for a reader would not be.
#[derive(Debug, Default)]
pub struct Counters {
    accepted: AtomicU64,
    status_served: AtomicU64,
    logins_refused: AtomicU64,
    failed: AtomicU64,
}

/// A point-in-time reading of the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetStats {
    pub accepted: u64,
    pub status_served: u64,
    pub logins_refused: u64,
    pub failed: u64,
}

async fn accept_loop(
    listener: TcpListener,
    ctx: Arc<SessionContext>,
    counters: Arc<Counters>,
    shutdown: Arc<Notify>,
    logger: Logger,
) {
    loop {
        let accepted = tokio::select! {
            // Biased so a shutdown request is taken even while connections are
            // arriving faster than they are served. Unbiased, `select!` picks
            // at random and a busy port can keep a stop waiting.
            biased;
            () = shutdown.notified() => return,
            result = listener.accept() => result,
        };
        let (socket, peer) = match accepted {
            Ok(pair) => pair,
            Err(e) => {
                // An accept can fail per-connection — the peer vanished
                // between the SYN and here, or the process is out of file
                // descriptors — and neither is a reason to stop listening.
                // Logged at warn and the loop continues, because a listener
                // that exits on a transient error is a denial of service with
                // the operator's own hands.
                logger.warn("dust::net", format!("accept failed: {e}"));
                continue;
            }
        };
        counters.accepted.fetch_add(1, Ordering::Relaxed);

        // Nagle off. Every packet this server sends before Play is a complete
        // message the client is waiting on, and a forty-millisecond delay
        // waiting for a coalescing partner that never comes is forty
        // milliseconds on every ping in the server list.
        if let Err(e) = socket.set_nodelay(true) {
            logger.debug("dust::net", format!("{peer}: could not set nodelay: {e}"));
        }

        let ctx = Arc::clone(&ctx);
        let counters = Arc::clone(&counters);
        let logger = logger.clone();
        tokio::spawn(async move {
            match serve(socket, ctx).await {
                Ok(Served::Status { pinged }) => {
                    counters.status_served.fetch_add(1, Ordering::Relaxed);
                    logger.debug(
                        "dust::net",
                        format!("{peer}: server list ping (measured: {pinged})"),
                    );
                }
                Ok(Served::LoginRefused) => {
                    counters.logins_refused.fetch_add(1, Ordering::Relaxed);
                    logger.info(
                        "dust::net",
                        format!("{peer}: asked to log in; this server cannot host players yet"),
                    );
                }
                Ok(Served::NothingAsked) => {
                    logger.trace("dust::net", format!("{peer}: connected and said nothing"));
                }
                Err(e) => {
                    counters.failed.fetch_add(1, Ordering::Relaxed);
                    // Debug, not warn. Every one of these is caused by a
                    // stranger, and a stranger who can raise the server's log
                    // level by sending rubbish can fill its disk.
                    logger.debug("dust::net", format!("{peer}: {e}"));
                }
            }
        });
    }
}

/// A running listener. Dropping this stops it.
#[derive(Debug)]
pub struct ListenerHandle {
    /// `Option` only so the destructor can take it. Always `Some` while alive.
    runtime: Option<Runtime>,
    addr: SocketAddr,
    counters: Arc<Counters>,
    shutdown: Arc<Notify>,
    logger: Logger,
}

impl ListenerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// What the listener has seen since it started.
    pub fn stats(&self) -> NetStats {
        NetStats {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            status_served: self.counters.status_served.load(Ordering::Relaxed),
            logins_refused: self.counters.logins_refused.load(Ordering::Relaxed),
            failed: self.counters.failed.load(Ordering::Relaxed),
        }
    }

    /// Stop accepting and release the port.
    ///
    /// Taking `self` by value rather than `&mut` so that "stopped" is a state
    /// the type system knows about: there is no handle left to call anything
    /// else on. The destructor does the same work for the path where nobody
    /// called this.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(runtime) = self.runtime.take() {
            // `shutdown_background` rather than a timed wait: in-flight
            // sessions are a status ping at most, they hold nothing that needs
            // saving, and blocking the shutdown sequence on a peer that has
            // stopped reading would hand a stranger control of how long this
            // server takes to stop. The world's shutdown grace is for the
            // world.
            runtime.shutdown_background();
        }
        self.logger
            .info("dust::net", format!("stopped listening on {}", self.addr));
    }
}
