//! The game's own rules about blocks and the things in the world.
//!
//! Entities, physics, inventory, redstone, fluids, AI and combat will live
//! here. What is here today is [`placement`]: which *state* a block takes when
//! a player puts it down.
//!
//! Nothing in this crate knows about a socket or a save file. It takes what a
//! click said and answers with a block state, which is what lets it be tested
//! without running a server — the same argument `dust-guard` makes about the
//! reach check, and the same reason both are crates rather than functions
//! inside the session.

#![forbid(unsafe_code)]

pub mod placement;

pub use placement::{replaces_beside, replaces_clicked, state_for, state_for_item, Click, Face};
