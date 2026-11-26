//! End-to-end tests of the delivery path without a database: the real
//! `ingest` binary with the fake `fleet-telemetry` of
//! `examples/fake_fleet_telemetry.rs` (`tests/harness/mod.rs`).
//!
//! No database answers: port 1 of the loopback interface refuses each
//! connection. So the start-up check gets no answer, and the shortened
//! limit of degraded mode opens the gate. These tests run in CI, on Linux;
//! the tests with a database are in `tests/storage/pipeline.rs`.
//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! Clippy, so that they may use `unwrap` and `panic` like the tests do.
#![cfg(test)]

mod harness;

use std::time::Duration;

use harness::{Ingest, VEHICLE_SENTINELS};
use serde_json::Value;

/// A database URL that no server answers. The password and the user name
/// are sentinels for the log check.
const NO_DATABASE: &str = "postgresql://telemetry_ingest:hunter2@127.0.0.1:1/neondb";

/// The exit code of a final flush that did not store everything.
const FLUSH_INCOMPLETE: i32 = 1;

fn start(extra: &[(&str, &str)]) -> Ingest {
    let mut vars = vec![
        ("INGEST_TEST_DEGRADED_AFTER_MS", "300".to_owned()),
        ("INGEST_TEST_FLUSH_DEADLINE_MS", "500".to_owned()),
        ("INGEST_TEST_RETRY_MAX_MS", "200".to_owned()),
    ];
    for (name, value) in extra {
        vars.retain(|(existing, _)| existing != name);
        vars.push((name, (*value).to_owned()));
    }
    Ingest::start(NO_DATABASE, &vars)
}

fn count(event: &Value, field: &str) -> u64 {
    event[field].as_u64().unwrap()
}

#[test]
fn records_pass_the_gate_in_degraded_mode_and_a_failed_final_flush_reports_them() {
    let mut ingest = start(&[]);
    ingest.read_event("degraded_mode");
    let mut vehicle = ingest.wait_for_vehicle();
    // Degraded mode opens the gate, but `/health` fails (ADR 0001).
    ingest.wait_for_health(503);
    assert_eq!(vehicle.command("publish V 0 20"), "ok 20");
    assert_eq!(vehicle.command("resend V 0 5"), "ok 5");
    for command in [
        "publish alerts 1",
        "publish errors 1",
        "publish connectivity 1",
    ] {
        assert_eq!(vehicle.command(command), "ok 1");
    }

    ingest.signal("TERM");
    let status = ingest.wait();
    assert_eq!(
        status.code(),
        Some(FLUSH_INCOMPLETE),
        "{:?}",
        ingest.messages()
    );
    // The drain read every message that was in flight: all 28 records wait
    // in the queue, and the final flush reports them as lost.
    let lost = ingest.with_message("flush_incomplete")[0].clone();
    assert_eq!(count(&lost, "records"), 28);
    assert_eq!(count(&lost, "signal_rows"), 25 * 3);
    let drained = ingest.with_message("subscriber_drained")[0].clone();
    assert!(count(&drained, "count") <= 28);
    // Nothing was lost on the ZMQ path.
    assert_eq!(
        count(ingest.with_message("shortfall_final_read")[0], "count"),
        0
    );
    let kinds = ingest.kinds();
    for kind in [
        "started",
        "degraded_mode",
        "gate_opened",
        "gate_closed",
        "stopped",
    ] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    // The complete log of the run.
    let mut secrets = VEHICLE_SENTINELS.to_vec();
    secrets.extend([
        "hunter2",
        "telemetry_ingest",
        "postgresql://",
        "127.0.0.1:1/",
    ]);
    ingest.assert_log_holds_none_of(&secrets);
}

#[test]
fn a_full_queue_drops_the_newest_messages_counts_them_and_closes_the_gate() {
    let mut ingest = start(&[("INGEST_TEST_QUEUE_BYTES", "65536")]);
    ingest.read_event("degraded_mode");
    let mut vehicle = ingest.wait_for_vehicle();
    // About 54 messages of 1 KiB fit into 64 KiB. The rest is dropped.
    assert_eq!(ingest.control("junk 100 1024"), "ok 100");
    let closed = ingest.read_until(
        |event| event["message"] == "ingest_event" && event["reason"] == "queue_high_mark",
        "the gate closed at the high mark",
    );
    assert_eq!(closed["kind"], "gate_closed");
    ingest.poll();
    let first = ingest.read_event("records_dropped");
    assert_eq!(first["reason"], "queue_full");
    // The closed gate ends the forwarded connection.
    assert!(vehicle.is_closed());
    assert!(ingest.vehicle().is_none());

    ingest.signal("TERM");
    assert_eq!(ingest.wait().code(), Some(FLUSH_INCOMPLETE));
    let dropped: u64 = ingest
        .with_message("ingest_event")
        .iter()
        .filter(|event| event["kind"] == "records_dropped")
        .map(|event| count(event, "count"))
        .sum();
    let queued = count(ingest.with_message("flush_incomplete")[0], "records");
    assert!(
        dropped > 0 && queued > 0,
        "dropped {dropped}, queued {queued}"
    );
    // Every message is either queued or counted as dropped.
    assert_eq!(dropped + queued, 100);
    // The dropped messages were received, so they are no shortfall.
    assert_eq!(
        count(ingest.with_message("shortfall_final_read")[0], "count"),
        0
    );
    ingest.assert_log_holds_none_of(VEHICLE_SENTINELS);
}

#[test]
fn the_shutdown_drains_the_messages_that_arrive_after_the_gate_closed() {
    let mut ingest = start(&[]);
    ingest.read_until(|event| event["message"] == "subscriber_settled", "settled");
    // 40 records, one each 20 ms, for about 0.8 s after the signal: the
    // records that `fleet-telemetry` acked before the gate closed and
    // dispatches late. The drain reads until no message arrives for 1 s.
    assert_eq!(ingest.control("trickle V 0 40 20"), "ok 40");
    ingest.signal("TERM");
    assert_eq!(ingest.wait().code(), Some(FLUSH_INCOMPLETE));
    let drained = count(ingest.with_message("subscriber_drained")[0], "count");
    assert!(drained > 0, "no message arrived during the drain");
    assert_eq!(
        count(ingest.with_message("flush_incomplete")[0], "records"),
        40
    );
}

#[test]
fn a_shortfall_is_reported_once_after_two_reads() {
    let mut ingest = start(&[("INGEST_TEST_SHORTFALL_PERIOD_MS", "200")]);
    ingest.read_until(|event| event["message"] == "subscriber_settled", "settled");
    assert_eq!(ingest.control("publish V 0 5"), "ok 5");
    // The counter grows by 3 without messages: a drop on the ZMQ path.
    assert_eq!(ingest.control("lose V 3"), "ok 0");
    let shortfall = ingest.read_event("zmq_shortfall");
    assert_eq!(count(&shortfall, "count"), 3);
    // Ten more periods report nothing new.
    std::thread::sleep(Duration::from_secs(2));
    ingest.signal("TERM");
    assert_eq!(ingest.wait().code(), Some(FLUSH_INCOMPLETE));
    let reported: Vec<_> = ingest
        .with_message("ingest_event")
        .into_iter()
        .filter(|event| event["kind"] == "zmq_shortfall")
        .collect();
    assert_eq!(reported.len(), 1, "{:?}", ingest.messages());
    // The final read sees the same gap and has nothing new to report.
    assert_eq!(
        count(ingest.with_message("shortfall_final_read")[0], "count"),
        3
    );
    ingest.assert_log_holds_none_of(VEHICLE_SENTINELS);
}

#[test]
fn a_child_exit_closes_the_gate_until_the_new_subscription_has_settled() {
    let mut ingest = start(&[]);
    ingest.read_event("degraded_mode");
    ingest.wait_for_vehicle();
    // The log task writes the lines of the events a little later than the
    // change; read the first `gate_opened` before the exit.
    ingest.read_event("gate_opened");
    let before = ingest.events.len();
    // `exit` ends the child at once, so the control connection gets no
    // answer.
    ingest.control("exit 3");
    let exited = ingest.read_event("child_exited");
    assert_eq!(exited["reason"], "exit code 3");
    ingest.read_after(
        before,
        |event| event["message"] == "ingest_event" && event["kind"] == "gate_opened",
        "the gate opened again",
    );
    let after: Vec<&str> = ingest.messages()[before..].to_vec();
    let position = |message: &str| {
        after
            .iter()
            .rposition(|m| *m == message)
            .unwrap_or_else(|| panic!("no {message} in {after:?}"))
    };
    let closed = after.iter().position(|m| *m == "ingest_event").unwrap();
    // The gate closed with the exit, before the restart; it opened again
    // only after the new connection and its settle time.
    assert!(closed < position("child_started"), "{after:?}");
    assert!(
        position("child_started") < position("subscriber_connected"),
        "{after:?}"
    );
    assert!(
        position("subscriber_connected") < position("subscriber_settled"),
        "{after:?}"
    );
    assert_eq!(after.last(), Some(&"ingest_event"));
    let mut vehicle = ingest.wait_for_vehicle();
    assert_eq!(vehicle.command("publish V 0 1"), "ok 1");
    ingest.signal("TERM");
    assert_eq!(ingest.wait().code(), Some(FLUSH_INCOMPLETE));
    assert_eq!(
        count(ingest.with_message("flush_incomplete")[0], "records"),
        1
    );
}
