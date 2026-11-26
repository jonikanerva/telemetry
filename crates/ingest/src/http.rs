//! HTTP server of the ingest process: the health endpoint and its limits.
//!
//! Railway calls `GET /health` on `PORT` at the start of a deployment, until
//! the answer is 200 or its healthcheck timeout (default 300 s) ends. Each
//! check needs one short request. The server has these bounds
//! ([`LIMITS`]):
//!
//! - **Header read timeout: 5 s.** hyper closes a connection that does not
//!   send a complete request head within this time. The same timeout closes
//!   an idle keep-alive connection. It does not run while hyper writes a
//!   response.
//! - **Connection lifetime: 10 s.** The server closes every connection 10 s
//!   after the accept, also one that does not read its responses. A health
//!   request takes milliseconds, so 10 s is far above the need. A client
//!   that reuses a keep-alive connection opens a new connection after the
//!   close. The Railway documentation does not say whether its healthcheck
//!   reuses connections.
//! - **Connection limit: 8.** The server accepts the next connection only
//!   when fewer than 8 are open. Other clients wait in the listen backlog
//!   of the operating system. The lifetime bounds the wait for one free
//!   slot to 10 s. Limit: clients that open new connections again and again
//!   can keep all 8 slots, and `/health` then does not answer. ADR 0001
//!   gives the service a TCP proxy only to port 4443 and no HTTP domain, so
//!   only the Railway healthcheck reaches `PORT` from outside the container.
//! - **Shutdown deadline: 3 s.** At the shutdown, the server stops
//!   accepting, asks each connection to finish its current request, and
//!   closes the connections that are still open after the deadline. An idle
//!   connection closes at once. The health endpoint holds no state, so a
//!   forced close loses nothing. The outcome is the event
//!   `connections_closed_at_deadline` with the count, and a normal stop.
//! - **Accept failure:** after a failed `accept`, for example "too many
//!   open files", the server waits 0.5 s before the next attempt. A
//!   shutdown ends this wait at once.
//!
//! The server uses hyper directly instead of `axum::serve`, because
//! `axum::serve` sets no timer (so no header read timeout), has no
//! connection limit, and waits without a limit for open connections.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{info, warn};

/// Bounds of the HTTP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    /// Time that a client has to send a complete request head.
    pub header_read_timeout: Duration,
    /// Time from the accept to the forced close of a connection.
    pub connection_lifetime: Duration,
    /// Number of connections that can be open at the same time.
    pub max_connections: usize,
    /// Time from the shutdown to the forced close of open connections.
    pub shutdown_deadline: Duration,
}

/// The bounds of the production server. The module documentation gives
/// the reasons.
pub const LIMITS: ServerLimits = ServerLimits {
    header_read_timeout: Duration::from_secs(5),
    connection_lifetime: Duration::from_secs(10),
    max_connections: 8,
    shutdown_deadline: Duration::from_secs(3),
};

/// Wait after a failed `accept` before the next attempt. A failure such
/// as "too many open files" must not become a busy loop.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Builds the router of the health endpoint.
///
/// `GET /health` answers 200 while the server runs. ADR 0001 makes the answer
/// depend on the gate: 200 only when the gate is open and the start-up
/// check passed. The gate does not exist yet. The change that adds the gate
/// also changes this handler.
pub fn router() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> StatusCode {
    StatusCode::OK
}

/// Serves `router` on `listener` until `shutdown` completes. Then stops as
/// the module documentation describes.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()>,
    limits: ServerLimits,
) {
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
            Err(error) => {
                warn!(error = %error, "accept_failed");
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
        // Remove the finished connections, so that the set stays small.
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
    if drained.is_err() {
        warn!(
            connections = connections.len(),
            "connections_closed_at_deadline"
        );
        connections.shutdown().await;
    }
    info!("shutdown_complete");
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
    let served = async {
        tokio::select! {
            result = connection.as_mut() => result,
            () = stopped => {
                // Finish the current request, then close. An idle
                // connection closes at once.
                connection.as_mut().graceful_shutdown();
                connection.as_mut().await
            }
        }
    };
    // The lifetime covers the whole connection, also a response write that
    // waits for a client that does not read. The drop at the timeout closes
    // the socket.
    match tokio::time::timeout(limits.connection_lifetime, served).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => info!(error = error_class(&error), "connection_failed"),
        Err(_elapsed) => info!("connection_lifetime_exceeded"),
    }
}

/// Maps a hyper error to a static class. The log never gets the text of a
/// library error (see `crate::logging`).
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
    if let Err(error) = finished
        && error.is_panic()
    {
        warn!("connection_task_panicked");
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    use super::*;

    const TEST_LIMITS: ServerLimits = ServerLimits {
        header_read_timeout: Duration::from_millis(300),
        connection_lifetime: Duration::from_secs(60),
        max_connections: 8,
        shutdown_deadline: Duration::from_millis(300),
    };

    struct TestServer {
        address: SocketAddr,
        stop: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestServer {
        async fn start(router: Router, limits: ServerLimits) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel::<()>();
            let task = tokio::spawn(serve(
                listener,
                router,
                async move { stopped.await.unwrap() },
                limits,
            ));
            Self {
                address,
                stop,
                task,
            }
        }

        /// Starts the shutdown and returns the time until `serve` returned.
        async fn stop(self) -> Duration {
            let started = Instant::now();
            self.stop.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .unwrap()
                .unwrap();
            started.elapsed()
        }
    }

    async fn get_response(address: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// Opens a connection that sends only a part of a request head.
    async fn stalled_client(address: SocketAddr) -> TcpStream {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(b"GET /health HTTP/1.1\r\n").await.unwrap();
        stream
    }

    /// Reads until the server closes the connection and returns the time.
    async fn time_to_close(stream: &mut TcpStream) -> Duration {
        let started = Instant::now();
        let mut buffer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buffer))
            .await
            .expect("the server did not close the connection")
            .ok();
        started.elapsed()
    }

    #[tokio::test]
    async fn health_answers_200_and_shutdown_completes() {
        let server = TestServer::start(router(), TEST_LIMITS).await;
        let address = server.address;

        let health = get_response(address, "/health").await;
        assert!(health.starts_with("HTTP/1.1 200 "), "{health}");
        let missing = get_response(address, "/other").await;
        assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");

        let elapsed = server.stop().await;
        assert!(elapsed < TEST_LIMITS.shutdown_deadline, "{elapsed:?}");
    }

    #[tokio::test]
    async fn a_client_that_does_not_finish_its_request_head_is_cut_off() {
        let server = TestServer::start(router(), TEST_LIMITS).await;
        let mut client = stalled_client(server.address).await;

        let elapsed = time_to_close(&mut client).await;
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        server.stop().await;
    }

    #[tokio::test]
    async fn a_client_that_sends_nothing_is_cut_off() {
        let server = TestServer::start(router(), TEST_LIMITS).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();

        let elapsed = time_to_close(&mut client).await;
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        server.stop().await;
    }

    #[tokio::test]
    async fn an_idle_keep_alive_connection_is_cut_off() {
        let server = TestServer::start(router(), TEST_LIMITS).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        let mut head = [0_u8; 12];
        client.read_exact(&mut head).await.unwrap();
        assert_eq!(&head, b"HTTP/1.1 200");

        // The connection stays open for a next request. Without one, the
        // header read timeout closes it.
        let elapsed = time_to_close(&mut client).await;
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        server.stop().await;
    }

    #[tokio::test]
    async fn a_failed_connection_logs_a_static_class_only() {
        let (capture, _guard) = crate::logging::capture::install(crate::logging::ALLOWED_FIELDS);
        let server = TestServer::start(router(), TEST_LIMITS).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();
        client
            .write_all(b"GET /health?token=hunter2 HTTP/1.1\r\nBad Header hunter2\r\n\r\n")
            .await
            .unwrap();
        time_to_close(&mut client).await;
        server.stop().await;

        let text = capture.text();
        assert!(text.contains("\"message\":\"connection_failed\""), "{text}");
        assert!(text.contains("\"error\":\"invalid_request\""), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[tokio::test]
    async fn a_client_that_does_not_read_is_cut_off_at_the_lifetime() {
        // One slot, and no header timeout in the way: only the lifetime can
        // free the slot.
        let limits = ServerLimits {
            header_read_timeout: Duration::from_secs(60),
            connection_lifetime: Duration::from_millis(500),
            max_connections: 1,
            ..TEST_LIMITS
        };
        let (capture, _guard) = crate::logging::capture::install(crate::logging::ALLOWED_FIELDS);
        let server = TestServer::start(router(), limits).await;

        // The client sends pipelined requests and never reads. With a small
        // receive buffer, the response writes of the server stop soon.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(1024).unwrap();
        let mut client = socket.connect(server.address).await.unwrap();
        let requests = b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n".repeat(200_000);
        let writer = tokio::spawn(async move {
            // The write fails when the server closes the connection.
            client.write_all(&requests).await.ok();
            client
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!writer.is_finished(), "the server read all requests");

        // `/health` answers again after the lifetime frees the slot.
        let started = Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            get_response(server.address, "/health"),
        )
        .await
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        assert!(started.elapsed() < Duration::from_secs(2));
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap();
        server.stop().await;
        assert!(
            capture
                .text()
                .contains("\"message\":\"connection_lifetime_exceeded\""),
            "{}",
            capture.text()
        );
    }

    #[tokio::test]
    async fn shutdown_closes_an_idle_keep_alive_connection_at_once() {
        // A long deadline and no header timeout in the way: only the
        // graceful close can end the connection early.
        let limits = ServerLimits {
            header_read_timeout: Duration::from_secs(60),
            shutdown_deadline: Duration::from_secs(5),
            ..TEST_LIMITS
        };
        let server = TestServer::start(router(), limits).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        let mut head = [0_u8; 12];
        client.read_exact(&mut head).await.unwrap();
        assert_eq!(&head, b"HTTP/1.1 200");

        let elapsed = server.stop().await;
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        assert!(time_to_close(&mut client).await < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_panic_in_a_connection_task_is_logged() {
        let (capture, _guard) = crate::logging::capture::install(crate::logging::ALLOWED_FIELDS);
        let router = Router::new().route("/panic", get(|| async { panic!("test panic") }));
        let server = TestServer::start(router, TEST_LIMITS).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();
        client
            .write_all(b"GET /panic HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        time_to_close(&mut client).await;
        server.stop().await;

        let text = capture.text();
        assert!(
            text.contains("\"message\":\"connection_task_panicked\""),
            "{text}"
        );
        assert!(!text.contains("test panic"), "{text}");
    }

    #[tokio::test]
    async fn shutdown_closes_a_stalled_client_at_the_deadline() {
        // A long header timeout, so that only the shutdown deadline can
        // close the connection.
        let limits = ServerLimits {
            header_read_timeout: Duration::from_secs(60),
            ..TEST_LIMITS
        };
        let server = TestServer::start(router(), limits).await;
        let mut client = stalled_client(server.address).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let elapsed = server.stop().await;
        assert!(elapsed >= limits.shutdown_deadline, "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert!(time_to_close(&mut client).await < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn shutdown_closes_a_hanging_request_at_the_deadline() {
        let router = Router::new().route("/hang", get(pending::<()>));
        let server = TestServer::start(router, TEST_LIMITS).await;
        let mut client = TcpStream::connect(server.address).await.unwrap();
        client
            .write_all(b"GET /hang HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        // Let the server start the request before the shutdown starts.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let elapsed = server.stop().await;
        assert!(elapsed >= TEST_LIMITS.shutdown_deadline, "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    #[tokio::test]
    async fn the_connection_limit_holds_the_next_client_until_a_slot_is_free() {
        let limits = ServerLimits {
            header_read_timeout: Duration::from_secs(60),
            max_connections: 1,
            ..TEST_LIMITS
        };
        let server = TestServer::start(router(), limits).await;
        let first = stalled_client(server.address).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second = tokio::spawn(get_response(server.address, "/health"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!second.is_finished(), "the second client got an answer");

        drop(first);
        let response = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        server.stop().await;
    }
}
