//! The contents of the registries a client is sent during configuration.
//!
//! # The problem this solves
//!
//! Since 1.20.5 a joining client is told the contents of every datapack
//! registry before it enters the world. The payload per entry is optional, and
//! a server omits it for any client that has acknowledged the server's known
//! packs — which every vanilla client does. So Dust has been able to serve
//! vanilla clients with names alone.
//!
//! Clients that acknowledge nothing get nothing, and are disconnected. That
//! is most of the bot and proxy ecosystem: `mineflayer` sends an empty pack
//! list, fails inside its own registry loader reading `undefined` where a
//! dimension type's contents should be, and never reaches the world. The
//! refusal was the correct behaviour for a server with no contents to send.
//!
//! This module is the contents.
//!
//! # Three files, three jobs
//!
//! * [`schema`] — what the wire form of an entry looks like. Types, and which
//!   keys are optional. A description of an interface, committed here.
//! * [`convert`] — one entry's JSON, under a schema, into an NBT compound.
//!   Pure, and every refusal names the path that reached it.
//! * [`source`] — reading a directory of the operator's own data. The values
//!   live there and not here; see decision record 0007.

pub mod convert;
pub mod schema;
pub mod source;

pub use convert::{ConvertError, ErrorKind};
pub use source::{load, Contents, EntryError, LoadError, Loaded};
