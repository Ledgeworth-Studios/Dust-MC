//! Transport for the Minecraft protocol: framing, compression, encryption,
//! connection state, and the sockets underneath them.
//!
//! Crate docs are completed in the final pass; see the module docs.

pub mod crypt;
pub mod frame;
pub mod login;
pub mod state;
pub mod testkeys;
pub mod varint;
