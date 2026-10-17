mod common;

use std::panic::catch_unwind;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use sqlx_testdb::{Config, PoolSettings, run_with};

use common::{BOOKKEEPING, token};

const UNSET_VAR: &str = "SQLX_TESTDB_PROBE_UNSET";
const WELL_UNDER_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
const FAST: Duration = Duration::from_secs(3);
const LIMITED: usize = 2;
const CACHED: Duration = Duration::from_millis(500);

fn config(url: &str) -> Config {
    Config {
        database_url_var: UNSET_VAR.to_owned(),
        database_url: Some(url.to_owned()),
        prefix: format!("it{}", token()),
        bookkeeping_schema: BOOKKEEPING.to_owned(),
        ..Config::default()
    }
}

fn failure(config: &Config, test_path: &'static str) -> (String, Duration) {
    let started = Instant::now();
    let outcome = catch_unwind(|| run_with(config, test_path, &[], |_: PgPool| async {}));
    let message = *outcome.unwrap_err().downcast::<String>().unwrap();
    (message, started.elapsed())
}

fn first_line(message: &str) -> &str {
    message.lines().next().unwrap()
}

fn server_url_with(change: impl FnOnce(&mut url::Url)) -> String {
    let mut url = url::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    change(&mut url);
    url.into()
}

#[test]
fn an_unreachable_server_fails_within_the_connect_timeout_and_is_probed_once() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let config = config(&format!("postgres://probe:secret@127.0.0.1:{port}/nowhere"));
    let (message, elapsed) = failure(&config, "probe::unreachable");
    assert_eq!(
        first_line(&message),
        format!(
            "probe::unreachable: cannot reach the test database server at \
             postgres://probe:redacted@127.0.0.1:{port}/nowhere"
        )
    );
    assert!(elapsed < WELL_UNDER_ACQUIRE_TIMEOUT, "took {elapsed:?}");
    let (again, elapsed) = failure(&config, "probe::unreachable");
    assert_eq!(first_line(&again), first_line(&message));
    assert!(elapsed < CACHED, "the second test re-probed: {elapsed:?}");
}

#[test]
fn a_wrong_password_fails_fast_with_the_reason() {
    let wrong = token();
    let url = server_url_with(|url| url.set_password(Some(&wrong)).unwrap());
    let (message, elapsed) = failure(&config(&url), "probe::password");
    let redacted = server_url_with(|url| url.set_password(Some("redacted")).unwrap());
    assert_eq!(
        first_line(&message),
        format!("probe::password: the test database server at {redacted} refused the connection")
    );
    assert!(message.contains("password authentication failed"), "{message}");
    assert!(elapsed < FAST, "took {elapsed:?}");
}

#[test]
fn a_missing_database_fails_fast_with_the_reason() {
    let missing = format!("sqlx_testdb_missing_{}", token());
    let url = server_url_with(|url| url.set_path(&missing));
    let (message, elapsed) = failure(&config(&url), "probe::missing");
    assert!(first_line(&message).ends_with("refused the connection"), "{message}");
    assert!(message.contains(&format!("database \"{missing}\" does not exist")), "{message}");
    assert!(elapsed < FAST, "took {elapsed:?}");
}

fn hold_connections(url: &str, count: usize) -> std::sync::mpsc::Sender<()> {
    let (release, released) = std::sync::mpsc::channel();
    let (connected, is_connected) = std::sync::mpsc::channel();
    let url = url.to_owned();
    std::thread::spawn(move || {
        common::block_on(async move {
            let mut held = Vec::with_capacity(count);
            for _ in 0..count {
                held.push(<sqlx::PgConnection as sqlx::Connection>::connect(&url).await.unwrap());
            }
            connected.send(()).unwrap();
            released.recv().unwrap();
            drop(held);
        });
    });
    is_connected.recv().unwrap();
    release
}

#[test]
fn a_server_without_free_connections_is_waited_for_until_the_acquire_timeout() {
    let role = format!("busy_{}", token());
    let bookkeeping = format!("sqlx_testdb_busy_{}", token());
    let database = common::block_on(async {
        let mut admin = common::admin().await;
        let database: String =
            sqlx::query_scalar("SELECT current_database()").fetch_one(&mut admin).await.unwrap();
        common::execute(
            &mut admin,
            &format!(
                r#"CREATE ROLE "{role}" LOGIN PASSWORD 'pw' CONNECTION LIMIT {LIMITED} CREATEDB;
                   GRANT CREATE ON DATABASE "{database}" TO "{role}";"#
            ),
        )
        .await;
        database
    });
    let url = server_url_with(|url| {
        url.set_username(&role).unwrap();
        url.set_password(Some("pw")).unwrap();
    });
    let with_acquire_timeout = |acquire_timeout| Config {
        bookkeeping_schema: bookkeeping.clone(),
        pool: PoolSettings { acquire_timeout, ..PoolSettings::DEFAULT },
        ..config(&url)
    };

    let release = hold_connections(&url, LIMITED);
    let impatient = with_acquire_timeout(Duration::from_secs(1));
    let (message, elapsed) = failure(&impatient, "probe::busy");
    assert_eq!(first_line(&message), "probe::busy: connecting to the test server");
    assert!(elapsed >= Duration::from_millis(900) && elapsed < WELL_UNDER_ACQUIRE_TIMEOUT);

    let patient = with_acquire_timeout(Duration::from_secs(20));
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        release.send(()).unwrap();
    });
    let started = Instant::now();
    run_with(&patient, "probe::busy_then_free", &[], |_: PgPool| async {});
    assert!(started.elapsed() >= Duration::from_millis(500));

    common::block_on(async {
        let mut admin = common::admin().await;
        common::drop_all(&mut admin, &patient.prefix).await;
        common::execute(
            &mut admin,
            &format!(
                r#"DROP SCHEMA IF EXISTS "{bookkeeping}" CASCADE;
                   DROP OWNED BY "{role}";
                   DROP ROLE "{role}";"#
            ),
        )
        .await;
    });
}

#[test]
fn a_reachable_server_passes_the_probe() {
    let config = config(&std::env::var("DATABASE_URL").unwrap());
    run_with(&config, "probe::reachable", &[], |pool: PgPool| async move {
        sqlx::query("SELECT 1").execute(&pool).await.unwrap();
    });
    common::block_on(async { common::drop_all(&mut common::admin().await, &config.prefix).await });
}
