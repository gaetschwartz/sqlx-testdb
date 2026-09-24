use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use parking_lot::Mutex;
use sqlx::Connection;
use tokio::time::Instant;

use crate::backend::{Backend, ConnectFailure, ConnectOptionsOf};
use crate::error::Error;

const REFUSED_RETRY_MILLIS: u64 = 250;
const REDACTED_PASSWORD: &str = "redacted";

type Outcome = Result<(), Arc<Error>>;

static PROBED: LazyLock<Mutex<HashMap<String, Outcome>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn ensure_reachable<DB: Backend>(
    url: &str,
    options: &ConnectOptionsOf<DB>,
    timeout: Duration,
) -> Result<(), Error> {
    let cached = PROBED.lock().get(url).cloned();
    let outcome = if let Some(outcome) = cached {
        outcome
    } else {
        let outcome = probe::<DB>(url, options, timeout).await.map_err(Arc::new);
        PROBED.lock().insert(url.to_owned(), outcome.clone());
        outcome
    };
    outcome.map_err(|source| Error::Probe { source })
}

async fn probe<DB: Backend>(
    url: &str,
    options: &ConnectOptionsOf<DB>,
    timeout: Duration,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    let retry = Duration::from_millis(REFUSED_RETRY_MILLIS);
    loop {
        let attempt = tokio::time::timeout_at(deadline, DB::Connection::connect_with(options));
        let Ok(connected) = attempt.await else {
            return Err(Error::ConnectTimeout {
                url: redact(url),
                timeout_secs: timeout.as_secs(),
            });
        };
        let error = match connected {
            Ok(conn) => {
                let _ = conn.close().await;
                return Ok(());
            }
            Err(error) => error,
        };
        match DB::classify_connect_error(&error) {
            ConnectFailure::Busy => return Ok(()),
            ConnectFailure::Rejected => {
                return Err(Error::Rejected { url: redact(url), source: error });
            }
            ConnectFailure::Refused if Instant::now() + retry < deadline => {
                tokio::time::sleep(retry).await;
            }
            ConnectFailure::Refused | ConnectFailure::Unreachable => {
                return Err(Error::Unreachable { url: redact(url), source: error });
            }
        }
    }
}

fn redact(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else { return "<an unparseable URL>".to_owned() };
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some(REDACTED_PASSWORD));
    }
    parsed.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_is_redacted_and_the_rest_kept() {
        assert_eq!(
            redact("postgres://user:secret@db.example:5432/app?sslmode=require"),
            "postgres://user:redacted@db.example:5432/app?sslmode=require"
        );
        assert_eq!(redact("postgres://user@db.example/app"), "postgres://user@db.example/app");
        assert_eq!(redact("not a url"), "<an unparseable URL>");
    }
}
