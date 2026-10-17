use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use parking_lot::Mutex;
use sqlx::Connection;
use tokio::time::Instant;

use crate::backend::{Backend, ConnectFailure, ConnectOptionsOf};
use crate::error::Error;

const RETRY_MILLIS: u64 = 250;
const REDACTED_PASSWORD: &str = "redacted";

static PROBED: LazyLock<Mutex<HashMap<String, Arc<Error>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn connect<DB: Backend>(
    url: &str,
    options: &ConnectOptionsOf<DB>,
    timeout: Duration,
    busy_timeout: Duration,
) -> Result<DB::Connection, Error> {
    let cached = PROBED.lock().get(url).cloned();
    if let Some(failure) = cached {
        return Err(Error::Probe { source: failure });
    }
    match attempt::<DB>(url, options, timeout, busy_timeout).await {
        Ok(conn) => Ok(conn),
        Err(error @ Error::Connect { .. }) => Err(error),
        Err(error) => {
            let failure = Arc::new(error);
            PROBED.lock().insert(url.to_owned(), failure.clone());
            Err(Error::Probe { source: failure })
        }
    }
}

async fn attempt<DB: Backend>(
    url: &str,
    options: &ConnectOptionsOf<DB>,
    timeout: Duration,
    busy_timeout: Duration,
) -> Result<DB::Connection, Error> {
    let started = Instant::now();
    let mut deadline = started + timeout;
    let retry = Duration::from_millis(RETRY_MILLIS);
    loop {
        let connecting = tokio::time::timeout_at(deadline, DB::Connection::connect_with(options));
        let Ok(connected) = connecting.await else {
            return Err(Error::ConnectTimeout {
                url: redact(url),
                timeout_secs: deadline.duration_since(started).as_secs(),
            });
        };
        let error = match connected {
            Ok(conn) => return Ok(conn),
            Err(error) => error,
        };
        match DB::classify_connect_error(&error) {
            ConnectFailure::Busy => {
                deadline = started + busy_timeout;
                if Instant::now() + retry >= deadline {
                    return Err(Error::Connect { source: error });
                }
                tokio::time::sleep(retry).await;
            }
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
