//! Structured JSON logs with an allowlist of fields.
//!
//! Each event becomes one JSON object on one line of standard output. The
//! object has the keys `timestamp`, `level`, and `target`, and one key for
//! each event field. The formatter writes the value of a field only when the
//! field name is in the allowlist. For every other field it writes
//! `[redacted]`. The formatter writes no span fields. A later change that
//! needs span context must add the span fields through the same allowlist.
//!
//! `VISION.md` forbids the VIN, the location, and signal values in logs.
//! Add a field name to [`ALLOWED_FIELDS`] only when every value of the field
//! is safe by construction, for example a count, a port, or an error type
//! whose text holds only static words.
//!
//! The `message` field is the event name. Keep it a constant text, for
//! example `info!(port, "listening")`. The formatter cannot see whether the
//! text came from format arguments, so code review must keep values out of
//! the message.
//!
//! The `error` field writes the `Display` text of an error. Today each
//! error in this crate holds static words or operating system text only.
//! The text of a database error or of a library error can contain row
//! values, a URL, or a credential. Do not pass such an error to the
//! `error` field. Map it to a static code first, for example the SQLSTATE
//! class, as ADR 0001 requires for the database writer.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::subscriber::SetGlobalDefaultError;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::registry::LookupSpan;

/// Field names whose values the logs may contain.
///
/// - `kind` and `reason`: the `ingest_event` kind and its reason. Both hold
///   static words and numbers only ([`crate::events`]).
/// - `count`: the count of an `ingest_event`.
/// - `task`: the static name of a task that the watchdog watches.
/// - `duration_ms` and `first_byte_ms`: durations of a gate connection.
/// - `peer_class`: a static class of a peer address, never the address
///   ([`crate::gate`]).
/// - `socket_options` and `socket_options_error`: a static outcome of the
///   socket options of a gate connection, and the `io::ErrorKind` name.
/// - `child_line`: a line of the child after the VIN redaction. Only the
///   text of [`crate::child_output::RedactedLine`] goes into this field.
/// - `stream` and `truncated`: the output stream of that line, and whether
///   the line was cut at 8 KiB.
/// - `pid`, `exit_code`, `exit_signal`, and `delay_ms`: numbers of the
///   child supervision.
pub const ALLOWED_FIELDS: &[&str] = &[
    "child_line",
    "connections",
    "count",
    "delay_ms",
    "duration_ms",
    "error",
    "exit_code",
    "exit_signal",
    "first_byte_ms",
    "kind",
    "message",
    "peer_class",
    "pid",
    "port",
    "reason",
    "signal",
    "socket_options",
    "socket_options_error",
    "stream",
    "task",
    "truncated",
];

/// Keys that the formatter writes itself. An event field cannot replace them.
pub const RESERVED_KEYS: &[&str] = &["level", "target", "timestamp"];

/// The text that replaces the value of a field outside the allowlist.
pub const REDACTED: &str = "[redacted]";

/// Installs the global JSON logger at level INFO on standard output.
///
/// # Errors
///
/// Returns an error when a global logger already exists.
pub fn init() -> Result<(), SetGlobalDefaultError> {
    let subscriber = tracing_subscriber::fmt()
        .event_format(JsonAllowlist::new(ALLOWED_FIELDS))
        .with_max_level(LevelFilter::INFO)
        .with_writer(std::io::stdout)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
}

/// Event formatter that writes one JSON object for each event and applies
/// an allowlist to the event fields.
#[derive(Debug, Clone, Copy)]
pub struct JsonAllowlist {
    allowed: &'static [&'static str],
}

impl JsonAllowlist {
    /// Makes a formatter that writes the values of the `allowed` fields only.
    #[must_use]
    pub const fn new(allowed: &'static [&'static str]) -> Self {
        Self { allowed }
    }
}

impl<S, N> FormatEvent<S, N> for JsonAllowlist
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut object = Map::new();
        event.record(&mut AllowlistVisitor {
            allowed: self.allowed,
            object: &mut object,
        });

        let mut timestamp = String::new();
        SystemTime.format_time(&mut Writer::new(&mut timestamp))?;
        let metadata = event.metadata();
        // Insert the reserved keys last, so that an event field cannot
        // replace them.
        object.insert("timestamp".to_owned(), Value::from(timestamp));
        object.insert("level".to_owned(), Value::from(metadata.level().as_str()));
        object.insert("target".to_owned(), Value::from(metadata.target()));

        let line = serde_json::to_string(&object).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

struct AllowlistVisitor<'a> {
    allowed: &'static [&'static str],
    object: &'a mut Map<String, Value>,
}

impl AllowlistVisitor<'_> {
    fn insert(&mut self, field: &Field, value: impl FnOnce() -> Value) {
        let value = if self.allowed.contains(&field.name()) {
            value()
        } else {
            Value::from(REDACTED)
        };
        self.object.insert(field.name().to_owned(), value);
    }
}

impl Visit for AllowlistVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, || Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, || Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, || Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, || Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, || Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, || Value::from(format!("{value:?}")));
    }
}

/// Log capture for tests in this crate.
#[cfg(test)]
pub(crate) mod capture {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing::subscriber::DefaultGuard;

    use super::{JsonAllowlist, LevelFilter};

    /// The output of a test logger.
    #[derive(Clone, Default)]
    pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub(crate) fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Installs a logger with the `allowed` fields on this thread until the
    /// guard drops.
    pub(crate) fn install(allowed: &'static [&'static str]) -> (Capture, DefaultGuard) {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .event_format(JsonAllowlist::new(allowed))
            .with_max_level(LevelFilter::INFO)
            .with_writer(move || writer.clone())
            .finish();
        (capture, tracing::subscriber::set_default(subscriber))
    }
}

#[cfg(test)]
mod tests {
    use tracing::{info, info_span};

    use super::*;
    use crate::config::SecretString;

    /// Runs `emit` with a logger that uses `allowed`, and returns the text
    /// and the parsed JSON objects of the output.
    fn capture(allowed: &'static [&'static str], emit: impl FnOnce()) -> (String, Vec<Value>) {
        let (capture, guard) = capture::install(allowed);
        emit();
        drop(guard);
        let text = capture.text();
        let objects = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (text, objects)
    }

    #[test]
    fn writes_one_json_object_with_allowed_fields() {
        let (_, objects) = capture(ALLOWED_FIELDS, || info!(port = 8080_u16, "listening"));
        assert_eq!(objects.len(), 1);
        let event = &objects[0];
        assert_eq!(event["message"], "listening");
        assert_eq!(event["port"], 8080);
        assert_eq!(event["level"], "INFO");
        assert_eq!(event["target"], "ingest::logging::tests");
        assert!(event["timestamp"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn redacts_a_field_outside_the_allowlist() {
        let vin = "5YJ3E1EA7KF000316";
        let (text, objects) = capture(ALLOWED_FIELDS, || {
            info!(vin, latitude = 60.17_f64, "record_received");
        });
        assert!(!text.contains(vin), "{text}");
        assert!(!text.contains("60.17"), "{text}");
        assert_eq!(objects[0]["vin"], REDACTED);
        assert_eq!(objects[0]["latitude"], REDACTED);
        assert_eq!(objects[0]["message"], "record_received");
    }

    #[test]
    fn redacts_a_secret_in_an_allowed_field() {
        static ALLOWED: &[&str] = &["message", "database_url"];
        let secret = SecretString::new("postgres://u:hunter2@db.invalid/x".to_owned());
        let (text, objects) = capture(ALLOWED, || {
            info!(database_url = ?secret, "connecting");
        });
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("db.invalid"), "{text}");
        assert_eq!(objects[0]["database_url"], REDACTED);
    }

    #[test]
    fn writes_no_span_fields() {
        let vin = "5YJ3E1EA7KF000316";
        let (text, objects) = capture(ALLOWED_FIELDS, || {
            let span = info_span!("connection", vin, port = 4443_u16);
            let _entered = span.enter();
            info!("connection_opened");
        });
        assert!(!text.contains(vin), "{text}");
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].get("vin"), None);
        assert_eq!(objects[0].get("port"), None);
    }

    #[test]
    fn an_event_field_cannot_replace_a_reserved_key() {
        static ALLOWED: &[&str] = &["message", "level", "target", "timestamp"];
        let (_, objects) = capture(ALLOWED, || {
            info!(level = "x", target = "x", timestamp = "x", "event");
        });
        assert_eq!(objects[0]["level"], "INFO");
        assert_eq!(objects[0]["target"], "ingest::logging::tests");
        assert_ne!(objects[0]["timestamp"], "x");
        for key in RESERVED_KEYS {
            assert!(!ALLOWED_FIELDS.contains(key), "{key} is reserved");
        }
    }

    #[test]
    fn drops_events_below_info() {
        let (text, _) = capture(ALLOWED_FIELDS, || tracing::debug!("detail"));
        assert_eq!(text, "");
    }
}
