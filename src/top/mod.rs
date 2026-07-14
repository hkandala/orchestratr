//! `orcr top` — the live, view-only monitoring TUI (spec §6.3, §7).
//!
//! The pure tree/filter/render logic lives in [`model`] (unit-tested without a PTY); the
//! interactive terminal app lives in [`app`]. The CLI builds a [`model::TopFilter`] from its
//! flags and hands off to [`app::run_top`].

pub mod app;
pub mod model;

pub use app::run_top;
pub use model::{build_tree, Snapshot, TopFilter, Tree};
