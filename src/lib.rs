//! orchestratr (`orcr`) — a cross-provider orchestrator for AI coding agents, built on
//! [herdr](https://herdr.dev).
//!
//! This crate is the `orcr` binary's library: server, CLI, herdr driver, store, and
//! integrations. M0 ("foundations") ships the plumbing everything else stands on: the
//! home layout, config, the store, and the herdr socket driver. No user-facing agent
//! features live here yet.

pub mod api;
pub mod cli;
pub mod config;
pub mod cron;
pub mod driver;
pub mod duration;
pub mod error;
pub mod events;
pub mod home;
pub mod lock;
pub mod path;
pub mod server;
pub mod service;
pub mod store;
pub mod wire;

pub use error::{ErrorCode, OrcrError, Result};
