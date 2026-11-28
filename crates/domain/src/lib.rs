//! Domain rules of the web application.
//!
//! [`battery`] holds the rules of the battery view (ADR 0013). Every
//! function is pure: the caller passes the time and the stored rows in.
//! The crate reads no clock, no environment, and no file.
//!
//! The ingest image (`ingest/Dockerfile`) does not build this crate. It
//! reads only `Cargo.toml` of this crate and makes a stub target, so a
//! change of the sources does not change the ingest image.

pub mod battery;
