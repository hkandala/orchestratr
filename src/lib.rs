//! orchestratr (`orcr`) — a cross-provider orchestrator for AI coding agents, built on
//! [herdr](https://herdr.dev).
//!
//! This crate is the `orcr` binary's library: the single-writer server behind the socket
//! API, the thin CLI client, the herdr socket driver, the sqlite store, provider
//! integrations, the durable loop scheduler, and the `top` monitoring TUI.

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
pub mod scaffold;
pub mod server;
pub mod service;
pub mod store;
pub mod top;
pub mod wire;

pub use error::{ErrorCode, OrcrError, Result};
