//! The ingest process of the telemetry service.
//!
//! This crate is the process skeleton: command parsing, start-up
//! configuration, structured logs, the health endpoint, and the shutdown
//! sequence. `docs/adr/0001-ingestion-architecture.md` describes the complete
//! process. Later changes add the gate, the child supervisor, the ZMQ reader,
//! the queue, and the database writer as new modules.
//!
//! `src/main.rs` is the only place that reads the process environment and
//! the command line. The modules receive these values as parameters, so that
//! tests can control them.

pub mod cli;
pub mod config;
pub mod decode;
pub mod events;
pub mod http;
pub mod lifecycle;
pub mod logging;
pub mod readiness;
pub mod record;
pub mod watchdog;
