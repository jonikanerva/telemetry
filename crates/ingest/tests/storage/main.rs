//! Tests of the storage against PostgreSQL 18 on the Neon branch `test`.
//!
//! The tests need `TEST_DATABASE_URL` (`README.md`, "Verification"). Each
//! test works in a schema of its own and removes it (`support.rs`). Every
//! VIN, coordinate, and value is invented.
//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! Clippy, so that they may use `unwrap` and `panic` like the tests do.
#![cfg(test)]

mod events;
#[path = "../harness/mod.rs"]
mod harness;
mod pipeline;
mod roles;
mod schema;
mod size;
mod support;
mod tls;
mod writer;
