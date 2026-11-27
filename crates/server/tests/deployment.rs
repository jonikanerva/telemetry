//! Tests of the deployment files of the web image (`webapp/Dockerfile`)
//! and of the Railway service `telemetry-web`. No container runtime runs
//! here: the tests read the files and check that they agree with each
//! other, with the tool pins, and with the code.
//!
//! - The Railway service settings that the lead applies by hand (constants
//!   in the module `railway`) agree with the image, the code, and the
//!   settings table of STACK.md §15.
//! - The image runs the release server as user 10001 with the SPA files in
//!   `WEB_STATIC_DIR`. The release build never has the feature
//!   `test-server`, and the image has no build argument.
//! - The builder images and the pnpm version are the pins of `mise.toml`
//!   and `rust-toolchain.toml`. The runtime image is the base image of the
//!   ingest image, so the Debian 13 triage of
//!   `docs/security/image-advisories.md` applies to both.
//! - Each input that the image build copies has a web watch path and an
//!   entry in `.dockerignore`. The ingest files match no web watch path,
//!   and no `COPY` line of the web image reads them.
//!
//! `crates/ingest/tests/deployment.rs` holds the same models of the Railway
//! watch paths and the `.dockerignore` rules for the ingest image. The two
//! test crates share no code: `crates/server` must not depend on `ingest`.
//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! Clippy, so that they may use `unwrap` and `panic` like the tests do.
#![cfg(test)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use telemetry_web_server::shutdown::SHUTDOWN_DEADLINE;

const DOCKERFILE: &str = "webapp/Dockerfile";
const SERVER_BINARY: &str = "/usr/local/bin/telemetry-web-server";
const STATIC_DIR: &str = "/usr/local/share/telemetry-web";
/// The user and group of the server process.
const WEB_ID: u32 = 10001;
/// The workspace members that the web image builds from their sources.
const BUILT_MEMBERS: [&str; 2] = ["crates/server", "crates/domain"];

/// The Railway service settings of `telemetry-web` that the lead applies by
/// hand. Railway reads no file of this repository for them. The one
/// documented place is the table "Railway settings of the service
/// `telemetry-web`" in STACK.md §15; the tests below check these values
/// against the image and the code, and check that the table holds the same
/// values.
mod railway {
    pub const DOCKERFILE_PATH: &str = "/webapp/Dockerfile";
    pub const HEALTHCHECK_PATH: &str = "/health";
    pub const HEALTHCHECK_TIMEOUT_SECONDS: u64 = 300;
    /// The service variable `NO_CACHE`. With the value `1`, Railway builds
    /// the image without its layer cache, so the `apt-get` step runs at
    /// each build and installs the Debian security fixes of the build date
    /// (STACK.md §15, "Build cache").
    pub const NO_CACHE: &str = "1";
    /// The port of the server and of the healthcheck (`PORT`).
    pub const PORT: u16 = 8080;
    pub const DRAINING_SECONDS: u64 = 15;
    pub const OVERLAP_SECONDS: u64 = 0;
    /// Every input of the image, and nothing else: a change of the ingest
    /// sources, of the browser tests, or of the documentation does not
    /// redeploy the web service. The manifest of each member is an input,
    /// because Cargo loads the whole workspace.
    pub const WATCH_PATHS: [&str; 16] = [
        "/webapp/**",
        "/web/**",
        "/packages/**",
        "/crates/server/**",
        "/crates/domain/**",
        "/crates/*/Cargo.toml",
        "/Cargo.toml",
        "/Cargo.lock",
        "/.cargo/**",
        "/rust-toolchain.toml",
        "/package.json",
        "/pnpm-lock.yaml",
        "/pnpm-workspace.yaml",
        "/.npmrc",
        "/tsconfig.base.json",
        "/.dockerignore",
    ];
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    fs::read_to_string(repository().join(path)).unwrap()
}

/// The instruction texts of the Dockerfile at `path`, after the checks of
/// the Dockerfile model (`dockerfile_model`), with each run of whitespace as
/// one space.
fn instructions_of(path: &str) -> Vec<String> {
    model_of(path)
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

fn instructions() -> Vec<String> {
    instructions_of(DOCKERFILE)
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

/// The value of a line `name = "value"` in the table `[section]` of a TOML
/// file. The search stops at the next table, so a task script with the
/// same name does not count.
fn toml_string(path: &str, section: &str, name: &str) -> String {
    let header = format!("[{section}]");
    let values: Vec<String> = read(path)
        .lines()
        .skip_while(|line| line.trim() != header)
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().trim_matches('"').to_owned())
        })
        .collect();
    assert_eq!(values.len(), 1, "{path} has no single {name} line");
    values[0].clone()
}

/// The Debian codename of the final stage of a Dockerfile
/// (`FROM debian:<codename>-slim`).
fn debian_codename(path: &str) -> String {
    let lines = instructions_of(path);
    let last = lines
        .iter()
        .rfind(|line| line.starts_with("FROM "))
        .unwrap();
    last.strip_prefix("FROM debian:")
        .and_then(|rest| rest.strip_suffix("-slim"))
        .unwrap_or_else(|| panic!("the final stage of {path} is not debian:<codename>-slim"))
        .to_owned()
}

/// The `FROM` lines are the pinned builder images and the base image of the
/// ingest image. The Node version and the pnpm version are the pins of
/// `mise.toml`; the Rust version is the channel of `rust-toolchain.toml`.
/// Each builder runs on the Debian release of the final stage, so that the
/// server links against the same glibc. No line names a build argument.
#[test]
fn the_images_and_tools_are_the_pinned_versions() {
    let codename = debian_codename(DOCKERFILE);
    assert_eq!(codename, debian_codename("ingest/Dockerfile"));
    let node = toml_string("mise.toml", "tools", "node");
    let pnpm = toml_string("mise.toml", "tools", "pnpm");
    let rust = toml_string("rust-toolchain.toml", "toolchain", "channel");
    let froms: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("FROM "))
        .collect();
    assert_eq!(
        froms,
        [
            format!("FROM node:{node}-{codename}-slim AS web"),
            format!("FROM rust:{rust}-slim-{codename} AS build"),
            format!("FROM debian:{codename}-slim"),
        ]
    );
    let installs: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.contains("pnpm@"))
        .collect();
    assert_eq!(installs, [format!("RUN npm install -g pnpm@{pnpm}")]);
    // No build argument: no secret and no service variable reaches a build
    // step through the Dockerfile.
    assert!(
        instructions().iter().all(|line| !line.starts_with("ARG ")),
        "{DOCKERFILE} has an ARG line"
    );
}

/// The image runs the release server as user 10001 with the SPA files of
/// the `web` stage. The release build has no feature, so the test-only
/// routes of the feature `test-server` are not in the binary.
#[test]
fn the_image_runs_the_release_server_as_user_10001() {
    let stage = final_stage();
    for expected in [
        format!("COPY --from=build /build/target/release/telemetry-web-server {SERVER_BINARY}"),
        format!("COPY --from=web /build/web/dist {STATIC_DIR}"),
        format!("ENV WEB_STATIC_DIR={STATIC_DIR}"),
        format!("USER {WEB_ID}:{WEB_ID}"),
        format!("EXPOSE {}", railway::PORT),
    ] {
        assert!(stage.contains(&expected), "missing: {expected}\n{stage:#?}");
    }
    // The only ENV line of the final stage: the service variables hold
    // PORT and the secrets.
    let envs: Vec<&String> = stage
        .iter()
        .filter(|line| line.starts_with("ENV "))
        .collect();
    assert_eq!(envs, [&format!("ENV WEB_STATIC_DIR={STATIC_DIR}")]);
    // One USER line, in the final stage, before CMD.
    let users: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("USER "))
        .collect();
    assert_eq!(users, [format!("USER {WEB_ID}:{WEB_ID}")]);
    // The last instruction starts the production command `serve`.
    let cmd = stage.last().unwrap().strip_prefix("CMD ").unwrap();
    let cmd: Value = serde_json::from_str(cmd).unwrap();
    assert_eq!(cmd, serde_json::json!([SERVER_BINARY, "serve"]));
    assert!(
        instructions()
            .iter()
            .all(|line| !line.starts_with("ENTRYPOINT")),
        "{DOCKERFILE} has an ENTRYPOINT line"
    );
    // Exactly one release build, without a feature and without another
    // profile; the final stage copies from target/release/ (above).
    let builds: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("RUN ") && line.contains("cargo "))
        .collect();
    assert_eq!(
        builds,
        ["RUN cargo build --release --locked -p telemetry-web-server"]
    );
    assert!(
        instructions()
            .iter()
            .all(|line| !line.contains("test-server") && !line.contains("--features")),
        "{DOCKERFILE} names a Cargo feature"
    );
    // The SPA build uses the committed client: the stage installs with the
    // frozen lockfile and builds, and generates nothing.
    let pnpm: Vec<String> = instructions()
        .into_iter()
        .filter(|line| line.starts_with("RUN pnpm "))
        .collect();
    assert_eq!(
        pnpm,
        [
            "RUN pnpm install --frozen-lockfile --filter \"telemetry-web-spa...\"",
            "RUN pnpm run build:web",
        ]
    );
}

/// The heading of the settings table of the web service in STACK.md §15.
const STACK_SETTINGS_HEADING: &str = "#### Railway settings of the service `telemetry-web`";

/// The row of the settings table of the web service that starts with
/// `name`. The search reads only the subsection of the web service, up to
/// the next heading, because the table of the ingest service uses the same
/// row names.
fn stack_row(name: &str) -> String {
    let stack = read("STACK.md");
    let headings = stack
        .lines()
        .filter(|line| *line == STACK_SETTINGS_HEADING)
        .count();
    assert_eq!(
        headings, 1,
        "STACK.md has no single heading {STACK_SETTINGS_HEADING:?}"
    );
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
fn the_settings_table_of_stack_md_holds_the_same_values() {
    for (name, value) in [
        ("Root directory", "`/`".to_owned()),
        ("Dockerfile path", format!("`{}`", railway::DOCKERFILE_PATH)),
        ("Custom start command", "None".to_owned()),
        ("Pre-deploy command", "None".to_owned()),
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
        ("TCP proxy", "None".to_owned()),
        ("Custom domain", "None".to_owned()),
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

/// The settings agree with the image and the code. The shutdown deadline
/// of the server ends before Railway sends SIGKILL at the end of the
/// draining time. The server ends with exit code 1 at the deadline
/// (`src/shutdown.rs`), not with the SIGKILL of Railway.
#[test]
fn the_railway_settings_match_the_image_and_the_code() {
    // The Dockerfile exists at the path, relative to the repository root.
    assert!(repository().join(&railway::DOCKERFILE_PATH[1..]).is_file());
    assert_eq!(railway::DOCKERFILE_PATH, format!("/{DOCKERFILE}"));
    assert!(SHUTDOWN_DEADLINE < Duration::from_secs(railway::DRAINING_SECONDS));
    assert_eq!(railway::OVERLAP_SECONDS, 0);
    // The server serves the healthcheck path with GET. The committed
    // contract is the export of the routes of the server, and the task
    // `contract-check` proves that it equals a new export.
    let contract: Value = serde_json::from_str(&read("contract/openapi.json")).unwrap();
    assert!(
        contract["paths"][railway::HEALTHCHECK_PATH]["get"].is_object(),
        "the contract has no GET {}",
        railway::HEALTHCHECK_PATH
    );
}

/// The build-context sources of `webapp/Dockerfile`, in the order of its
/// `COPY` lines.
fn context_inputs() -> Vec<String> {
    model_of(DOCKERFILE)
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

/// The model block of this file equals the block of `crates/ingest/tests/deployment.rs`.
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

/// No `COPY` source of `webapp/Dockerfile` is, holds, or sits below a symbolic link,
/// and the repository holds no symbolic link and no submodule
/// (`link_findings`).
#[test]
fn the_build_inputs_hold_no_symbolic_link_or_submodule() {
    let findings = link_findings(&repository(), &context_inputs(), &in_build_context);
    assert!(findings.is_empty(), "{findings:#?}");
}

/// True when the `COPY` input `input` reads the file at the relative
/// `path`: the input is the path itself or one of its parent directories.
fn input_reads(input: &str, path: &str) -> bool {
    path == input
        || path
            .strip_prefix(input)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True when a `COPY` line of the web image reads the file at `path`.
fn copied(path: &str) -> bool {
    context_inputs()
        .iter()
        .any(|input| input_reads(input, path))
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

/// True when a web watch path matches the file at the relative `path`.
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
    let mut expected: Vec<String> = [
        "package.json",
        "pnpm-lock.yaml",
        "pnpm-workspace.yaml",
        ".npmrc",
        "tsconfig.base.json",
        "packages",
        "web",
        "Cargo.toml",
        "Cargo.lock",
        ".cargo",
    ]
    .map(str::to_owned)
    .to_vec();
    expected.extend(BUILT_MEMBERS.map(str::to_owned));
    expected.extend(
        stub_members()
            .iter()
            .map(|member| format!("{member}/Cargo.toml")),
    );
    assert_eq!(inputs, expected);
    for input in &inputs {
        let path = repository().join(input);
        // A file of the input: the file itself, or a file in the directory.
        let file = if input == ".cargo" {
            ".cargo/config.toml".to_owned()
        } else if path.is_dir() {
            format!("{input}/any/file")
        } else {
            input.clone()
        };
        assert!(watched(&file), "no web watch path for {input}");
        assert!(
            in_build_context(&file),
            "no .dockerignore entry for {input}"
        );
    }
    // The Dockerfile, the toolchain pin, and the context rules also start a
    // deployment.
    for path in [DOCKERFILE, "rust-toolchain.toml", ".dockerignore"] {
        assert!(watched(path), "no web watch path for {path}");
    }
    assert!(watched("crates/server/.sqlx/query-any.json"));
    assert!(in_build_context("crates/server/.sqlx/query-any.json"));
}

/// The ingest files, the browser tests, the contract export, and the
/// documentation are not inputs of the web image: no web watch path
/// matches them, and no `COPY` line reads them. So a merge that changes
/// only these files does not redeploy the web service. The web watch paths
/// name no path of the ingest image other than the shared Cargo files.
#[test]
fn ingest_files_and_documents_are_not_web_image_inputs() {
    for path in [
        "/ingest/**",
        "/crates/ingest/**",
        "/db/**",
        "/migrations/**",
    ] {
        assert!(
            !railway::WATCH_PATHS.contains(&path),
            "the web watch paths name {path}"
        );
    }
    for path in [
        "ingest/Dockerfile",
        "ingest/entrypoint.sh",
        "ingest/server_config.json",
        "crates/ingest/src/main.rs",
        "crates/ingest/build.rs",
        "crates/ingest/.sqlx/query-any.json",
        "db/roles.sql",
        "migrations/0001_raw.sql",
        "e2e/tests/login.spec.ts",
        "e2e/package.json",
        "contract/openapi.json",
        "scripts/contract-check.sh",
        "scripts/image-check-web.sh",
        "docs/web-release.md",
        "STACK.md",
        "README.md",
        ".github/workflows/ci.yml",
    ] {
        assert!(!watched(path), "a web watch path matches {path}");
        assert!(!copied(path), "a COPY line of {DOCKERFILE} reads {path}");
    }
    // Control cases of the models: an ingest manifest is an input, and a
    // wider COPY input such as `crates` would read the ingest sources.
    assert!(watched("crates/ingest/Cargo.toml"));
    assert!(copied("crates/ingest/Cargo.toml"));
    assert!(copied("crates/server/src/main.rs"));
    assert!(copied("web/src/main.tsx"));
    assert!(input_reads("crates", "crates/ingest/src/main.rs"));
    // Secrets and build output of the SPA stay out of the build context.
    for path in [
        "web/.env.local",
        "web/node_modules/vite/package.json",
        "web/dist/index.html",
        "packages/api-client/node_modules/zod/package.json",
        "crates/server/target/x",
        "crates/server/secrets/key",
    ] {
        assert!(!in_build_context(path), "the build context holds {path}");
    }
}

/// The workspace members, as paths relative to the repository, from
/// `cargo metadata`.
fn workspace_members() -> BTreeSet<String> {
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
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
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

/// The image build copies `crates/server` and `crates/domain` in full and
/// makes a stub target for each other workspace member, so that Cargo can
/// load the workspace. A member that the list does not name would stop the
/// build (`--locked` cannot load the workspace).
#[test]
fn the_dockerfile_stubs_every_other_workspace_member() {
    let mut others = workspace_members();
    for member in BUILT_MEMBERS {
        assert!(others.remove(member), "{member} is not a workspace member");
    }
    let stubs = stub_members();
    assert_eq!(stubs.iter().cloned().collect::<BTreeSet<_>>(), others);
    assert_eq!(stubs.len(), others.len());
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
    // that the image does not have. Without its `build.rs`, the stub has no
    // build script.
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
