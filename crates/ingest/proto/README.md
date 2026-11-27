# Vendored fleet-telemetry protobuf schema

These files are unchanged copies of the protobuf schema of Tesla's
`fleet-telemetry` server. `build.rs` generates the Rust types from them
(ADR 0001, "Wire format: protobuf").

- Source: <https://github.com/teslamotors/fleet-telemetry/tree/6689adb831691b1de211b4aa8220babf7309f31c/protos>
- Tag: `v0.10.1`. The deployed image is `tesla/fleet-telemetry:v0.10.1`
  (`ingest/Dockerfile`).
- Commit: `6689adb831691b1de211b4aa8220babf7309f31c`. The GitHub API
  showed on 26 November 2026 that tag `v0.10.1` points to this commit.
- Licence: Apache License 2.0. `LICENSE` is the licence file of the
  upstream repository at the same commit.

| File | SHA-256 |
| ---- | ------- |
| `vehicle_data.proto` | `98d4f1e0ed85a505df9e9a89df2f3500adcb8f84779c7489cdfe0e7c55fbb38b` |
| `vehicle_alert.proto` | `3599c68b5f8f19805fe89a4a0f4a6dca754e87600eb614577ec01cbd8b0b2cde` |
| `vehicle_error.proto` | `11704aa26e79ad99262b1f2bf351924efb60dcf43e3a7fa4a3f4392289e3f842` |
| `vehicle_connectivity.proto` | `c961a3c529d640c4ccd1709630c2647bc2b13e63eba33fd2077ef555eed49f0d` |

Upstream also has `vehicle_metric.proto`. The server configuration
dispatches only the record types `V`, `alerts`, `errors`, and
`connectivity` (ADR 0001), so that file is not vendored.

## Update

Change these files only together with the `fleet-telemetry` image tag in
`ingest/Dockerfile`:

1. Copy the four files from `protos/` of the new tag without changes.
2. Copy `LICENSE` of the new tag.
3. Update the tag, the commit, and the checksums above
   (`shasum -a 256 *.proto`).
4. Run `mise run verify`. A new variant of `Value.value`, or a new field
   of `Doors`, `Time`, or `TireLocation`, stops the build until
   `src/decode.rs` maps it. The tests compare the decode with the new
   schema.
