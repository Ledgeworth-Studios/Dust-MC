//! The variable-length integers, re-exported from their single home.
//!
//! There is one VarInt implementation in this workspace, and it is not here.
//! It lives in [`dust_protocol::varint`], which owns it because every field
//! codec reads a VarInt before anything else and nothing in the workspace may
//! depend on this crate — the shared thing has to sit below both of us. When
//! the crates merged, this crate's decoder was the stricter of the two and its
//! rule was adopted wholesale: overlong encodings and too-wide final bytes are
//! refused everywhere, because two byte strings that decode to one value break
//! every replay guard and rate limiter that ever compares frames. The
//! rationale in full is documented where the code now lives.
//!
//! This module keeps `dust_net::varint::...` paths working for everything
//! already importing them, and adds nothing on top. If it ever wants to grow
//! something net-specific, the first question is whether `dust-protocol`
//! should have it instead.

pub use dust_protocol::varint::*;
