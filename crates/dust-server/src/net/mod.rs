//! The socket: where a stranger's bytes enter this process.
//!
//! # The boundary this module draws
//!
//! Three crates meet here and none of them can meet anywhere else.
//! `dust-net` owns bytes, frames, ciphers and the legality of a state
//! transition; `dust-protocol` owns what a frame's contents mean in a given
//! version; `dust-config` owns what this operator asked for. Assembly is
//! `dust-server`'s job by the architecture's dependency rule, and this is the
//! assembly.
//!
//! # Binding happens in the boot phase; serving happens on the runtime
//!
//! The listener's socket is bound *synchronously*, inside the ordered boot, and
//! only then handed to the asynchronous runtime that accepts on it. That split
//! is the whole design of [`listen`], and it exists because the two failures
//! are completely different in kind. A port already in use is a *boot* failure:
//! the operator gets an error naming the setting, the phases that already
//! started are torn down in reverse, and the process exits non-zero. If the
//! bind happened inside a spawned task instead, the same mistake would produce
//! a server that started, logged something, ticked happily and accepted no
//! connections — the worst outcome available, and the same one the ore
//! resolver refuses for unknown ore names.
//!
//! # What the tick loop has to do with this, today: nothing
//!
//! A server-list ping touches no world state, no entity and no player. It is
//! answered entirely off the network runtime while the tick loop runs beside
//! it, and the two share nothing but an atomic player count. That is not a
//! shortcut — it is why status is the right first thing to serve, and it is
//! also why the seam that will carry Play traffic into the tick is not
//! invented here on speculation. When there is a player to move, there will be
//! a [`TickParticipant`](crate::participant::TickParticipant) to move them.

pub mod favicon;
pub mod listen;
pub mod session;
pub mod status;

pub use favicon::{Favicon, FaviconError};
pub use listen::{Listener, ListenerHandle};
pub use session::{Authority, Served, SessionContext, SessionError};
pub use status::StatusPolicy;
