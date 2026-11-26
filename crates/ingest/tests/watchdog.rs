//! Out-of-process tests of the production watchdog action: a stalled task
//! ends the whole process with the watchdog exit code, with a log line, and
//! also when standard output is blocked.
//!
//! Each test starts its own test binary again with an environment variable.
//! In that second process, the same test function registers a task that
//! never beats and starts the watchdog with [`watchdog::expire`]. The first
//! process checks the exit status and the log of the second.
#![cfg(test)]

use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ingest::watchdog::{self, Task, Watchdog};

/// Set in the second process; the value names the mode.
const MODE: &str = "INGEST_TEST_WATCHDOG_MODE";

/// Time limit for the second process. A failing test then stops instead of
/// hanging.
const WAIT: Duration = Duration::from_secs(10);

/// Runs the named test of this binary in a second process with `mode`, and
/// returns its exit status, its standard output, and its run time.
fn run_second_process(test_name: &str, mode: &str) -> (ExitStatus, String, Duration) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(MODE, mode)
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
    let elapsed = started.elapsed();
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    (status, stdout, elapsed)
}

fn watchdog_events(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["message"] == "watchdog_expired")
        .collect()
}

#[test]
fn the_watchdog_ends_the_process_when_a_task_stalls() {
    if std::env::var_os(MODE).is_some() {
        run_stalled_task(false);
    }
    let (status, stdout, _) =
        run_second_process("the_watchdog_ends_the_process_when_a_task_stalls", "log");

    assert_eq!(status.code(), Some(watchdog::EXIT_CODE), "{stdout}");
    let events = watchdog_events(&stdout);
    assert_eq!(events.len(), 1, "{stdout}");
    assert_eq!(events[0]["task"], "stalled_task");
    assert_eq!(events[0]["level"], "ERROR");
}

#[test]
fn the_watchdog_ends_the_process_also_when_standard_output_is_blocked() {
    if std::env::var_os(MODE).is_some() {
        run_stalled_task(true);
    }
    let (status, stdout, elapsed) = run_second_process(
        "the_watchdog_ends_the_process_also_when_standard_output_is_blocked",
        "blocked",
    );

    assert_eq!(status.code(), Some(watchdog::EXIT_CODE), "{stdout}");
    // The log line cannot be written, and the process ends after the wait
    // of `expire` for it (1 s).
    assert_eq!(watchdog_events(&stdout).len(), 0, "{stdout}");
    assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
}

/// The body of the second process. It never returns: the watchdog ends the
/// process. With `block_output`, another thread holds the lock of standard
/// output first, as a hung writer would.
fn run_stalled_task(block_output: bool) -> ! {
    ingest::logging::init().unwrap();
    if block_output {
        let (locked, is_locked) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = std::io::stdout().lock();
            stdout.flush().ok();
            locked.send(()).unwrap();
            // Hold the lock until the process ends.
            loop {
                std::thread::park();
            }
        });
        is_locked.recv().unwrap();
    }
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
