//! Web application server. The program has no function yet.
//!
//! The ingest image (`ingest/Dockerfile`) does not build this crate. It
//! reads only `Cargo.toml` of this crate and makes a stub target, so a
//! change of the sources does not change the ingest image. Do not declare
//! target paths in `Cargo.toml`: the stub works only with the automatic
//! target discovery of Cargo.

fn main() {}
