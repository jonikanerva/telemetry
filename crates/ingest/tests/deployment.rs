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

/// The instruction lines of the Dockerfile, one item for each instruction.
/// A line that ends with a backslash continues on the next line.
fn instructions() -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in read("ingest/Dockerfile").lines() {
        let line = line.trim();
        if current.is_empty() && (line.is_empty() || line.starts_with('#')) {
            continue;
        }
        if let Some(start) = line.strip_suffix('\\') {
            current.push_str(start.trim_end());
            current.push(' ');
        } else {
            current.push_str(line);
            lines.push(std::mem::take(&mut current));
        }
    }
    assert!(
        current.is_empty(),
        "the Dockerfile ends inside an instruction"
    );
    lines
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
    // `reliable_ack`; the earlier configuration had one without effect.
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
    /// The port of `/health` and of the healthcheck (`PORT`).
    pub const PORT: u16 = 8080;
    /// Every input of the image, and nothing else: a web or documentation
    /// merge does not redeploy ingest.
    pub const WATCH_PATHS: [&str; 10] = [
        "/ingest/**",
        "/crates/**",
        "/db/**",
        "/migrations/**",
        "/.sqlx/**",
        "/.cargo/**",
        "/Cargo.toml",
        "/Cargo.lock",
        "/rust-toolchain.toml",
        "/.dockerignore",
    ];
}

/// The row of the settings table of STACK.md §15 that starts with `name`.
fn stack_row(name: &str) -> String {
    let stack = read("STACK.md");
    let rows: Vec<&str> = stack
        .lines()
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

/// The sources of the `COPY` lines that read the build context.
fn context_inputs() -> Vec<String> {
    instructions()
        .iter()
        .filter_map(|line| line.strip_prefix("COPY "))
        .filter(|rest| !rest.starts_with("--"))
        .flat_map(|rest| {
            let mut parts: Vec<&str> = rest.split_whitespace().collect();
            parts.pop();
            parts.into_iter().map(str::to_owned).collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn every_build_input_has_a_watch_path_and_a_context_entry() {
    let inputs = context_inputs();
    assert_eq!(
        inputs,
        [
            "Cargo.toml",
            "Cargo.lock",
            ".cargo",
            "crates",
            "db",
            "migrations",
            ".sqlx",
            "ingest/server_config.json",
            "ingest/entrypoint.sh",
        ]
    );
    let watch = railway::WATCH_PATHS;
    let ignore = read(".dockerignore");
    let included: Vec<&str> = ignore
        .lines()
        .filter_map(|line| line.strip_prefix('!'))
        .collect();
    for input in &inputs {
        let path = repository().join(input);
        let (pattern, entry) = if path.is_dir() {
            (format!("/{input}/**"), format!("{input}/"))
        } else {
            (format!("/{input}"), input.clone())
        };
        let parent = input.split('/').next().unwrap();
        assert!(
            watch.contains(&pattern.as_str()) || watch.contains(&format!("/{parent}/**").as_str()),
            "no watch path for {input}"
        );
        assert!(
            included.contains(&entry.as_str())
                || included.contains(&format!("{parent}/").as_str())
                || (input == ".cargo" && included.contains(&".cargo/config.toml")),
            "no .dockerignore entry for {input}"
        );
    }
    // The Dockerfile itself and the context rules also start a deployment.
    assert!(watch.contains(&"/ingest/**"));
    assert!(watch.contains(&"/.dockerignore"));
}

/// The `.dockerignore` rules, in order (an allowlist: `*` first, then the
/// build inputs, then the exclusions inside them). The order decides the
/// meaning, so the test compares the exact rules. With these rules, no
/// `.env*` file, `secrets/`, `.bin/`, `.git`, or `target/` directory can
/// enter the build context: `*` excludes every top-level entry, no
/// include rule names one of them, and the last rules exclude them again
/// inside the included directories.
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
            "!crates/",
            "!db/",
            "!migrations/",
            "!.sqlx/",
            "!ingest/",
            "**/.env*",
            "**/secrets",
            "**/.bin",
            "**/target",
        ]
    );
    for forbidden in [".env", "secrets", ".bin", ".git", "target"] {
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
