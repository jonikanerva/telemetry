//! The HTTP/1 accept loop and its limits.
//!
//! The server uses hyper directly instead of `axum::serve`, because
//! `axum::serve` sets no timer (so no header read timeout), has no
//! connection limit, and waits without a limit for open connections at
//! the shutdown. The bounds ([`LIMITS`]):
//!
//! - **Header read timeout: 10 s.** hyper closes a connection that does not
//!   send a complete request head within this time. The timer also runs
//!   while a keep-alive connection waits for its next request, so an idle
//!   connection closes after 10 s. The value is a provisional choice; if
//!   the Railway proxy reuses idle connections longer, a request can meet
//!   a closed connection.
//! - **Connection limit: 256.** The server accepts the next connection only
//!   when fewer than 256 are open. Other clients wait in the listen
//!   backlog. One user with a few browser tabs needs far fewer. Limit: a
//!   client that reads a response very slowly keeps its slot; no timeout
//!   bounds the write of a response body.
//! - **Shutdown deadline: 10 s** ([`crate::shutdown`]).
//! - **Accept failure:** after a failed `accept`, for example "too many
//!   open files", the server waits 0.5 s before the next attempt.
//!
//! The request timeout (30 s) and the body limit (16 KiB) are middleware
//! of the router (`crate::app`). Only HTTP/1.1 is served: the Railway proxy
//! ends TLS and HTTP/2 at its edge. A request that hyper rejects before
//! the router (for example a malformed head, status 400 or 431) gets the
//! plain hyper response, without a problem body or the security headers.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{info, warn};

use crate::shutdown::SHUTDOWN_DEADLINE;

/// Bounds of the HTTP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    /// Time that a client has to send a complete request head.
    pub header_read_timeout: Duration,
    /// Number of connections that can be open at the same time.
    pub max_connections: usize,
    /// Time from the shutdown to the forced close of open connections.
    pub shutdown_deadline: Duration,
}

/// The bounds of the production server. The module documentation gives
/// the reasons.
pub const LIMITS: ServerLimits = ServerLimits {
    header_read_timeout: Duration::from_secs(10),
    max_connections: 256,
    shutdown_deadline: SHUTDOWN_DEADLINE,
};

/// Wait after a failed `accept` before the next attempt.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// How the server stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    /// Every connection closed before the deadline.
    Drained,
    /// The deadline ended the wait; the value is the count of connections
    /// that the server closed.
    DeadlineExceeded(usize),
}

/// Serves `router` on `listener` until `shutdown` completes. Then stops
/// as [`crate::shutdown`] describes.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()>,
    limits: ServerLimits,
) -> ServeOutcome {
    let permits = Arc::new(Semaphore::new(limits.max_connections));
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    let mut shutdown = pin!(shutdown);

    loop {
        let permit = tokio::select! {
            () = &mut shutdown => break,
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                // The semaphore is never closed. Stop serving if it is.
                Err(_closed) => break,
            },
        };
        let accepted = tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _address)) => stream,
            Err(err) => {
                warn!(error = ?err.kind(), "accept_failed");
                tokio::select! {
                    () = &mut shutdown => break,
                    () = tokio::time::sleep(ACCEPT_RETRY_DELAY) => continue,
                }
            }
        };
        connections.spawn(serve_connection(
            stream,
            router.clone(),
            limits,
            stop_rx.clone(),
            permit,
        ));
        while let Some(finished) = connections.try_join_next() {
            log_task_failure(finished);
        }
    }

    // Close the listening socket first, so that no client waits for an
    // accept that never comes.
    drop(listener);
    stop_tx.send_replace(true);
    let drained = tokio::time::timeout(limits.shutdown_deadline, async {
        while let Some(finished) = connections.join_next().await {
            log_task_failure(finished);
        }
    })
    .await;
    let outcome = if drained.is_ok() {
        ServeOutcome::Drained
    } else {
        let open = connections.len();
        warn!(connections = open, "connections_closed_at_deadline");
        connections.shutdown().await;
        ServeOutcome::DeadlineExceeded(open)
    };
    info!("server_stopped");
    outcome
}

async fn serve_connection(
    stream: TcpStream,
    router: Router,
    limits: ServerLimits,
    mut stop: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);
    let connection =
        builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(router));
    let mut connection = pin!(connection);

    // `wait_for` returns a guard that is not `Send`. Map it to `()` before
    // the next await.
    let stopped = async move {
        stop.wait_for(|stop| *stop).await.ok();
    };
    let result = tokio::select! {
        result = connection.as_mut() => result,
        () = stopped => {
            // Finish the current request, then close. An idle connection
            // closes at once.
            connection.as_mut().graceful_shutdown();
            connection.as_mut().await
        }
    };
    // A failed connection is a client matter (a reset, a timeout, a
    // malformed request). The class goes to the log, never the text.
    if let Err(err) = result {
        info!(cause = error_class(&err), "connection_failed");
    }
}

/// Maps a hyper error to a static class.
fn error_class(error: &hyper::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_parse() {
        "invalid_request"
    } else if error.is_incomplete_message() {
        "incomplete_request"
    } else {
        "connection_error"
    }
}

/// Logs a connection task that panicked. A task that the shutdown
/// cancelled is not a failure.
fn log_task_failure(finished: Result<(), JoinError>) {
    if let Err(err) = finished
        && err.is_panic()
    {
        warn!("connection_task_panicked");
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Notify, oneshot};

    use super::*;

    const TEST_LIMITS: ServerLimits = ServerLimits {
        header_read_timeout: Duration::from_millis(300),
        max_connections: 8,
        shutdown_deadline: Duration::from_millis(300),
    };

    /// `/stall` signals `started` when its handler runs, then never ends.
    fn router(started: Arc<Notify>) -> Router {
        Router::new().route("/fast", get(|| async { "ok" })).route(
            "/stall",
            get(move || async move {
                started.notify_one();
                pending::<&'static str>().await
            }),
        )
    }

    struct TestServer {
        address: SocketAddr,
        started: Arc<Notify>,
        stop: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<ServeOutcome>,
    }

    impl TestServer {
        async fn start(limits: ServerLimits) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let started = Arc::new(Notify::new());
            let (stop, stopped) = oneshot::channel::<()>();
            let task = tokio::spawn(serve(
                listener,
                router(Arc::clone(&started)),
                async move { stopped.await.unwrap() },
                limits,
            ));
            Self {
                address,
                started,
                stop,
                task,
            }
        }

        /// Starts the shutdown and returns the outcome and the time until
        /// `serve` returned.
        async fn stop(self) -> (ServeOutcome, Duration) {
            let started = Instant::now();
            self.stop.send(()).unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .unwrap()
                .unwrap();
            (outcome, started.elapsed())
        }
    }

    async fn send(address: SocketAddr, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        stream
    }

    #[tokio::test]
    async fn serves_a_request_and_drains_at_the_shutdown() {
        let server = TestServer::start(TEST_LIMITS).await;
        let mut stream = send(server.address, "/fast").await;
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        let (outcome, _elapsed) = server.stop().await;
        assert_eq!(outcome, ServeOutcome::Drained);
    }

    #[tokio::test]
    async fn closes_an_open_request_at_the_deadline() {
        let server = TestServer::start(TEST_LIMITS).await;
        let mut stream = send(server.address, "/stall").await;
        // Wait until the handler runs, so the request is open.
        tokio::time::timeout(Duration::from_secs(5), server.started.notified())
            .await
            .unwrap();
        let (outcome, elapsed) = server.stop().await;
        assert_eq!(outcome, ServeOutcome::DeadlineExceeded(1));
        assert!(elapsed >= TEST_LIMITS.shutdown_deadline, "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.ok();
        assert_eq!(rest, b"");
    }

    #[tokio::test]
    async fn closes_a_connection_without_a_complete_head() {
        let server = TestServer::start(TEST_LIMITS).await;
        let mut stream = TcpStream::connect(server.address).await.unwrap();
        stream.write_all(b"GET /fast HTTP/1.1\r\n").await.unwrap();
        let started = Instant::now();
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest))
            .await
            .expect("the server did not close the connection")
            .ok();
        assert!(started.elapsed() < Duration::from_secs(2));
        let (outcome, _) = server.stop().await;
        assert_eq!(outcome, ServeOutcome::Drained);
    }

    #[test]
    fn the_production_deadline_is_below_the_railway_draining_time() {
        assert_eq!(LIMITS.shutdown_deadline, Duration::from_secs(10));
        assert!(LIMITS.shutdown_deadline < Duration::from_secs(15));
    }
}
