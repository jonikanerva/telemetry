//! The read of `dispatch_total` from the Prometheus metrics of
//! `fleet-telemetry` (ADR 0001, "ZMQ shortfall check").
//!
//! The metric, from the upstream source at tag `v0.10.1` (commit
//! `6689adb`):
//!
//! - `server/streaming/socket.go`, lines 378-382: the counter has the
//!   name `dispatch_total` and the one label `record_type`. Lines 288-291:
//!   it grows by one after `record.Dispatch()` returns, with the record
//!   type (`TxType`) as the label value, for example `V`, `alerts`, or
//!   `errors`.
//! - `metrics/adapter/prometheus/prometheus.go`, lines 27-42: the counter
//!   is a `prometheus.CounterVec` with the name as given, without a
//!   namespace or a subsystem.
//! - `server/monitoring/metrics_server.go`, lines 34-46: the handler is
//!   `promhttp.Handler()` on `/metrics`, on `prometheus_metrics_host`
//!   (default `127.0.0.1`) and `prometheus_metrics_port`.
//!
//! The text format: `promhttp` writes each series as
//! `dispatch_total{record_type="V"} 42`. The value is a float in the Go
//! `'g'` format (`prometheus/common` v0.42.0, `expfmt/text_create.go`,
//! `writeFloat`), so a count of one million is `1e+06`. A series that
//! never grew is absent; its count is 0.
//!
//! The response holds other metrics too, and one of them has the VIN as a
//! label (`vin_signal_count`, `server/streaming/socket.go`, line 238). The
//! parser reads only the `dispatch_total` lines, and nothing of the body
//! reaches a log or an error.
//!
//! Bounds: one TCP connection to the loopback interface, one request, a
//! body of at most [`super::Limits::metrics_body_bytes`], and one deadline
//! for the connect, the request, and the whole body.

use std::net::Ipv4Addr;
use std::time::Duration;

use axum::body::Body;
use hyper::header::HOST;
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

/// The values of `dispatch_total` for the three record types of the check.
/// `connectivity` records are outside the check: the server dispatches them
/// directly, and the counter does not count them (ADR 0001).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dispatched {
    /// `record_type="V"`.
    pub vehicle_data: u64,
    /// `record_type="alerts"`.
    pub alerts: u64,
    /// `record_type="errors"`.
    pub errors: u64,
}

/// A failed read. The text is static.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// No TCP connection to the metrics port.
    #[error("connect")]
    Connect,
    /// The HTTP exchange failed.
    #[error("http")]
    Http,
    /// The status was not 200.
    #[error("status")]
    Status,
    /// The body was larger than the bound, or broke.
    #[error("body")]
    Body,
    /// A `dispatch_total` line of a known record type does not parse.
    #[error("parse")]
    Parse,
    /// The read did not end within its deadline.
    #[error("timeout")]
    Timeout,
}

impl ReadError {
    /// Static text for a log event.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Http => "http",
            Self::Status => "status",
            Self::Body => "body",
            Self::Parse => "parse",
            Self::Timeout => "timeout",
        }
    }
}

/// Reads `dispatch_total` from `http://127.0.0.1:<port>/metrics` within
/// `deadline`.
///
/// # Errors
///
/// Returns a [`ReadError`] when the read fails or does not end in time.
pub async fn read(port: u16, max_body: usize, deadline: Duration) -> Result<Dispatched, ReadError> {
    let body = tokio::time::timeout(deadline, fetch(port, max_body))
        .await
        .unwrap_or(Err(ReadError::Timeout))?;
    parse(&body)
}

/// A task handle that aborts its task when it is dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn fetch(port: u16, max_body: usize) -> Result<Vec<u8>, ReadError> {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| ReadError::Connect)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| ReadError::Http)?;
    // The connection task drives the socket until the body is read. The
    // guard ends it with this function, also at the deadline.
    let _connection = AbortOnDrop(tokio::spawn(async move {
        connection.await.ok();
    }));
    let request = Request::get("/metrics")
        .header(HOST, "127.0.0.1")
        .body(Body::empty())
        .map_err(|_| ReadError::Http)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| ReadError::Http)?;
    if response.status() != StatusCode::OK {
        return Err(ReadError::Status);
    }
    let body = axum::body::to_bytes(Body::new(response.into_body()), max_body)
        .await
        .map_err(|_| ReadError::Body)?;
    Ok(body.to_vec())
}

/// The record types of the check and their label values.
const RECORD_TYPES: [&[u8]; 3] = [b"V", b"alerts", b"errors"];

/// Reads the `dispatch_total` series of the three record types from a body
/// in the Prometheus text format (module documentation).
///
/// # Errors
///
/// Returns [`ReadError::Parse`] when a series of a known record type has a
/// value that is not a whole count, or appears twice.
pub fn parse(body: &[u8]) -> Result<Dispatched, ReadError> {
    let mut counts: [Option<u64>; 3] = [None; 3];
    for line in body.split(|byte| *byte == b'\n') {
        let Some(rest) = line.strip_prefix(b"dispatch_total{") else {
            continue;
        };
        let Some(end) = rest.iter().position(|byte| *byte == b'}') else {
            return Err(ReadError::Parse);
        };
        let (labels, value) = rest.split_at(end);
        let Some(index) = record_type(labels)
            .and_then(|name| RECORD_TYPES.iter().position(|known| *known == name))
        else {
            continue;
        };
        let count = count(value.get(1..).unwrap_or_default())?;
        let slot = counts.get_mut(index).ok_or(ReadError::Parse)?;
        if slot.replace(count).is_some() {
            return Err(ReadError::Parse);
        }
    }
    let [vehicle_data, alerts, errors] = counts.map(|count| count.unwrap_or(0));
    Ok(Dispatched {
        vehicle_data,
        alerts,
        errors,
    })
}

/// The value of the label `record_type` in `name="value",...`.
fn record_type(labels: &[u8]) -> Option<&[u8]> {
    let start = labels
        .windows(b"record_type=\"".len())
        .position(|window| window == b"record_type=\"")?;
    let preceded = start == 0 || labels.get(start - 1) == Some(&b',');
    if !preceded {
        return None;
    }
    let value = labels.get(start + b"record_type=\"".len()..)?;
    let end = value.iter().position(|byte| *byte == b'"')?;
    value.get(..end)
}

/// The largest count that a float holds exactly: 2^53.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

/// A whole, non-negative count in the Go `'g'` float format, followed by an
/// optional timestamp.
fn count(value: &[u8]) -> Result<u64, ReadError> {
    let text = std::str::from_utf8(value).map_err(|_| ReadError::Parse)?;
    let number: f64 = text
        .split_ascii_whitespace()
        .next()
        .and_then(|token| token.parse().ok())
        .ok_or(ReadError::Parse)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > MAX_EXACT {
        return Err(ReadError::Parse);
    }
    // The checks above make the conversion exact.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is a whole number from 0 to 2^53"
    )]
    Ok(number as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "\
# HELP dispatch_total The number of records dispatched.
# TYPE dispatch_total counter
dispatch_total{record_type=\"V\"} 1e+06
dispatch_total{record_type=\"alerts\"} 3
dispatch_total{record_type=\"connectivity\"} 99
# HELP vin_signal_count The number signals per VIN.
vin_signal_count{record_type=\"V\",vin=\"0FAKEVXN000000001\"} 12
dispatch_totalx{record_type=\"errors\"} 5
";

    #[test]
    fn parses_the_three_record_types_and_an_absent_series_is_zero() {
        assert_eq!(
            parse(BODY.as_bytes()),
            Ok(Dispatched {
                vehicle_data: 1_000_000,
                alerts: 3,
                errors: 0,
            })
        );
        assert_eq!(parse(b""), Ok(Dispatched::default()));
    }

    #[test]
    fn reads_the_go_float_format() {
        for (text, expected) in [
            ("0", 0),
            ("1", 1),
            ("123456", 123_456),
            ("1.234567e+06", 1_234_567),
            ("9.007199254740992e+15", 9_007_199_254_740_992),
            ("42 1764160000000", 42),
        ] {
            assert_eq!(count(format!(" {text}").as_bytes()), Ok(expected), "{text}");
        }
        for text in ["NaN", "+Inf", "-1", "1.5", "1e+16", "", "x"] {
            assert_eq!(
                count(format!(" {text}").as_bytes()),
                Err(ReadError::Parse),
                "{text}"
            );
        }
    }

    #[test]
    fn a_broken_or_repeated_series_is_a_parse_error() {
        assert_eq!(
            parse(b"dispatch_total{record_type=\"V\"} 1.5\n"),
            Err(ReadError::Parse)
        );
        assert_eq!(
            parse(b"dispatch_total{record_type=\"V\" 1\n"),
            Err(ReadError::Parse)
        );
        assert_eq!(
            parse(b"dispatch_total{record_type=\"V\"} 1\ndispatch_total{record_type=\"V\"} 2\n"),
            Err(ReadError::Parse)
        );
        // Another label before the record type.
        assert_eq!(
            parse(b"dispatch_total{a=\"b\",record_type=\"errors\"} 7\n"),
            Ok(Dispatched {
                errors: 7,
                ..Dispatched::default()
            })
        );
        // A label whose name only ends in `record_type`.
        assert_eq!(
            parse(b"dispatch_total{xrecord_type=\"V\"} 7\n"),
            Ok(Dispatched::default())
        );
    }

    #[test]
    fn the_error_text_is_static() {
        assert_eq!(ReadError::Parse.to_string(), "parse");
        assert_eq!(ReadError::Timeout.as_str(), "timeout");
    }

    async fn serve_once(response: impl AsRef<[u8]> + Send + 'static) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 1024];
            let read = stream.read(&mut request).await.unwrap();
            assert!(request[..read].starts_with(b"GET /metrics HTTP/1.1\r\n"));
            stream.write_all(response.as_ref()).await.unwrap();
            // Keep the connection open: the client must stop by itself.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        port
    }

    #[tokio::test]
    async fn reads_a_response_from_the_loopback_port() {
        let port = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 35\r\n\r\ndispatch_total{record_type=\"V\"} 12\n",
        )
        .await;
        let read = read(port, 1024, Duration::from_secs(5)).await;
        assert_eq!(
            read,
            Ok(Dispatched {
                vehicle_data: 12,
                ..Dispatched::default()
            })
        );
    }

    #[tokio::test]
    async fn a_large_body_a_bad_status_and_a_silent_server_fail() {
        let mut large = b"HTTP/1.1 200 OK\r\nContent-Length: 2000\r\n\r\n".to_vec();
        large.extend_from_slice(&[b'x'; 2000]);
        let port = serve_once(large).await;
        assert_eq!(
            read(port, 1024, Duration::from_secs(5)).await,
            Err(ReadError::Body)
        );
        let port =
            serve_once(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
        assert_eq!(
            read(port, 1024, Duration::from_secs(5)).await,
            Err(ReadError::Status)
        );
        // A response head without the body: the deadline ends the read.
        let port = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n").await;
        assert_eq!(
            read(port, 1024, Duration::from_millis(200)).await,
            Err(ReadError::Timeout)
        );
    }

    #[tokio::test]
    async fn a_closed_port_fails_to_connect() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            read(port, 1024, Duration::from_secs(5)).await,
            Err(ReadError::Connect)
        );
    }
}
