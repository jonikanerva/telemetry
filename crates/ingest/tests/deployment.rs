//! Tests of the deployment files in `ingest/` (ADR 0001, "Layout" and
//! "`fleet-telemetry` configuration"). No container runtime runs here: the
//! tests read the files and check that they agree with each other and with
//! the code.
//!
//! - The `ENV` lines of the image are a valid configuration of `ingest run`,
//!   and the ports equal the ports of the server configuration. A wrong
//!   port would keep the gate closed in production.
//! - The server configuration sends every record type to `zmq` only, so no
//!   record reaches the log.
//! - The image copies the files to the paths that the settings name.
//! - The Railway service settings that the lead applies by hand (constants
//!   in the module `railway`) agree with the image, the code, the shutdown
//!   budget, and the settings table of STACK.md §15. Each input that the
//!   image build copies has a watch path and an entry in `.dockerignore`.
//!   The sources of the other workspace members and the web files match no
//!   watch path, and no `COPY` line of the image reads them: the image
//!   build reads only the `Cargo.toml` files of the other members and makes
//!   a stub target for each. `.dockerignore` also serves the web image, so
//!   the web files are in the build context, but the ingest image does not
//!   read them.
//! - The image has no `USER` line, so the container starts as root.
//!   `ingest/entrypoint.sh` writes the TLS files with mode 0600, gives them
//!   to user 10001, and starts `ingest run` through `setpriv` as user 10001
//!   without the TLS variables (issue #29). The test runs a copy of the
//!   script with temporary paths in place of the image paths, and with
//!   stubs for `setpriv` and `chown` in `PATH`: the development machine and
//!   CI run the test as a user without root, and macOS has no `setpriv`.
//!   The stubs record their arguments and environment. The CI job
//!   `image-check` runs the real programs in the image.
//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! Clippy, so that they may use `unwrap` and `panic` like the tests do.
#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use ingest::config::RunConfig;
use ingest::lifecycle::{BUDGET, DRAINING};
use ingest::record::TOPIC_PREFIX;
use ingest::storage::MIGRATE_DEADLINE;
use serde_json::{Value, json};

/// The paths in the image that the entrypoint test replaces.
const TLS_DIR: &str = "/etc/fleet-telemetry/tls";
const INGEST_BINARY: &str = "/usr/local/bin/ingest";
/// The user and group of `ingest` and `fleet-telemetry`. The TLS directory
/// of the image, the `chown` of the TLS files, and the `setpriv` line of
/// the entrypoint use the same number.
const INGEST_ID: u32 = 10001;

/// The options of `setpriv` in the last line of the entrypoint (issue #29):
/// user and group 10001, no supplementary groups, no inheritable
/// capabilities, an empty capability bounding set, and `no_new_privs`.
fn setpriv_options() -> Vec<String> {
    vec![
        "--reuid".to_owned(),
        INGEST_ID.to_string(),
        "--regid".to_owned(),
        INGEST_ID.to_string(),
        "--clear-groups".to_owned(),
        "--inh-caps=-all".to_owned(),
        "--bounding-set=-all".to_owned(),
        "--no-new-privs".to_owned(),
    ]
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    fs::read_to_string(repository().join(path)).unwrap()
}

fn json_file(path: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

/// The instruction texts of `ingest/Dockerfile`, after the checks of the
/// Dockerfile model (`dockerfile_model`), with each run of whitespace as one
/// space.
fn instructions() -> Vec<String> {
    model_of("ingest/Dockerfile")
        .into_iter()
        .map(|instruction| {
            instruction
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The model of the Dockerfile at `path`; a refused text fails the test.
fn model_of(path: &str) -> Vec<Instruction> {
    dockerfile_model(&read(path)).unwrap_or_else(|reason| panic!("{path}: {reason}"))
}

/// The instructions of the final stage: the lines after the last `FROM`.
fn final_stage() -> Vec<String> {
    let lines = instructions();
    let last_from = lines
        .iter()
        .rposition(|line| line.starts_with("FROM "))
        .unwrap();
    lines[last_from + 1..].to_vec()
}

/// The `ENV` values of the final stage.
fn image_env() -> BTreeMap<String, String> {
    final_stage()
        .iter()
        .filter_map(|line| line.strip_prefix("ENV "))
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn env_port(name: &str) -> u64 {
    image_env()[name].parse().unwrap()
}

#[test]
fn the_image_settings_are_a_valid_run_configuration() {
    let env = image_env();
    // Only the fixed settings: no secret, no PORT, and no test override in
    // the image (a release build ignores the overrides in any case).
    let names: BTreeSet<&str> = env.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "INGEST_CHILD_CONFIG",
            "INGEST_CHILD_PROGRAM",
            "INGEST_GATE_PORT",
            "INGEST_METRICS_PORT",
            "INGEST_UPSTREAM_PORT",
            "INGEST_ZMQ_PORT",
        ])
    );
    // The Railway service variables add PORT and DATABASE_URL.
    let mut vars: BTreeMap<String, OsString> = env
        .iter()
        .map(|(name, value)| (name.clone(), OsString::from(value)))
        .collect();
    vars.insert("PORT".to_owned(), OsString::from(railway::PORT.to_string()));
    vars.insert(
        "DATABASE_URL".to_owned(),
        OsString::from("postgresql://telemetry_ingest:secret@db.invalid/neondb"),
    );
    let config = RunConfig::from_vars(|name| vars.get(name).cloned()).unwrap();
    assert_eq!(
        (
            config.gate_port,
            config.upstream_port,
            config.zmq_port,
            config.metrics_port
        ),
        (4443, 4444, 5284, 9090)
    );
}

#[test]
fn the_server_configuration_uses_the_ports_of_the_image() {
    let config = json_file("ingest/server_config.json");
    assert_eq!(config["host"], "127.0.0.1");
    assert_eq!(config["port"], env_port("INGEST_UPSTREAM_PORT"));
    assert_eq!(
        config["zmq"]["addr"],
        format!("tcp://127.0.0.1:{}", env_port("INGEST_ZMQ_PORT"))
    );
    assert_eq!(
        config["monitoring"]["prometheus_metrics_port"],
        env_port("INGEST_METRICS_PORT")
    );
    // The metrics listen on the default host 127.0.0.1 of upstream.
    assert_eq!(config["monitoring"].get("prometheus_metrics_host"), None);
    // The reader subscribes to the topics of this namespace.
    assert_eq!(
        format!("{}_", config["namespace"].as_str().unwrap()),
        TOPIC_PREFIX
    );
}

#[test]
fn the_server_configuration_keeps_records_out_of_the_log() {
    let config = json_file("ingest/server_config.json");
    // Every record type goes to the zmq dispatcher only. The logger
    // dispatcher would write the VIN and the location to the Railway log.
    assert_eq!(
        config["records"],
        json!({
            "V": ["zmq"],
            "alerts": ["zmq"],
            "errors": ["zmq"],
            "connectivity": ["zmq"],
        })
    );
    // `debug` logs raw payloads (ADR 0001).
    assert_eq!(config["log_level"], "info");
    assert_eq!(config["transmit_decoded_records"], false);
    // No reliable acks (ADR 0001, "Transport"). Upstream has no key
    // `reliable_ack`, so the file must not hold one.
    assert_eq!(config.get("reliable_ack_sources"), None);
    assert_eq!(config.get("reliable_ack"), None);
    assert_eq!(
        config["rate_limit"],
        json!({"enabled": true, "message_interval_time": 30, "message_limit": 10000})
    );
}

#[test]
fn the_image_copies_the_files_to_the_paths_of_the_settings() {
    let env = image_env();
    let stage = final_stage();
    for expected in [
        format!(
            "COPY --from=upstream /fleet-telemetry {}",
            env["INGEST_CHILD_PROGRAM"]
        ),
        format!(
            "COPY ingest/server_config.json {}",
            env["INGEST_CHILD_CONFIG"]
        ),
        format!("COPY --from=build /build/target/release/ingest {INGEST_BINARY}"),
        "COPY ingest/entrypoint.sh /entrypoint.sh".to_owned(),
        format!(
            "RUN chmod 0755 /entrypoint.sh && install -d -m 0700 -o {INGEST_ID} -g {INGEST_ID} {TLS_DIR}"
        ),
        format!("EXPOSE {}", env["INGEST_GATE_PORT"]),
        r#"CMD ["/entrypoint.sh"]"#.to_owned(),
    ] {
        assert!(stage.contains(&expected), "missing: {expected}\n{stage:#?}");
    }
    // No stage names a user, so the container starts as root, and the init
    // of Railway and its environment belong to root (issue #29). The
    // entrypoint drops to user 10001 before it starts ingest.
    assert!(
        instructions().iter().all(|line| !line.starts_with("USER ")),
        "the Dockerfile has a USER line"
    );
    // The image holds a release build: only a debug build reads the
    // `INGEST_TEST_*` overrides (`src/main.rs`). The build stage builds with
    // `--release --locked` and no other profile, and the final stage copies
    // from `target/release/` (above). The CI job `image-check` also proves
    // that the binary in the image ignores `INGEST_TEST_DEGRADED_AFTER_MS`.
    let builds: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("RUN cargo"))
        .collect();
    assert_eq!(builds, ["RUN cargo build --release --locked -p ingest"]);
    // The server reads its TLS files from the directory that the
    // entrypoint writes.
    let config = json_file("ingest/server_config.json");
    let entrypoint = read("ingest/entrypoint.sh");
    for (key, file) in [("server_cert", "tls.crt"), ("server_key", "tls.key")] {
        let path = format!("{TLS_DIR}/{file}");
        assert_eq!(config["tls"][key], path.as_str());
        assert!(entrypoint.contains(&format!("> {path}\n")), "{path}");
    }
    // The last line starts ingest run through setpriv, never as root.
    assert!(
        entrypoint.ends_with(&format!(
            "\nexec setpriv {} {INGEST_BINARY} run\n",
            setpriv_options().join(" ")
        )),
        "the last line of the entrypoint is not the setpriv line"
    );
    assert_eq!(entrypoint.matches(INGEST_BINARY).count(), 1);
}

/// The Railway service settings that the lead applies by hand. Railway
/// reads no file of this repository for them (ADR 0001, lead decision 7).
/// The one documented place is the table "Railway settings of the service
/// `telemetry`" in STACK.md §15; the tests below check these values against
/// the image and the code, and check that the table holds the same values.
mod railway {
    pub const DOCKERFILE_PATH: &str = "/ingest/Dockerfile";
    pub const PRE_DEPLOY_COMMAND: &str = "/usr/local/bin/ingest migrate";
    pub const PRE_DEPLOY_TIMEOUT_SECONDS: u64 = 1200;
    pub const HEALTHCHECK_PATH: &str = "/health";
    pub const DRAINING_SECONDS: u64 = 30;
    pub const OVERLAP_SECONDS: u64 = 0;
    pub const HEALTHCHECK_TIMEOUT_SECONDS: u64 = 300;
    /// The service variable `NO_CACHE`. With the value `1`, Railway builds
    /// the image without its layer cache, so the `apt-get` step runs at
    /// each build and installs the Debian security fixes of the build date
    /// (STACK.md §15, "Build cache").
    pub const NO_CACHE: &str = "1";
    /// The port of `/health` and of the healthcheck (`PORT`).
    pub const PORT: u16 = 8080;
    /// Every input of the image, and nothing else: a change of the sources
    /// of another workspace member or of the documentation does not
    /// redeploy ingest. The manifest of each member is an input, because
    /// Cargo loads the whole workspace.
    pub const WATCH_PATHS: [&str; 10] = [
        "/ingest/**",
        "/crates/ingest/**",
        "/crates/*/Cargo.toml",
        "/db/**",
        "/migrations/**",
        "/.cargo/**",
        "/Cargo.toml",
        "/Cargo.lock",
        "/rust-toolchain.toml",
        "/.dockerignore",
    ];
}

/// The heading of the settings table of the ingest service in STACK.md §15.
const STACK_SETTINGS_HEADING: &str = "#### Railway settings of the service `telemetry`";

/// The row of the settings table of STACK.md §15 that starts with `name`.
/// The search reads only the subsection of the ingest service, up to the
/// next heading, so that the table of another service can use the same
/// row names.
fn stack_row(name: &str) -> String {
    let stack = read("STACK.md");
    let headings: Vec<&str> = stack
        .lines()
        .filter(|line| *line == STACK_SETTINGS_HEADING)
        .collect();
    assert_eq!(headings.len(), 1, "STACK.md has no single settings heading");
    let rows: Vec<&str> = stack
        .lines()
        .skip_while(|line| *line != STACK_SETTINGS_HEADING)
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .filter(|line| line.starts_with(&format!("| {name} ")))
        .collect();
    assert_eq!(rows.len(), 1, "STACK.md has no single row {name:?}");
    rows[0].to_owned()
}

/// The second cell of a table row.
fn value_cell(row: &str) -> String {
    row.split('|').nth(2).unwrap().trim().to_owned()
}

#[test]
fn the_railway_settings_match_the_image_and_the_code() {
    // The Dockerfile exists at the path, relative to the repository root.
    assert!(repository().join(&railway::DOCKERFILE_PATH[1..]).is_file());
    // One string: the Railway schema allows a string or an array with at
    // most one item. The command runs the binary that the image holds.
    assert_eq!(
        railway::PRE_DEPLOY_COMMAND,
        format!("{INGEST_BINARY} migrate")
    );
    // The Railway bound ends a pre-deploy command that never ends (for
    // example an old image that runs its server). It is longer than the
    // client deadline of `ingest migrate`, so it never cuts a migration.
    assert!(
        railway::PRE_DEPLOY_TIMEOUT_SECONDS > MIGRATE_DEADLINE.as_secs()
            && railway::PRE_DEPLOY_TIMEOUT_SECONDS <= 3600
    );
    // The health server serves this path.
    assert!(
        read("crates/ingest/src/http.rs")
            .contains(&format!(".route(\"{}\"", railway::HEALTHCHECK_PATH))
    );
    assert_eq!(railway::DRAINING_SECONDS, DRAINING.as_secs());
    assert!(BUDGET.total() < DRAINING);
    assert_eq!(railway::OVERLAP_SECONDS, 0);
    // `NO_CACHE` is a build variable of Railway, not an input of the image:
    // the Dockerfile has no `ARG` line, so no service variable reaches a
    // build step.
    assert_eq!(railway::NO_CACHE, "1");
    assert!(instructions().iter().all(|line| !line.starts_with("ARG ")));
}

#[test]
fn the_settings_table_of_stack_md_holds_the_same_values() {
    for (name, value) in [
        ("Root directory", "`/` (unchanged)".to_owned()),
        ("Dockerfile path", format!("`{}`", railway::DOCKERFILE_PATH)),
        (
            "Pre-deploy command",
            format!("`{}`", railway::PRE_DEPLOY_COMMAND),
        ),
        (
            "Pre-deploy timeout",
            format!("{} s", railway::PRE_DEPLOY_TIMEOUT_SECONDS),
        ),
        (
            "Healthcheck path",
            format!("`{}`", railway::HEALTHCHECK_PATH),
        ),
        (
            "Healthcheck timeout",
            format!("{} s (the default)", railway::HEALTHCHECK_TIMEOUT_SECONDS),
        ),
        ("`PORT`", format!("`{}`", railway::PORT)),
        ("Draining time", format!("{} s", railway::DRAINING_SECONDS)),
        ("Overlap time", format!("{} s", railway::OVERLAP_SECONDS)),
        ("Restart policy", "Always".to_owned()),
        ("`NO_CACHE`", format!("`{}`", railway::NO_CACHE)),
        ("Custom start command", "None".to_owned()),
    ] {
        assert_eq!(value_cell(&stack_row(name)), value, "{name}");
    }
    let watch = value_cell(&stack_row("Watch paths"));
    let listed: BTreeSet<&str> = watch
        .split(", ")
        .map(|item| item.trim_matches('`'))
        .collect();
    assert_eq!(listed, BTreeSet::from(railway::WATCH_PATHS));
}

/// The release runbook refers to the STACK.md §15 table and does not repeat
/// its values, so the table stays the one documented place.
#[test]
fn the_runbook_refers_to_the_table_and_repeats_no_value() {
    let runbook = read("docs/ingest-release.md");
    assert!(runbook.contains("\"Railway settings of the service `telemetry`\""));
    for value in [
        format!("{} s", railway::PRE_DEPLOY_TIMEOUT_SECONDS),
        format!(" {} s", railway::DRAINING_SECONDS),
        format!("{} s", railway::HEALTHCHECK_TIMEOUT_SECONDS),
        railway::PORT.to_string(),
        railway::PRE_DEPLOY_COMMAND.to_owned(),
        format!("`{}`", railway::DOCKERFILE_PATH),
        "OVERLAP_SECONDS` | ".to_owned(),
    ]
    .into_iter()
    .chain(railway::WATCH_PATHS.iter().map(|path| format!("`{path}`")))
    {
        assert!(!runbook.contains(&value), "the runbook repeats {value:?}");
    }
    // A stale or wrong value, too: no number follows the name of a timed
    // setting (for example "pre-deploy timeout of 900 s").
    let lower = runbook.to_lowercase();
    for name in [
        "pre-deploy timeout",
        "healthcheck timeout",
        "draining time",
        "overlap time",
    ] {
        for (at, _) in lower.match_indices(name) {
            let after: String = lower[at + name.len()..].chars().take(8).collect();
            assert!(
                !after.chars().any(|c| c.is_ascii_digit()),
                "the runbook gives a value after {name:?}: {after:?}"
            );
        }
    }
}

/// The build-context sources of `ingest/Dockerfile`, in the order of its
/// `COPY` lines.
fn context_inputs() -> Vec<String> {
    model_of("ingest/Dockerfile")
        .into_iter()
        .flat_map(|instruction| instruction.sources)
        .collect()
}

// BEGIN DOCKERFILE MODEL
// `crates/ingest/tests/deployment.rs` and `crates/server/tests/deployment.rs`
// hold this block in the same text, and a test in each file compares the two
// copies. Each test crate keeps its own copy, so that no file outside its
// crate is part of its build. A shared crate or a module inside one of the
// two crate directories would make the ingest tests depend on a web file, or
// the web tests on an ingest file.

/// One instruction that the Dockerfile model accepts: the number of its
/// first line, its text after the line joining, and the build-context
/// sources that it reads.
struct Instruction {
    line: usize,
    text: String,
    sources: Vec<String>,
}

/// The Dockerfile model: a text check, not a Dockerfile parser. It accepts a
/// small set of instruction forms, the forms that `ingest/Dockerfile` and
/// `webapp/Dockerfile` use, and refuses every other text with its line
/// number:
///
/// - `FROM <image>` and `FROM <image> AS <stage>`, without a flag;
/// - `WORKDIR <absolute path>`;
/// - `ENV <NAME>=<value>`, one variable;
/// - `COPY <source>... <destination>` and
///   `COPY --from=<stage> <path>... <destination>`, with no other token
///   that starts with `-`; each source of a `COPY` without `--from=` is a
///   plain relative path (letters, digits, `.`, `_`, `-`, and `/`, with no
///   `.` or `..` segment);
/// - `RUN <shell command>` (shell form), where the command does not start
///   with `-` or `[` and holds no `--mount`, `--network`, `--security`, or `<<`;
/// - `USER <uid>:<gid>`, `EXPOSE <port>`, and `CMD [...]`.
///
/// Characters and lines: each line is ASCII without a control character,
/// except tabs in the leading indentation (so no byte-order mark and no
/// carriage return other than the one of a CRLF line end). A comment line
/// with the form of a parser directive (`syntax`, `escape`, or `check`) is
/// refused on any line. In an instruction line, a backslash is allowed
/// only as the last character of a line of a `RUN` instruction, where it
/// continues the instruction (a comment line can hold a backslash
/// anywhere; the model and `BuildKit` skip it); a
/// backslash before it, whitespace after it, an empty or comment line
/// inside the continuation, and a file that ends inside the instruction are
/// refused. Every other instruction is one line.
///
/// For the accepted forms, the joining follows the `BuildKit` parser
/// (`moby/buildkit`, `frontend/dockerfile/parser/parser.go`, commit
/// `ad25d4b2af8e4e3230ec3178ea0d121129e8650d`): the first line loses its
/// leading whitespace, each continued line loses its final backslash, the
/// next line keeps its leading whitespace, and the parts are joined without
/// a separator.
///
/// The sources that the model lists are checked on disk too: no listed
/// source, parent directory, or entry below a listed directory is a
/// symbolic link, and the repository holds no symbolic link and no
/// submodule (`link_findings`).
///
/// Limits: the model reads the text of the Dockerfile; it does not run
/// Docker. It does not see an `ONBUILD` trigger of a base image or a
/// setting of the builder, such as `--build-context` or `BUILDKIT_SYNTAX`.
/// A base image or a `RUN` command can read what an accepted `COPY` put
/// into the image. The review of each Dockerfile change and the CI job
/// `image-check`, which builds both images, also cover the Dockerfiles.
fn dockerfile_model(text: &str) -> Result<Vec<Instruction>, String> {
    let mut instructions = Vec::new();
    // The first line number and the joined text of an open `RUN`.
    let mut open: Option<(usize, String)> = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        check_characters(number, raw)?;
        let trimmed = raw.trim();
        if is_parser_directive(trimmed) {
            return Err(format!(
                "line {number}: a parser directive is not allowed: {trimmed}"
            ));
        }
        let skip = trimmed.is_empty() || trimmed.starts_with('#');
        if skip && open.is_some() {
            return Err(format!(
                "line {number}: an empty or comment line inside a continuation is not allowed"
            ));
        }
        if skip {
            continue;
        }
        let (part, continues) = continuation(number, raw)?;
        let (start, joined) = match open.take() {
            // The next line of a continuation keeps its leading whitespace.
            Some((start, mut joined)) => {
                joined.push_str(part);
                (start, joined)
            }
            // The first line of an instruction loses its leading whitespace.
            None => (number, part.trim_start().to_owned()),
        };
        if continues {
            if !joined.starts_with("RUN ") {
                return Err(format!(
                    "line {start}: only a RUN instruction can continue on the next line"
                ));
            }
            open = Some((start, joined));
        } else {
            instructions.push(check_instruction(start, joined)?);
        }
    }
    if let Some((start, _)) = open {
        return Err(format!("line {start}: the file ends inside an instruction"));
    }
    Ok(instructions)
}

/// Refuses a line that is not ASCII or that holds a control character
/// outside its leading indentation of spaces and tabs.
fn check_characters(number: usize, raw: &str) -> Result<(), String> {
    if !raw.is_ascii() {
        return Err(format!(
            "line {number}: a non-ASCII character is not allowed"
        ));
    }
    let body = raw.trim_start_matches([' ', '\t']);
    if body.chars().any(|c| c.is_ascii_control()) {
        return Err(format!(
            "line {number}: a control character is not allowed outside the leading indentation"
        ));
    }
    Ok(())
}

/// The text of a line without its final continuation backslash, and true
/// when the line continues. A backslash is allowed only as the last
/// character, after a character that is not a backslash.
fn continuation(number: usize, raw: &str) -> Result<(&str, bool), String> {
    let (part, continues) = match raw.strip_suffix('\\') {
        Some(head) => (head, true),
        None => (raw, false),
    };
    if part.contains('\\') {
        return Err(format!(
            "line {number}: a backslash is allowed only as the last character of a line"
        ));
    }
    if continues && part.trim().is_empty() {
        return Err(format!(
            "line {number}: a line with only a backslash is not allowed"
        ));
    }
    Ok((part, continues))
}

/// True when `line` has the form of a parser directive: `#`, whitespace, a
/// name, `=`, and a value, with the name `syntax`, `escape`, or `check` in
/// any letter case (`directives.go` of the `BuildKit` parser).
fn is_parser_directive(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    let Some((name, _)) = rest.split_once('=') else {
        return false;
    };
    let name = name.trim().to_ascii_lowercase();
    ["syntax", "escape", "check"].contains(&name.as_str())
}

/// True when `word` is a plain path: letters, digits, `.`, `_`, `-`, and `/`.
fn is_plain_path(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// True when `word` is a plain relative path without a `.` or `..` segment
/// and without an empty segment.
fn is_plain_relative_path(word: &str) -> bool {
    is_plain_path(word)
        && !word.starts_with('/')
        && word
            .trim_end_matches('/')
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// True when `word` is a plain name of an image or a stage: a plain path
/// that can also hold `:` and `@`.
fn is_plain_name(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@'))
}

/// Checks one joined instruction against the forms of `dockerfile_model`.
fn check_instruction(line: usize, text: String) -> Result<Instruction, String> {
    let refuse = |reason: &str| Err(format!("line {line}: {reason}: {text}"));
    let words: Vec<&str> = text
        .split([' ', '\t'])
        .filter(|word| !word.is_empty())
        .collect();
    let Some((keyword, args)) = words.split_first() else {
        return refuse("an empty instruction");
    };
    let rest = text[keyword.len()..].trim_start_matches([' ', '\t']);
    let mut sources = Vec::new();
    let valid = match *keyword {
        "FROM" => match args {
            [image] => is_plain_name(image),
            [image, as_word, stage] => {
                *as_word == "AS" && is_plain_name(image) && is_plain_path(stage)
            }
            _ => false,
        },
        "WORKDIR" => matches!(args, [path] if path.starts_with('/') && is_plain_path(path)),
        "ENV" => matches!(args, [pair] if pair.split_once('=').is_some_and(|(name, value)| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && (value.is_empty() || is_plain_name(value))
        })),
        "COPY" => {
            let (from_stage, paths) = match args.split_first() {
                Some((flag, paths)) if flag.starts_with('-') => {
                    let Some(stage) = flag.strip_prefix("--from=") else {
                        return refuse("a COPY flag other than --from=<stage> is not allowed");
                    };
                    if !is_plain_path(stage) {
                        return refuse("the stage of --from is not a plain name");
                    }
                    (true, paths)
                }
                _ => (false, args),
            };
            if paths.iter().any(|path| path.starts_with('-')) {
                return refuse("a COPY flag after the first word is not allowed");
            }
            if paths.len() < 2 || !paths.iter().all(|path| is_plain_path(path)) {
                return refuse("COPY needs plain paths: sources and a destination");
            }
            if !from_stage {
                let (destination, inputs) = paths.split_last().unwrap_or((&"", &[]));
                if destination.is_empty() || !inputs.iter().all(|path| is_plain_relative_path(path))
                {
                    return refuse("a COPY source is not a plain relative path");
                }
                sources = inputs.iter().map(|path| (*path).to_owned()).collect();
            }
            true
        }
        "RUN" => {
            !rest.is_empty()
                && !rest.starts_with('-')
                && !rest.starts_with('[')
                && !["--mount", "--network", "--security", "<<"]
                    .iter()
                    .any(|word| text.contains(word))
        }
        "USER" => matches!(args, [user] if user.split_once(':').is_some_and(|(uid, gid)| {
            !uid.is_empty()
                && !gid.is_empty()
                && uid.chars().all(|c| c.is_ascii_digit())
                && gid.chars().all(|c| c.is_ascii_digit())
        })),
        "EXPOSE" => {
            matches!(args, [port] if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        }
        "CMD" => rest.starts_with('[') && rest.ends_with(']'),
        _ => return refuse("the instruction is not in the allowlist of the model"),
    };
    if !valid {
        return refuse("the form of the instruction is not in the allowlist of the model");
    }
    Ok(Instruction {
        line,
        text,
        sources,
    })
}

/// The model refuses each text outside its allowlist and lists the sources
/// of each accepted `COPY`. The refused texts cover the ways to read the
/// build context that the model does not list, the line forms where the
/// `BuildKit` parser joins or splits lines in another way than a simple
/// joiner, and one case of each rule.
#[test]
fn the_dockerfile_model_accepts_only_its_allowlist() {
    for text in [
        // Flags, other instructions, and mounts that read the context.
        "COPY --chmod=0644 web/index.html /build/web-index.html",
        "COPY --chmod=0644 crates/ingest/src/main.rs /build/x",
        "ADD web /build/web",
        "ADD crates/ingest /build/x",
        "RUN --mount=type=bind,source=web,target=/w true",
        "RUN true --mount=type=bind,source=web,target=/w",
        "RUN true --network=host",
        "RUN true --security=insecure",
        // BuildKit joins the lines without a separator: `RUN --mount=...`.
        "RUN -\\\n-mount=type=bind,source=web,target=/w cp -r /w /build/web-copy",
        "RUN -\\\n-mount=type=bind,source=crates/ingest,target=/w cp -r /w /build/ingest-copy",
        "RUN true \\\n  --mount=type=bind,source=web,target=/w",
        // BuildKit does not continue a line that ends with two backslashes.
        "RUN echo x\\\\\nADD web /build/web",
        "RUN echo x\\\\\nCOPY crates/ingest /build/x",
        // Other backslashes and continuations.
        "RUN echo \\x",
        "RUN echo x \\ \n    && true",
        "RUN echo x \\\n\\\n    && true",
        "COPY db \\\n    web/ /build/x",
        "COPY Cargo.toml Cargo.lock\\\n./",
        "RUN echo x \\",
        // BuildKit skips an empty or comment line inside a continuation.
        "COPY db web \\\n\n    db/",
        "COPY web crates/ingest \\\n\n    web/",
        "RUN echo x \\\n\n    && true",
        "RUN echo x \\\n# x\n    && true",
        // Parser directives, also after the first line.
        "# syntax=example.invalid/any-frontend:1\nFROM debian:trixie-slim",
        "# escape=`\nFROM debian:trixie-slim",
        "#check=skip=all\nFROM debian:trixie-slim",
        "FROM debian:trixie-slim\n# syntax=example.invalid/any-frontend:1",
        // Characters.
        "\u{feff}FROM debian:trixie-slim",
        "FROM debian:trixie-slim\rCOPY web /w",
        "COPY\tweb /build/w",
        "COPY\u{a0}web /build/w",
        "# \u{e4}\nFROM debian:trixie-slim",
        // COPY sources and flags.
        "COPY --from=build --link /etc/passwd /x",
        "COPY --link web /build/web",
        "COPY --from build /a /b",
        "COPY . /build/all",
        "COPY ../web /build/w",
        "COPY crates/./ingest /build/w",
        "COPY /etc/passwd /build/w",
        "COPY $D /build/web",
        "COPY \"crates/ingest\" /build/x",
        "COPY crates/in* /build/x",
        "COPY [\"web\", \"/build/web\"]",
        "COPY <<EOF /build/x",
        "COPY web",
        // One case of each other class.
        "ARG D=web",
        "copy web /build/web",
        "COPYX web /build/w",
        "ONBUILD COPY web /build/web",
        "ENTRYPOINT [\"/entrypoint.sh\"]",
        "RUN <<EOF",
        "RUN [\"true\"]",
        "FROM --platform=linux/amd64 debian:trixie-slim",
        "ENV A=1 B=2",
        "USER app",
    ] {
        assert!(
            dockerfile_model(text).is_err(),
            "the model accepts {text:?}"
        );
    }
    let error = dockerfile_model("FROM debian:trixie-slim\nWORKDIR /build\nADD web /w")
        .err()
        .unwrap_or_default();
    assert!(error.starts_with("line 3:"), "{error}");
    let model = dockerfile_model(
        "FROM rust:1.100.0-slim-trixie AS build\r\n\
         # a comment\n\
         \n\
         COPY a b/c ./\n\
         COPY --from=build /a /b\n\
         RUN apt-get update \\\n    && true\n\
         CMD [\"/x\"]",
    )
    .unwrap_or_else(|reason| panic!("{reason}"));
    let sources: Vec<&str> = model
        .iter()
        .flat_map(|instruction| instruction.sources.iter().map(String::as_str))
        .collect();
    assert_eq!(sources, ["a", "b/c"]);
    assert_eq!(model[3].line, 6);
    // The parts are joined without a separator, as in BuildKit.
    assert_eq!(model[3].text, "RUN apt-get update     && true");
}
/// The symbolic links and submodules that can carry a build of an image
/// outside its listed sources, each as a finding with its path. A `COPY`
/// from the build context follows a symbolic link, and the transfer of the
/// build context includes the target of the link, so a link at a listed
/// source can make an image read a file of the other image. The rule:
///
/// - the Git index holds no symbolic link (mode `120000`) and no submodule
///   (mode `160000`);
/// - in the work tree, no listed source and no parent directory of it is a
///   symbolic link, and no entry below a listed directory that the
///   `.dockerignore` rules (`in_context`) send into the build context is a
///   symbolic link; an untracked link is in the build context too.
///
/// Limit: the rule reads the Git index and the work tree of this checkout.
/// It does not see a link that a build tool makes during the build.
fn link_findings(
    root: &Path,
    sources: &[String],
    in_context: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut findings = Vec::new();
    match Command::new("git")
        .args(["ls-files", "-s", "-z"])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => {
            for entry in output.stdout.split(|byte| *byte == 0) {
                let entry = String::from_utf8_lossy(entry);
                let Some((meta, path)) = entry.split_once('\t') else {
                    continue;
                };
                match meta.split(' ').next() {
                    Some("120000") => findings.push(format!("tracked symbolic link: {path}")),
                    Some("160000") => findings.push(format!("tracked submodule: {path}")),
                    _ => {}
                }
            }
        }
        _ => findings.push("git ls-files -s failed".to_owned()),
    }
    for source in sources {
        let mut path = root.to_path_buf();
        let mut relative = String::new();
        for part in source.split('/').filter(|part| !part.is_empty()) {
            path.push(part);
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    findings.push(format!("symbolic link at a COPY source: {relative}"));
                }
                Ok(_) => {}
                Err(_) => findings.push(format!("missing COPY source: {relative}")),
            }
        }
        let mut pending = vec![(path, relative)];
        while let Some((dir, prefix)) = pending.pop() {
            let Ok(meta) = fs::symlink_metadata(&dir) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                findings.push(format!("unreadable directory: {prefix}"));
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let child = format!("{prefix}/{name}");
                if !in_context(&child) {
                    continue;
                }
                match entry.file_type() {
                    Ok(kind) if kind.is_symlink() => {
                        findings.push(format!("symbolic link below a COPY source: {child}"));
                    }
                    Ok(kind) if kind.is_dir() => pending.push((entry.path(), child)),
                    Ok(_) => {}
                    Err(_) => findings.push(format!("unreadable entry: {child}")),
                }
            }
        }
    }
    findings
}
// END DOCKERFILE MODEL

/// The model block of this file equals the block of `crates/server/tests/deployment.rs`.
#[test]
fn the_dockerfile_model_equals_the_other_copy() {
    let block = |path: &str| {
        let text = read(path);
        let start = text.find("// BEGIN DOCKERFILE MODEL").unwrap();
        let end = text.find("// END DOCKERFILE MODEL").unwrap();
        text[start..end].to_owned()
    };
    assert_eq!(
        block("crates/ingest/tests/deployment.rs"),
        block("crates/server/tests/deployment.rs")
    );
}

/// No `COPY` source of `ingest/Dockerfile` is, holds, or sits below a symbolic link,
/// and the repository holds no symbolic link and no submodule
/// (`link_findings`).
#[test]
fn the_build_inputs_hold_no_symbolic_link_or_submodule() {
    let findings = link_findings(&repository(), &context_inputs(), &in_build_context);
    assert!(findings.is_empty(), "{findings:#?}");
}

/// True when the glob `pattern` matches the relative `path`. Both are
/// split at `/`. A `**` item matches zero or more path items; a `*` inside
/// an item matches any characters except `/`. Railway watch paths and
/// `.dockerignore` rules use these two forms. This is a model of their
/// matchers, not their code: the test cannot run Railway or Docker.
fn glob_matches(pattern: &str, path: &str) -> bool {
    fn items(pattern: &[&str], path: &[&str]) -> bool {
        match (pattern.first(), path.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                items(&pattern[1..], path) || (!path.is_empty() && items(pattern, &path[1..]))
            }
            (Some(item), Some(name)) => {
                item_matches(item, name) && items(&pattern[1..], &path[1..])
            }
            _ => false,
        }
    }
    fn item_matches(item: &str, name: &str) -> bool {
        match item.split_once('*') {
            None => item == name,
            Some((head, rest)) => {
                name.starts_with(head)
                    && (head.len()..=name.len()).any(|at| item_matches(rest, &name[at..]))
            }
        }
    }
    let pattern: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    items(&pattern, &path)
}

/// True when a Railway watch path matches the file at the relative `path`.
fn watched(path: &str) -> bool {
    railway::WATCH_PATHS
        .iter()
        .any(|pattern| glob_matches(pattern, path))
}

/// True when the `.dockerignore` rules send the file at the relative `path`
/// into the build context. The last rule that matches the path, or a
/// parent directory of the path, decides; a rule with `!` includes.
fn in_build_context(path: &str) -> bool {
    let ignore = read(".dockerignore");
    let items: Vec<&str> = path.split('/').collect();
    let mut included = true;
    for rule in ignore
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (include, pattern) = match rule.strip_prefix('!') {
            Some(pattern) => (true, pattern),
            None => (false, rule),
        };
        if (1..=items.len()).any(|end| glob_matches(pattern, &items[..end].join("/"))) {
            included = include;
        }
    }
    included
}

#[test]
fn every_build_input_has_a_watch_path_and_a_context_entry() {
    let inputs = context_inputs();
    let mut expected: Vec<String> = ["Cargo.toml", "Cargo.lock", ".cargo", "crates/ingest"]
        .map(str::to_owned)
        .to_vec();
    expected.extend(
        stub_members()
            .iter()
            .map(|member| format!("{member}/Cargo.toml")),
    );
    expected.extend(
        [
            "db",
            "migrations",
            "ingest/server_config.json",
            "ingest/entrypoint.sh",
        ]
        .map(str::to_owned),
    );
    assert_eq!(inputs, expected);
    for input in &inputs {
        let path = repository().join(input);
        // A file of the input: the file itself, or a file in the directory.
        let file = if path.is_dir() {
            if input == ".cargo" {
                ".cargo/config.toml".to_owned()
            } else {
                format!("{input}/any/file")
            }
        } else {
            input.clone()
        };
        assert!(watched(&file), "no watch path for {input}");
        assert!(
            in_build_context(&file),
            "no .dockerignore entry for {input}"
        );
    }
    // The Dockerfile itself and the context rules also start a deployment.
    assert!(watched("ingest/Dockerfile"));
    assert!(watched(".dockerignore"));
    assert!(watched("crates/ingest/.sqlx/query-any.json"));
    assert!(in_build_context("crates/ingest/.sqlx/query-any.json"));
}

/// True when the `COPY` input `input` (a file or a directory of the build
/// context) reads the file at the relative `path`: the input is the path
/// itself or one of its parent directories.
fn input_reads(input: &str, path: &str) -> bool {
    path == input
        || path
            .strip_prefix(input)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True when a `COPY` line of `ingest/Dockerfile` reads the file at the
/// relative `path`.
fn copied(path: &str) -> bool {
    context_inputs()
        .iter()
        .any(|input| input_reads(input, path))
}

/// The files of the other workspace members, of the web application, and
/// of the documentation are not inputs of the ingest image: no watch path
/// matches them, and no `COPY` line reads them. So a merge that changes
/// only these files does not redeploy ingest, and the image does not
/// change.
///
/// The root `.dockerignore` serves the ingest image and the web image
/// (`webapp/Dockerfile`), so the web files are in the build context. The
/// build context is not the guarantee: the `COPY` lines are. The files that
/// no image needs (documents, the root `.sqlx/`, secrets, and build output)
/// stay out of the build context.
#[test]
fn other_members_and_documents_are_not_image_inputs() {
    let web_files = [
        "crates/server/src/main.rs",
        "crates/server/src/battery/mod.rs",
        "crates/server/.sqlx/query-any.json",
        "crates/server/tests/battery.rs",
        "crates/domain/src/lib.rs",
        "web/src/main.tsx",
        "web/index.html",
        "packages/api-client/src/generated/sdk.gen.ts",
        "webapp/Dockerfile",
        "e2e/tests/login.spec.ts",
        "contract/openapi.json",
        "package.json",
        "pnpm-lock.yaml",
        "pnpm-workspace.yaml",
        ".npmrc",
        "tsconfig.base.json",
        "tsconfig.json",
        "scripts/contract-check.sh",
        "scripts/image-check-web.sh",
    ];
    let outside_every_image = [
        ".sqlx/query-any.json",
        "docs/ingest-release.md",
        "docs/adr/0001-ingest.md",
        "STACK.md",
        "README.md",
        "e2e/tests/login.spec.ts",
        "contract/openapi.json",
        "scripts/contract-check.sh",
        ".github/workflows/ci.yml",
    ];
    for path in web_files.iter().chain(&outside_every_image) {
        assert!(!watched(path), "an ingest watch path matches {path}");
        assert!(
            !copied(path),
            "a COPY line of ingest/Dockerfile reads {path}"
        );
    }
    for path in outside_every_image {
        assert!(!in_build_context(path), "the build context holds {path}");
    }
    // Control cases of the models. A member manifest is an input. A COPY
    // input reads the files below it, so a wider input such as `crates`
    // would read the web sources.
    for path in ["crates/server/Cargo.toml", "crates/domain/Cargo.toml"] {
        assert!(watched(path), "no watch path matches {path}");
        assert!(copied(path), "no COPY line reads {path}");
        assert!(in_build_context(path), "the build context lacks {path}");
    }
    assert!(copied("crates/ingest/src/main.rs"));
    assert!(input_reads("crates", "crates/server/src/main.rs"));
    assert!(!input_reads(
        "crates/ingest",
        "crates/ingest-web/src/lib.rs"
    ));
    // Secrets and build output stay out of the build context, also inside
    // an included directory.
    for path in [
        "crates/ingest/.env",
        "crates/ingest/target/x",
        "crates/server/secrets/key",
        "web/.env.local",
        "web/node_modules/vite/package.json",
        "web/dist/index.html",
        "packages/api-client/node_modules/zod/package.json",
        "node_modules/.pnpm/lock.yaml",
    ] {
        assert!(!in_build_context(path), "the build context holds {path}");
    }
}

/// The first-party packages of the workspace, from `cargo metadata`.
fn cargo_metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(repository())
        .output()
        .unwrap();
    assert!(output.status.success(), "cargo metadata failed");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The workspace members other than `crates/ingest`, as paths relative to
/// the repository, from `cargo metadata`.
fn workspace_members() -> BTreeSet<String> {
    let metadata = cargo_metadata();
    let root = Path::new(metadata["workspace_root"].as_str().unwrap());
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| {
            let manifest = Path::new(package["manifest_path"].as_str().unwrap());
            manifest
                .parent()
                .unwrap()
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The name of each first-party package, from `cargo metadata`.
fn package_names() -> Vec<String> {
    let metadata = cargo_metadata();
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| package["name"].as_str().unwrap().to_owned())
        .collect()
}

/// osv-scanner reads each `Cargo.lock` package by its name as a crates.io
/// package, also a first-party package without a `source`. A first-party
/// package with the name of a crates.io crate gets the advisories of that
/// crate (the name `domain` got RUSTSEC-2026-0310). So each first-party
/// package name other than `ingest` starts with `telemetry-`. `ingest` is
/// also a crates.io name, but the Railway pre-deploy command, the image,
/// and `-p ingest` use it; it has no advisory now.
#[test]
fn first_party_package_names_have_the_project_prefix() {
    let names = package_names();
    assert!(names.iter().any(|name| name == "ingest"));
    for name in names.iter().filter(|name| *name != "ingest") {
        assert!(
            name.starts_with("telemetry-"),
            "the package name {name} has no telemetry- prefix"
        );
    }
}

/// The members that the Dockerfile copies as a manifest with a stub target,
/// in the order of its `COPY` lines.
fn stub_members() -> Vec<String> {
    context_inputs()
        .iter()
        .filter_map(|input| input.strip_suffix("/Cargo.toml"))
        .filter(|member| member.starts_with("crates/"))
        .map(str::to_owned)
        .collect()
}

/// The image build reads the manifest of each other workspace member and
/// makes an empty `src/lib.rs` for it, so that Cargo can load the
/// workspace. A member that the list does not name would stop the build
/// (`--locked` cannot load the workspace); a member that the list names
/// but the workspace does not have would also stop the build.
#[test]
fn the_dockerfile_stubs_every_other_workspace_member() {
    let mut others = workspace_members();
    assert!(others.remove("crates/ingest"));
    let stubs = stub_members();
    assert_eq!(stubs.iter().cloned().collect::<BTreeSet<_>>(), others);
    assert_eq!(stubs.len(), others.len());
    // The one RUN line that makes the stubs names the same members.
    let runs: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("RUN for member in "))
        .collect();
    assert_eq!(
        runs,
        [format!(
            "RUN for member in {}; do mkdir -p \"$member/src\" && : > \"$member/src/lib.rs\"; done",
            stubs.join(" ")
        )]
    );
    // A stub `src/lib.rs` works only with the automatic target discovery
    // of Cargo: an explicit target or build script path would name a file
    // that the image does not have.
    for member in &stubs {
        let manifest = read(&format!("{member}/Cargo.toml"));
        for line in manifest.lines().map(str::trim) {
            assert!(
                !(line.starts_with("[lib]")
                    || line.starts_with("[[")
                    || line.starts_with("path")
                    || line.starts_with("build ")
                    || line.starts_with("build=")),
                "{member}/Cargo.toml declares a target or build script: {line}"
            );
        }
    }
}

/// The `.dockerignore` rules, in order (an allowlist: `*` first, then the
/// build inputs of the ingest image and of the web image, then the
/// exclusions inside them). The order decides the meaning, so the test
/// compares the exact rules. With these rules, no `.env*` file, `secrets/`,
/// `.bin/`, `.git`, `target/`, `node_modules/`, or `dist/` directory can
/// enter the build context: `*` excludes every top-level entry, no include
/// rule names one of them, and the last rules exclude them again inside the
/// included directories. The web rules let the web image read its inputs;
/// `other_members_and_documents_are_not_image_inputs` shows that the ingest
/// image does not read them.
#[test]
fn the_build_context_excludes_secrets_and_build_output() {
    let ignore = read(".dockerignore");
    let rules: Vec<&str> = ignore
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        rules,
        [
            "*",
            "!Cargo.toml",
            "!Cargo.lock",
            "!.cargo/config.toml",
            "!crates/*/Cargo.toml",
            "!crates/ingest/",
            "!db/",
            "!migrations/",
            "!ingest/",
            "!crates/server/",
            "!crates/domain/",
            "!webapp/",
            "!package.json",
            "!pnpm-lock.yaml",
            "!pnpm-workspace.yaml",
            "!.npmrc",
            "!tsconfig.base.json",
            "!packages/",
            "!web/",
            "**/.env*",
            "**/secrets",
            "**/.bin",
            "**/target",
            "**/node_modules",
            "**/dist",
        ]
    );
    for forbidden in [
        ".env",
        "secrets",
        ".bin",
        ".git",
        "target",
        "node_modules",
        "dist",
    ] {
        assert!(
            rules
                .iter()
                .filter(|rule| rule.starts_with('!'))
                .all(|rule| !rule.contains(forbidden)),
            "an include rule names {forbidden}"
        );
    }
}

/// A new, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ingest-deployment-{}-{name}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The stub programs of the entrypoint test. A stub writes its arguments
/// (one on each line) and its environment to files named after the stub.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stub {
    Setpriv,
    Chown,
}

impl Stub {
    fn name(self) -> &'static str {
        match self {
            Stub::Setpriv => "setpriv",
            Stub::Chown => "chown",
        }
    }
}

/// A copy of the entrypoint with temporary paths, a fake `ingest` that
/// writes its arguments, its environment, and its open-file limits, and a
/// directory `bin` with the stubs of `setpriv` and `chown` for `PATH`.
///
/// The `setpriv` stub skips its options and replaces itself with the rest
/// of its arguments (`exec`), like `setpriv`. The `chown` stub changes no
/// owner. A stub named in `failing` exits with 1 after it wrote its files.
fn entrypoint_copy(dir: &Path, failing: Option<Stub>) -> PathBuf {
    let tls = dir.join("tls");
    fs::create_dir(&tls).unwrap();
    fs::set_permissions(&tls, fs::Permissions::from_mode(0o700)).unwrap();
    let out = dir.display();
    let fake = dir.join("fake-ingest");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$@\" > {out}/args\nenv > {out}/env\n\
             ulimit -S -n > {out}/soft\nulimit -H -n > {out}/hard\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    for stub in [Stub::Setpriv, Stub::Chown] {
        let name = stub.name();
        let rest = if failing == Some(stub) {
            "exit 1\n".to_owned()
        } else if stub == Stub::Setpriv {
            format!("shift {}\nexec \"$@\"\n", setpriv_options().len())
        } else {
            String::new()
        };
        let path = bin.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {out}/{name}-args\nenv > {out}/{name}-env\n{rest}"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let original = read("ingest/entrypoint.sh");
    // rm, the two writes, and chown name the files of the directory.
    assert_eq!(original.matches(TLS_DIR).count(), 6);
    assert_eq!(original.matches(INGEST_BINARY).count(), 1);
    let script = dir.join("entrypoint.sh");
    fs::write(
        &script,
        original
            .replace(TLS_DIR, &tls.display().to_string())
            .replace(INGEST_BINARY, &fake.display().to_string()),
    )
    .unwrap();
    script
}

const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIBfake\n-----END CERTIFICATE-----";
const KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIGHfakekey\n-----END PRIVATE KEY-----";

/// `PATH` for the copy: the stubs first.
fn stub_path(dir: &Path) -> String {
    format!("{}:/bin:/usr/bin", dir.join("bin").display())
}

/// Runs the copy with the variables of a Railway start.
fn run_entrypoint(dir: &Path, script: &Path) -> std::process::Output {
    run_entrypoint_with(dir, script, &[])
}

/// Runs the copy with the variables of a Railway start and the `extra`
/// variables.
fn run_entrypoint_with(dir: &Path, script: &Path, extra: &[(&str, &str)]) -> std::process::Output {
    Command::new("/bin/sh")
        .arg(script)
        .env_clear()
        .env("PATH", stub_path(dir))
        .env("TLS_CERT", CERT)
        .env("TLS_KEY", KEY)
        .env("DATABASE_URL", "postgresql://u:p@db.invalid/neondb")
        .env(
            "MIGRATION_DATABASE_URL",
            "postgresql://telemetry_migrator:migrator-secret@db.invalid/neondb",
        )
        .envs(extra.iter().copied())
        .output()
        .unwrap()
}

/// Fails when an environment holds a TLS variable, the key, or the
/// migrator URL.
fn assert_no_secret_in(env: &str, what: &str) {
    for secret in [
        "TLS_",
        "PRIVATE KEY",
        "MIGRATION_DATABASE_URL",
        "migrator-secret",
    ] {
        assert!(!env.contains(secret), "{what} holds {secret:?}:\n{env}");
    }
}

#[test]
fn the_entrypoint_writes_private_tls_files_and_starts_ingest_without_them() {
    let dir = scratch("writes");
    let script = entrypoint_copy(&dir, None);
    let output = run_entrypoint(&dir, &script);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"");
    // Each file holds the variable and a final newline, with mode 0600.
    let tls = dir.join("tls");
    for (file, content) in [("tls.crt", CERT), ("tls.key", KEY)] {
        let path = tls.join(file);
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{content}\n"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{file}");
    }
    // chown gives both files to user and group 10001. In the image, root
    // runs it; here the stub records the arguments.
    assert_eq!(
        fs::read_to_string(dir.join("chown-args")).unwrap(),
        format!(
            "{INGEST_ID}:{INGEST_ID}\n{}\n{}\n",
            tls.join("tls.crt").display(),
            tls.join("tls.key").display()
        )
    );
    // `exec setpriv` with the exact options, then ingest run. The setpriv
    // stub replaced itself with ingest, so the arguments of ingest are
    // `run`.
    let mut setpriv = setpriv_options();
    setpriv.push(dir.join("fake-ingest").display().to_string());
    setpriv.push("run".to_owned());
    assert_eq!(
        fs::read_to_string(dir.join("setpriv-args")).unwrap(),
        format!("{}\n", setpriv.join("\n"))
    );
    assert_eq!(fs::read_to_string(dir.join("args")).unwrap(), "run\n");
    // The key is never a process argument.
    for file in ["chown-args", "setpriv-args", "args"] {
        let args = fs::read_to_string(dir.join(file)).unwrap();
        assert!(
            !args.contains("PRIVATE KEY") && !args.contains("fakekey"),
            "{file}"
        );
    }
    // `ingest run` needs only DATABASE_URL. fleet-telemetry runs as the
    // same user and can read the environment of ingest, so the TLS
    // variables and the migrator URL must not reach it. chown and setpriv
    // run after the unset, so they do not get them either.
    let env = fs::read_to_string(dir.join("env")).unwrap();
    assert!(
        env.lines().any(|line| line.starts_with("DATABASE_URL=")),
        "{env}"
    );
    assert_no_secret_in(&env, "the environment of ingest");
    for stub in [Stub::Chown, Stub::Setpriv] {
        let file = format!("{}-env", stub.name());
        let stub_env = fs::read_to_string(dir.join(&file)).unwrap();
        assert!(
            stub_env
                .lines()
                .any(|line| line.starts_with("DATABASE_URL=")),
            "{file}: {stub_env}"
        );
        assert_no_secret_in(&stub_env, &file);
    }
    // The soft open-file limit equals the hard limit.
    assert_eq!(
        fs::read_to_string(dir.join("soft")).unwrap(),
        fs::read_to_string(dir.join("hard")).unwrap()
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// The variables `cert` and `key` of the script must not reach a program
/// (issue #31). A container environment can already hold variables with
/// these names, for example from a Railway service variable. The shell then
/// keeps each name exported, and an assignment exports the TLS value.
#[test]
fn the_entrypoint_does_not_export_inherited_cert_and_key_variables() {
    let inherited = [("cert", "inherited-cert"), ("key", "inherited-key")];
    let dir = scratch("inherited");
    let script = entrypoint_copy(&dir, None);
    let output = run_entrypoint_with(&dir, &script, &inherited);
    assert!(output.status.success(), "{output:?}");
    for file in ["env", "chown-env", "setpriv-env"] {
        let env = fs::read_to_string(dir.join(file)).unwrap();
        assert!(
            !env.lines()
                .any(|line| line.starts_with("cert=") || line.starts_with("key=")),
            "{file} holds cert or key:\n{env}"
        );
        assert!(!env.contains("BEGIN CERTIFICATE"), "{file}:\n{env}");
        assert_no_secret_in(&env, file);
    }
    fs::remove_dir_all(&dir).unwrap();
    // Premise: without the unset, the TLS key reaches ingest. So the
    // checks above can detect the fault.
    let dir = scratch("inherited-unfixed");
    let script = entrypoint_copy(&dir, None);
    let fixed = fs::read_to_string(&script).unwrap();
    let unfixed = fixed.replacen("unset cert key\n", "", 1);
    assert_ne!(fixed, unfixed, "the script has no line `unset cert key`");
    fs::write(&script, unfixed).unwrap();
    let output = run_entrypoint_with(&dir, &script, &inherited);
    assert!(output.status.success(), "{output:?}");
    let env = fs::read_to_string(dir.join("env")).unwrap();
    assert!(env.contains("PRIVATE KEY"), "{env}");
    fs::remove_dir_all(&dir).unwrap();
}

/// A container that starts again can keep the files of the earlier start.
/// A process of user 10001 can replace them with symbolic links, because
/// the directory belongs to that user. The root shell must not write
/// through such a link: it removes the link and creates a new file.
#[test]
fn the_entrypoint_replaces_old_files_and_does_not_write_through_a_link() {
    let dir = scratch("link");
    let script = entrypoint_copy(&dir, None);
    let victim = dir.join("victim");
    fs::write(&victim, "unchanged\n").unwrap();
    let tls = dir.join("tls");
    std::os::unix::fs::symlink(&victim, tls.join("tls.key")).unwrap();
    fs::write(tls.join("tls.crt"), "old certificate\n").unwrap();
    let output = run_entrypoint(&dir, &script);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "unchanged\n");
    for (file, content) in [("tls.crt", CERT), ("tls.key", KEY)] {
        let path = tls.join(file);
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_file(), "{file} is not a regular file");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{file}");
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{content}\n"));
    }
    assert_eq!(fs::read_to_string(dir.join("args")).unwrap(), "run\n");
    fs::remove_dir_all(&dir).unwrap();
}

/// Fail closed (issue #29): when chown or setpriv fails, ingest does not
/// start, and the script exits with an error. In the image, the container
/// then stops.
#[test]
fn the_entrypoint_stops_when_chown_or_setpriv_fails() {
    for stub in [Stub::Chown, Stub::Setpriv] {
        let name = stub.name();
        let dir = scratch(&format!("failing-{name}"));
        let script = entrypoint_copy(&dir, Some(stub));
        let output = run_entrypoint(&dir, &script);
        assert!(!output.status.success(), "{name}: {output:?}");
        // The failing stub ran, and ingest did not start.
        assert!(dir.join(format!("{name}-args")).exists(), "{name}");
        assert!(!dir.join("args").exists(), "{name}");
        // After a failed chown, setpriv does not run either.
        if stub == Stub::Chown {
            assert!(!dir.join("setpriv-args").exists());
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn the_entrypoint_stops_without_a_tls_variable_and_does_not_show_the_other() {
    for (missing, present, value) in [("TLS_KEY", "TLS_CERT", CERT), ("TLS_CERT", "TLS_KEY", KEY)] {
        let dir = scratch(&format!("missing-{missing}"));
        let script = entrypoint_copy(&dir, None);
        let output = Command::new("/bin/sh")
            .arg(&script)
            .env_clear()
            .env("PATH", stub_path(&dir))
            .env(present, value)
            .env(missing, "")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(missing), "{stderr}");
        assert!(!stderr.contains("fake"), "{stderr}");
        // ingest did not start, no program ran, and no key file exists.
        assert!(!dir.join("args").exists());
        assert!(!dir.join("chown-args").exists());
        assert!(!dir.join("setpriv-args").exists());
        assert!(!dir.join("tls").join("tls.key").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
