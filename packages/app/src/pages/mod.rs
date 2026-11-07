//! Renderer-neutral page rendering, split by product area.
//!
//! Route handlers, form parsing, and draft logic live in `crate::routes`; these modules only build
//! `HyperChad` containers from already-authorized presentation data.

pub mod auth;
pub mod dashboard;
pub mod game;
pub mod lobby;
