//! Out-of-process test of the production watchdog action: a stalled task
//! ends the whole process with the watchdog exit code and a log line.
//!
//! The test starts its own test binary again with an environment variable.
//! In that second process, the same test function registers a task that
//! never beats and starts the watchdog with [`watchdog::expire`]. The first
//! process checks the exit status and the log of the second.
#![cfg(test)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ingest::watchdog::{self, Task, Watchdog};

const STALLED_MODE: &str = "INGEST_TEST_WATCHDOG_STALLED";
const TEST_NAME: &str = "the_watchdog_ends_the_process_when_a_task_stalls";

/// Time limit for the second process. A failing test then stops instead of
/// hanging.
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn the_watchdog_ends_the_process_when_a_task_stalls() {
    if std::env::var_os(STALLED_MODE).is_some() {
        run_stalled_task();
    }

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(STALLED_MODE, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > WAIT {
            child.kill().ok();
            child.wait().ok();
            panic!("the watchdog did not end the process");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();

    assert_eq!(status.code(), Some(watchdog::EXIT_CODE), "{stdout}");
    let event = stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["message"] == "watchdog_expired")
        .unwrap_or_else(|| panic!("no watchdog_expired event: {stdout}"));
    assert_eq!(event["task"], "stalled_task");
    assert_eq!(event["level"], "ERROR");
}

/// The body of the second process. It never returns: the watchdog ends the
/// process.
fn run_stalled_task() -> ! {
    ingest::logging::init().unwrap();
    let watchdog = Watchdog::new();
    let _stalled = watchdog.register(Task {
        name: "stalled_task",
        limit: Duration::from_millis(200),
    });
    watchdog
        .start(Duration::from_millis(20), watchdog::expire)
        .unwrap();
    std::thread::sleep(WAIT * 2);
    panic!("the watchdog did not act");
}
