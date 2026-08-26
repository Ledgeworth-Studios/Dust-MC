//! Transport for the Minecraft Java Edition protocol: everything between an
//! accepted connection and the first packet of the game.
//!
//! The crate is the bottom of Dust's network stack. It owns sockets, framing,
//! compression, encryption, and the connection state machine. It does **not**
//! own packets: what id `0x00` means in which state, what fields a Login
//! Start carries — that is the `dust-protocol` seam, described in
//! [`state`]. This division lets both sides be built at once by people who do
//! not have each other's code.
//!
//! # The modules, in the order bytes meet them
//!
//! * [`varint`] — variable-length integers, with a canonicity rule stricter
//!   than vanilla's and the reasoning that justifies it.
//! * [`frame`] — length prefixes and the compression header. Seven
//!   defences against unauthenticated input, each with a test built to fail
//!   if the defence were removed; the threshold boundary (`>=`, per vanilla)
//!   walked cell by cell in its own test file.
//! * [`crypt`] — AES-128-CFB8 with key equal to IV, switched on mid-stream at
//!   exactly the byte the protocol chooses. Pinned to NIST's CFB8 vector and
//!   to ciphertexts LibreSSL and a real JVM produced.
//! * [`login`] — RSA-1024 key pairs, X.509 SubjectPublicKeyInfo encoding,
//!   PKCS#1 v1.5 decryption, the verify-token challenge, and the login digest
//!   the session server will one day be asked about.
//! * [`state`] — where a connection is in the protocol, with every transition
//!   checked and re-entry into configuration modelled properly.
//! * [`io`] — the driver that ties it together over any byte stream:
//!   idle and pre-authentication timeouts, a bounded outbound queue whose
//!   slow consumers cost latency instead of memory, graceful and abortive
//!   close, and mode switches that land in the right place by construction.
//! * [`testkeys`] — fixed, published, harmless cryptographic fixtures.
//!
//! # Who this code faces
//!
//! Every pre-authentication state here is reachable by a stranger, so the
//! threat model starts before any identity exists. The decoder bounds every
//! length before allocating anything; decompression output is bounded twice;
//! negative and non-canonical encodings are refused as themselves; a peer
//! that stops talking hits an idle timeout, and one that dribbles forever
//! without progressing hits the wall-clock budget for unauthenticated life.
//! What this layer does not do is pace well-behaved traffic — rate limiting
//! needs knowledge of other connections, which no per-connection module has.
//!
//! # How it is checked
//!
//! The rule throughout is that self-consistency proves almost nothing: an
//! encoder and decoder that are wrong together agree perfectly. So every
//! half of this crate is anchored to something outside itself — published
//! protocol tables for VarInts, NIST vectors plus LibreSSL- and JVM-produced
//! ciphertexts for the crypto, structural wire-byte assertions for the
//! compression boundary — and then a seeded mutation fuzzer attacks the
//! whole surface thousands of times, asserting only errors, bounded memory
//! and bounded reads. The in-memory tests run the same driver code the real
//! socket path will run, because a special-cased harness would prove things
//! about the harness.
//!
//! ```no_run
//! use dust_net::frame::Frame;
//! use dust_net::io::{Conn, ConnConfig};
//! use dust_net::state::State;
//!
//! # async fn drive(socket: impl tokio::io::AsyncRead + tokio::io::AsyncWrite
//! #     + Unpin + Send + 'static) -> Result<(), dust_net::io::ConnError> {
//! let mut conn = Conn::new(socket, ConnConfig::default());
//! conn.handshake(2)?; // the handshake's next-state field said "login"
//! while let Some(frame) = conn.next_frame().await? {
//!     // Hand `frame` up to dust-protocol; take transitions from there.
//!     conn.send(Frame::new(0x01, b"reply")).await?;
//!     if conn.state() == State::Configuration {
//!         break; // authenticated; hand off to the play path
//!     }
//! }
//! conn.close().await?; // flush everything accepted, then hang up
//! # Ok(())
//! # }
//! ```

pub mod crypt;
pub mod frame;
pub mod io;
pub mod login;
pub mod state;
pub mod testkeys;
pub mod varint;
