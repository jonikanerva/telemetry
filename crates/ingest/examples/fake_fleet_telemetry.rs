//! A fake `fleet-telemetry` for the end-to-end tests of `ingest run`. It is
//! not part of the ingest image.
//!
//! `ingest` starts it as its child with `-config <path>` and a cleared
//! environment, as it starts the real server. The configuration file is
//! JSON with four loopback ports:
//!
//! ```json
//! {"upstream_port": 4444, "zmq_port": 5284, "metrics_port": 9090, "control_port": 4445}
//! ```
//!
//! What it does, like the real server (ADR 0001):
//!
//! - Binds a ZMQ PUB socket on `tcp://127.0.0.1:<zmq_port>` and publishes
//!   each record as two frames: the topic `tesla_telemetry_<record type>`
//!   and the protobuf payload. The payloads are built with the types of
//!   the vendored schema (`ingest::decode::proto`).
//! - Listens on `127.0.0.1:<upstream_port>`, where the gate forwards the
//!   vehicle connections. It writes the line `ready` at once to each
//!   connection, so that the gate sees a first byte (gate rule 1).
//! - Serves `GET /metrics` on `127.0.0.1:<metrics_port>` in the Prometheus
//!   text format, with the counter `dispatch_total{record_type="..."}` for
//!   `V`, `alerts`, and `errors`, and a `vin_signal_count` series with the
//!   VIN as a label, as the real server has.
//!
//! Commands, one line each, on a vehicle connection through the gate or on
//! `127.0.0.1:<control_port>` directly. The answer is `ok` and the number of
//! published messages, or `error`.
//!
//! | Command | Effect |
//! | ------- | ------ |
//! | `publish V <first> <count>` | Publishes the `V` records `first` to `first + count - 1`; `dispatch_total` grows by `count` |
//! | `resend V <first> <count>` | The same records with `is_resend` set |
//! | `publish alerts <n>` / `publish errors <n>` | One record of the type, number `n` |
//! | `publish connectivity <n>` | One `connectivity` record; `dispatch_total` does not count it |
//! | `publish nul <n>` | One `V` record with a text value that holds a NUL byte, which PostgreSQL refuses |
//! | `junk <count> <bytes>` | `count` messages of topic `tesla_telemetry_V` with `bytes` bytes that do not decode |
//! | `trickle V <first> <count> <ms>` | Answers at once, then publishes the `V` records `first` to `first + count - 1`, one each `ms` milliseconds |
//! | `lose V <count>` | `dispatch_total` grows by `count`, nothing is published: a ZMQ drop |
//! | `exit <code>` | Exits with the code |
//!
//! Record `i` of type `V` has `created_at` 2026-11-01T00:00:00Z plus `i`
//! seconds, the battery level `50 + i / 100`, a location, and a text value.
//! The same `i` always gives the same record, so a second `publish` or a
//! `resend` is a duplicate. Every value is invented; the VIN
//! `0FAKEVXN000000001` is not a real VIN.
//!
//! The process ends when its parent process ends: on macOS no kernel rule
//! ends a child with its parent.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ingest::decode::proto::vehicle_alerts::{VehicleAlert, VehicleAlerts};
use ingest::decode::proto::vehicle_connectivity::VehicleConnectivity;
use ingest::decode::proto::vehicle_data::{Datum, Field, LocationValue, Payload, Value, value};
use ingest::decode::proto::vehicle_error::{VehicleError, VehicleErrors};
use prost::Message;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use zeromq::prelude::{Socket, SocketSend};
use zeromq::{PubSocket, ZmqMessage};

/// The invented VIN of every record.
pub const VIN: &str = "0FAKEVXN000000001";

/// 2026-11-01T00:00:00Z.
const BASE: i64 = 1_793_491_200;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default)]
struct Dispatched {
    vehicle_data: u64,
    alerts: u64,
    errors: u64,
}

struct Fake {
    socket: tokio::sync::Mutex<PubSocket>,
    dispatched: Mutex<Dispatched>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(flag), Some(path), None) = (args.next(), args.next(), args.next()) else {
        return Err("usage: fake_fleet_telemetry -config <path>".into());
    };
    if flag != "-config" {
        return Err("usage: fake_fleet_telemetry -config <path>".into());
    }
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let port = |name: &str| -> Result<u16> {
        config
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .ok_or_else(|| format!("missing port {name}").into())
    };
    let mut socket = PubSocket::new();
    socket
        .bind(&format!("tcp://127.0.0.1:{}", port("zmq_port")?))
        .await?;
    let fake = Arc::new(Fake {
        socket: tokio::sync::Mutex::new(socket),
        dispatched: Mutex::new(Dispatched::default()),
    });
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, port("upstream_port")?)).await?;
    let control = TcpListener::bind((Ipv4Addr::LOCALHOST, port("control_port")?)).await?;
    let metrics = TcpListener::bind((Ipv4Addr::LOCALHOST, port("metrics_port")?)).await?;
    tokio::spawn(accept(upstream, Arc::clone(&fake), true));
    tokio::spawn(accept(control, Arc::clone(&fake), false));
    tokio::spawn(serve_metrics(metrics, Arc::clone(&fake)));

    let parent = std::os::unix::process::parent_id();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if std::os::unix::process::parent_id() != parent {
            return Ok(());
        }
    }
}

async fn accept(listener: TcpListener, fake: Arc<Fake>, banner: bool) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve_commands(stream, Arc::clone(&fake), banner));
    }
}

async fn serve_commands(stream: TcpStream, fake: Arc<Fake>, banner: bool) -> Result<()> {
    let (read, mut write) = stream.into_split();
    if banner {
        write.write_all(b"ready\n").await?;
    }
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let answer = match Arc::clone(&fake).command(&line).await {
            Ok(published) => format!("ok {published}\n"),
            Err(_) => "error\n".to_owned(),
        };
        write.write_all(answer.as_bytes()).await?;
    }
    Ok(())
}

impl Fake {
    fn dispatched(&self) -> std::sync::MutexGuard<'_, Dispatched> {
        self.dispatched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    async fn command(self: Arc<Self>, line: &str) -> Result<u64> {
        let words: Vec<&str> = line.split_ascii_whitespace().collect();
        let number = |index: usize| -> Result<u64> {
            Ok(words.get(index).ok_or("missing number")?.parse()?)
        };
        match words.as_slice() {
            ["publish" | "resend", "V", ..] => {
                let resend = words.first() == Some(&"resend");
                let (first, count) = (number(2)?, number(3)?);
                for index in first..first + count {
                    self.publish("V", vehicle_data(index, resend)?.encode_to_vec())
                        .await?;
                    self.dispatched().vehicle_data += 1;
                }
                Ok(count)
            }
            ["publish", "nul", _] => {
                self.publish("V", text_with_nul(number(2)?)?.encode_to_vec())
                    .await?;
                self.dispatched().vehicle_data += 1;
                Ok(1)
            }
            ["publish", "alerts", _] => {
                self.publish("alerts", alerts(number(2)?)?.encode_to_vec())
                    .await?;
                self.dispatched().alerts += 1;
                Ok(1)
            }
            ["publish", "errors", _] => {
                self.publish("errors", errors(number(2)?)?.encode_to_vec())
                    .await?;
                self.dispatched().errors += 1;
                Ok(1)
            }
            ["publish", "connectivity", _] => {
                self.publish("connectivity", connectivity(number(2)?)?.encode_to_vec())
                    .await?;
                Ok(1)
            }
            ["junk", _, _] => {
                let (count, bytes) = (number(1)?, usize::try_from(number(2)?)?);
                for index in 0..count {
                    // Printable bytes that are not a protobuf message: the
                    // first byte 0x4a gives field 9 with wire type 2, and
                    // the length then runs past the end.
                    let mut payload = format!("JUNKPAYLOAD{index:08}").into_bytes();
                    payload.resize(bytes.max(payload.len()), b'J');
                    self.publish("V", payload).await?;
                    self.dispatched().vehicle_data += 1;
                }
                Ok(count)
            }
            ["trickle", "V", _, _, _] => {
                let (first, count) = (number(2)?, number(3)?);
                let interval = Duration::from_millis(number(4)?);
                let fake = Arc::clone(&self);
                tokio::spawn(async move {
                    for index in first..first + count {
                        tokio::time::sleep(interval).await;
                        let Ok(payload) = vehicle_data(index, false) else {
                            return;
                        };
                        if fake.publish("V", payload.encode_to_vec()).await.is_err() {
                            return;
                        }
                        fake.dispatched().vehicle_data += 1;
                    }
                });
                Ok(count)
            }
            ["lose", "V", _] => {
                self.dispatched().vehicle_data += number(2)?;
                Ok(0)
            }
            ["exit", _] => std::process::exit(i32::try_from(number(1)?)?),
            _ => Err("unknown command".into()),
        }
    }

    async fn publish(&self, record_type: &str, payload: Vec<u8>) -> Result<()> {
        let mut message = ZmqMessage::from(format!("tesla_telemetry_{record_type}").into_bytes());
        let frame = ZmqMessage::from(payload)
            .into_vec()
            .into_iter()
            .next()
            .ok_or("no frame")?;
        message.push_back(frame);
        self.socket.lock().await.send(message).await?;
        Ok(())
    }

    fn metrics(&self) -> String {
        let dispatched = self.dispatched();
        let mut text = String::from(
            "# HELP dispatch_total The number of records dispatched.\n\
             # TYPE dispatch_total counter\n",
        );
        for (name, count) in [
            ("V", dispatched.vehicle_data),
            ("alerts", dispatched.alerts),
            ("errors", dispatched.errors),
        ] {
            // A counter vector shows only the label values that grew.
            if count > 0 {
                writeln!(
                    text,
                    "dispatch_total{{record_type=\"{name}\"}} {}",
                    go_float(count)
                )
                .ok();
            }
        }
        writeln!(
            text,
            "# TYPE vin_signal_count gauge\nvin_signal_count{{record_type=\"V\",vin=\"{VIN}\"}} {}",
            dispatched.vehicle_data
        )
        .ok();
        text
    }
}

/// A count in the Go `'g'` float format of the Prometheus text writer:
/// `123456`, but `1e+06` and `1.234567e+06` from one million.
fn go_float(count: u64) -> String {
    if count < 1_000_000 {
        return count.to_string();
    }
    let digits = count.to_string();
    let exponent = digits.len() - 1;
    let mantissa = digits.trim_end_matches('0');
    let (first, rest) = mantissa.split_at(1);
    if rest.is_empty() {
        format!("{first}e+{exponent:02}")
    } else {
        format!("{first}.{rest}e+{exponent:02}")
    }
}

async fn serve_metrics(listener: TcpListener, fake: Arc<Fake>) {
    while let Ok((stream, _)) = listener.accept().await {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move { answer_metrics(stream, &fake).await });
    }
}

async fn answer_metrics(mut stream: TcpStream, fake: &Fake) -> Result<()> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await?;
        if read == 0 || request.len() > 16 * 1024 {
            return Ok(());
        }
        request.extend_from_slice(buffer.get(..read).ok_or("read")?);
    }
    let body = fake.metrics();
    respond(&mut stream, &body).await
}

async fn respond(stream: &mut (impl AsyncWrite + Unpin), body: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn timestamp(seconds: i64) -> prost_types::Timestamp {
    prost_types::Timestamp { seconds, nanos: 0 }
}

fn seconds(index: u64) -> Result<i64> {
    Ok(BASE + i64::try_from(index)?)
}

fn datum(field: Field, value: value::Value) -> Datum {
    Datum {
        key: field as i32,
        value: Some(Value { value: Some(value) }),
    }
}

fn vehicle_data(index: u64, resend: bool) -> Result<Payload> {
    let hundredths = u32::try_from(index % 10_000)?;
    Ok(Payload {
        data: vec![
            datum(
                Field::BatteryLevel,
                value::Value::DoubleValue(50.0 + f64::from(hundredths) / 100.0),
            ),
            datum(
                Field::Location,
                value::Value::LocationValue(LocationValue {
                    latitude: 60.169_912_3,
                    longitude: 24.938_445_6,
                }),
            ),
            datum(
                Field::VehicleName,
                value::Value::StringValue("SENTINEL-TEXT-VALUE".to_owned()),
            ),
        ],
        created_at: Some(timestamp(seconds(index)?)),
        vin: VIN.to_owned(),
        is_resend: resend,
    })
}

/// A `V` record with a text value that holds a NUL byte, which PostgreSQL
/// refuses in `text` (SQLSTATE 22021). Its time is one day after record
/// `index`, so that its keys differ from the other records.
fn text_with_nul(index: u64) -> Result<Payload> {
    Ok(Payload {
        data: vec![datum(
            Field::VehicleName,
            value::Value::StringValue("SENTINEL\0NUL".to_owned()),
        )],
        created_at: Some(timestamp(seconds(index)? + 86_400)),
        vin: VIN.to_owned(),
        is_resend: false,
    })
}

fn alerts(index: u64) -> Result<VehicleAlerts> {
    Ok(VehicleAlerts {
        alerts: vec![VehicleAlert {
            name: format!("SENTINEL_ALERT_{index}"),
            audiences: vec![1],
            started_at: Some(timestamp(seconds(index)?)),
            ended_at: None,
        }],
        created_at: Some(timestamp(seconds(index)?)),
        vin: VIN.to_owned(),
    })
}

fn errors(index: u64) -> Result<VehicleErrors> {
    Ok(VehicleErrors {
        errors: vec![VehicleError {
            created_at: Some(timestamp(seconds(index)?)),
            name: format!("SENTINEL_ERROR_{index}"),
            tags: BTreeMap::from([("SENTINEL_TAG".to_owned(), "SENTINEL_TAG_VALUE".to_owned())])
                .into_iter()
                .collect(),
            body: "SENTINEL-ERROR-BODY".to_owned(),
        }],
        created_at: Some(timestamp(seconds(index)?)),
        vin: VIN.to_owned(),
    })
}

fn connectivity(index: u64) -> Result<VehicleConnectivity> {
    Ok(VehicleConnectivity {
        vin: VIN.to_owned(),
        connection_id: format!("00000000-0000-4000-8000-{index:012}"),
        status: 1,
        created_at: Some(timestamp(seconds(index)?)),
        network_interface: "wifi".to_owned(),
    })
}
