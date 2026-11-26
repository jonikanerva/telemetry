//! Certificate verification and the log of server notices.

use std::io;
use std::sync::{Arc, Mutex};

use ingest::logging::{ALLOWED_FIELDS, JsonAllowlist};
use ingest::storage::Schema;
use sqlx::{Connection, PgConnection};
use tracing_subscriber::fmt::MakeWriter;

use crate::support::{owner_options, without_verification};

#[tokio::test]
async fn connections_verify_the_server_certificate() {
    let options = owner_options(&Schema::new("public").unwrap());
    // The name in the certificate is the host name. Connect to the same
    // server by its address: the name check must fail the TLS handshake.
    let host = options.get_host().to_owned();
    let address = tokio::net::lookup_host((host.as_str(), options.get_port()))
        .await
        .unwrap()
        .next()
        .unwrap()
        .ip()
        .to_string();
    let error = PgConnection::connect_with(&options.clone().host(&address))
        .await
        .unwrap_err();
    // rustls rejects the certificate inside the TLS stream, so SQLx
    // returns it as an I/O error.
    let text = format!("{error:?}");
    assert!(text.contains("InvalidCertificate"), "{text}");
    // Without the verification, TLS passes and the server answers; Neon then
    // refuses the session because the address names no endpoint.
    let unverified =
        PgConnection::connect_with(&without_verification(options.clone()).host(&address)).await;
    let text = format!("{unverified:?}");
    assert!(!text.contains("InvalidCertificate"), "{text}");
    if let Ok(conn) = unverified {
        conn.close().await.unwrap();
    }
    // With the host name, the verified connection works.
    let conn = PgConnection::connect_with(&options).await.unwrap();
    conn.close().await.unwrap();
}

/// A log writer that keeps the output in memory.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn server_notices_do_not_reach_the_log() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .event_format(JsonAllowlist::new(ALLOWED_FIELDS))
        .with_max_level(tracing::Level::INFO)
        .with_writer(capture.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let options = owner_options(&Schema::new("public").unwrap());
    // `DROP TABLE IF EXISTS` of a missing table sends a NOTICE with the
    // table name.
    let statement = "DROP TABLE IF EXISTS itest_no_such_table_7f3a";
    let mut conn = PgConnection::connect_with(&options).await.unwrap();
    sqlx::raw_sql(statement).execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
    let quiet = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!quiet.contains("sqlx::postgres::notice"), "{quiet}");

    // With notices on, SQLx logs one, and the formatter redacts its text.
    let mut conn =
        PgConnection::connect_with(&options.options([("client_min_messages", "notice")]))
            .await
            .unwrap();
    sqlx::raw_sql(statement).execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
    drop(guard);
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("sqlx::postgres::notice"),
        "the notice was logged: {text}"
    );
    assert!(!text.contains("itest_no_such_table"), "{text}");
}
