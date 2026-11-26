//! The ingest process of the telemetry service.
//!
//! `docs/adr/0001-ingestion-architecture.md` describes the complete process.
//! This crate has command parsing, start-up configuration, structured logs,
//! the health endpoint, the TCP gate, the supervision of `fleet-telemetry`,
//! the watchdog, the decode of the ZMQ messages ([`decode`]), the delivery
//! path ([`pipeline`]: the ZMQ reader, the bounded queue, the writer, and
//! the shortfall check), the storage ([`storage`]: the raw schema, the
//! migrations, and the database writer), and the start and the shutdown
//! sequence ([`lifecycle`]). The parts connect through four interfaces:
//! the storage ([`storage`]), the readiness input of the gate
//! ([`readiness::Readiness`]), the sink of the `ingest_event` records
//! ([`events::EventSink`]), and the heartbeat registration
//! ([`watchdog::Watchdog::register`]).
//!
//! `src/main.rs` is the only place that reads the process environment and
//! the command line. The modules receive these values as parameters, so that
//! tests can control them.

pub mod child;
pub mod child_output;
pub mod cli;
pub mod config;
pub mod decode;
pub mod events;
pub mod gate;
pub mod http;
pub mod lifecycle;
pub mod logging;
pub mod pipeline;
pub mod readiness;
pub mod record;
pub mod storage;
pub mod watchdog;
