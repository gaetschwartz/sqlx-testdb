mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use sqlx::{PgConnection, PgPool};
use sqlx_testdb::{Config, run};

use common::{
    BOOKKEEPING, admin, block_on, config, current_database, drop_all, execute, exists, is_template,
    leak_config, names, record, seen, token, with_prefix,
};

const SCHEMA: &str = "CREATE TABLE things (id INT PRIMARY KEY);";

fn fail() {
    panic!("the test failed");
}

fn passing(config: &'static Config, test_path: &'static str) -> String {
    let seen = seen();
    let recorder = seen.clone();
    run(config, test_path, &[], move |pool: PgPool| async move {
        record(&recorder, current_database(&pool).await);
    });
    names(&seen).remove(0)
}

async fn failed_at_is_set(conn: &mut PgConnection, bookkeeping: &str, name: &str) -> bool {
    sqlx::query_scalar(&format!(
        r#"SELECT failed_at IS NOT NULL FROM "{bookkeeping}".databases WHERE name = $1"#
    ))
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap()
}

async fn is_recorded(conn: &mut PgConnection, bookkeeping: &str, table: &str, name: &str) -> bool {
    sqlx::query_scalar(&format!(
        r#"SELECT EXISTS (SELECT 1 FROM "{bookkeeping}".{table} WHERE name = $1)"#
    ))
    .bind(name)
    .fetch_one(conn)
    .await
    .unwrap()
}

#[test]
fn the_template_is_built_once_and_reused_and_passing_databases_are_dropped() {
    let prefix = format!("it{}", token());
    let config = leak_config(config(&prefix, BOOKKEEPING, &token(), SCHEMA));
    let first = passing(config, "lifecycle::first");
    let second = passing(config, "lifecycle::second");
    assert_ne!(first, second);
    block_on(async {
        let mut conn = admin().await;
        assert!(!exists(&mut conn, &first).await);
        assert!(!exists(&mut conn, &second).await);
        assert!(!is_recorded(&mut conn, BOOKKEEPING, "databases", &first).await);
        let templates = with_prefix(&mut conn, &format!("{prefix}_template_")).await;
        assert_eq!(templates.len(), 1, "{templates:?}");
        assert!(is_template(&mut conn, &templates[0]).await);
        assert!(is_recorded(&mut conn, BOOKKEEPING, "templates", &templates[0]).await);
        drop_all(&mut conn, &prefix).await;
    });
}

#[test]
fn a_schema_change_builds_a_new_template() {
    let prefix = format!("it{}", token());
    let root = token();
    let before = leak_config(config(&prefix, BOOKKEEPING, &root, SCHEMA));
    let after = leak_config(config(&prefix, BOOKKEEPING, &root, "CREATE TABLE others ();"));
    passing(before, "lifecycle::before");
    let seen = seen();
    let recorder = seen.clone();
    run(after, "lifecycle::after", &[], move |pool: PgPool| async move {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        record(&recorder, tables.join(","));
    });
    assert_eq!(names(&seen), ["others"]);
    block_on(async {
        let mut conn = admin().await;
        let templates = with_prefix(&mut conn, &format!("{prefix}_template_")).await;
        assert_eq!(templates.len(), 2, "{templates:?}");
        drop_all(&mut conn, &prefix).await;
    });
}

#[test]
fn a_failing_test_keeps_its_database_and_marks_it_failed() {
    let prefix = format!("it{}", token());
    let config = leak_config(config(&prefix, BOOKKEEPING, &token(), SCHEMA));
    let panicked = seen();
    let recorder = panicked.clone();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        run(config, "lifecycle::panics", &[], move |pool: PgPool| async move {
            record(&recorder, current_database(&pool).await);
            fail();
        });
    }));
    assert!(outcome.is_err());
    let errored = seen();
    let recorder = errored.clone();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        run(config, "lifecycle::errs", &[], move |pool: PgPool| async move {
            record(&recorder, current_database(&pool).await);
            Err::<(), _>("the test returned an error")
        });
    }));
    assert!(outcome.is_err());
    block_on(async {
        let mut conn = admin().await;
        for name in [&names(&panicked)[0], &names(&errored)[0]] {
            assert!(exists(&mut conn, name).await, "{name}");
            assert!(failed_at_is_set(&mut conn, BOOKKEEPING, name).await, "{name}");
            execute(
                &mut conn,
                &format!(r#"DELETE FROM "{BOOKKEEPING}".databases WHERE name = '{name}'"#),
            )
            .await;
        }
        drop_all(&mut conn, &prefix).await;
    });
}

#[test]
fn keep_failed_off_drops_a_failing_database() {
    let prefix = format!("it{}", token());
    let config = leak_config(Config {
        keep_failed: false,
        ..config(&prefix, BOOKKEEPING, &token(), SCHEMA)
    });
    let seen = seen();
    let recorder = seen.clone();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        run(config, "lifecycle::dropped", &[], move |pool: PgPool| async move {
            record(&recorder, current_database(&pool).await);
            fail();
        });
    }));
    assert!(outcome.is_err());
    block_on(async {
        let mut conn = admin().await;
        let name = &names(&seen)[0];
        assert!(!exists(&mut conn, name).await);
        assert!(!is_recorded(&mut conn, BOOKKEEPING, "databases", name).await);
        drop_all(&mut conn, &prefix).await;
    });
}

#[test]
fn a_missing_database_url_names_the_variable() {
    let config = leak_config(Config {
        database_url_var: "SQLX_TESTDB_UNSET_URL",
        ..config("itunset", BOOKKEEPING, "unset", SCHEMA)
    });
    let outcome = catch_unwind(|| run(config, "lifecycle::unset", &[], |_: PgPool| async {}));
    let message = outcome.unwrap_err().downcast::<String>().unwrap();
    assert_eq!(
        message.trim_end(),
        "lifecycle::unset: SQLX_TESTDB_UNSET_URL is not set; point it at the database server the \
         tests may use"
    );
}

#[test]
fn the_first_test_of_a_run_sweeps_leftovers_without_forcing() {
    let prefix = format!("it{}", token());
    let bookkeeping = format!("sqlx_testdb_sweep_{}", token());
    let base = config(&prefix, &bookkeeping, "sweep-setup", SCHEMA);
    let current = passing(leak_config(base), "lifecycle::setup");
    let (stale, busy, live, old_failed, new_failed) = (
        format!("{prefix}_stale"),
        format!("{prefix}_busy"),
        format!("{prefix}_live"),
        format!("{prefix}_old_failed"),
        format!("{prefix}_new_failed"),
    );
    let idle_template = format!("{prefix}_template_0000000000000000");
    let orphan_build = format!("{prefix}_building_abcdef");
    let b = &bookkeeping;
    block_on(async {
        let mut conn = admin().await;
        for name in [&stale, &busy, &live, &old_failed, &new_failed, &idle_template, &orphan_build]
        {
            execute(&mut conn, &format!(r#"CREATE DATABASE "{name}""#)).await;
        }
        execute(
            &mut conn,
            &format!(
                r#"ALTER DATABASE "{idle_template}" WITH IS_TEMPLATE true ALLOW_CONNECTIONS false;
                INSERT INTO "{b}".templates (name, last_used_at)
                    VALUES ('{idle_template}', now() - interval '2 days');
                INSERT INTO "{b}".runs (run, project_root, last_seen_at) VALUES
                    ('stale', 'x', now() - interval '2 hours'), ('live', 'x', now());
                INSERT INTO "{b}".databases (name, test_path, run, failed_at) VALUES
                    ('{stale}', 't', 'stale', NULL), ('{busy}', 't', 'stale', NULL),
                    ('{live}', 't', 'live', NULL),
                    ('{old_failed}', 't', 'live', now() - interval '2 days'),
                    ('{new_failed}', 't', 'live', now());"#
            ),
        )
        .await;
    });
    let busy_conn = hold_connection(&busy);
    passing(leak_config(Config { project_root: "sweep-1", ..base }), "lifecycle::sweep");
    block_on(async {
        let mut conn = admin().await;
        assert!(!exists(&mut conn, &stale).await);
        assert!(!is_recorded(&mut conn, b, "databases", &stale).await);
        assert!(exists(&mut conn, &busy).await, "a database in use is never forced");
        assert!(is_recorded(&mut conn, b, "databases", &busy).await);
        assert!(exists(&mut conn, &live).await);
        assert!(!exists(&mut conn, &old_failed).await);
        assert!(exists(&mut conn, &new_failed).await);
        assert!(!exists(&mut conn, &idle_template).await);
        assert!(!is_recorded(&mut conn, b, "templates", &idle_template).await);
        assert!(!exists(&mut conn, &orphan_build).await);
        let template = current_template(&mut conn, &prefix).await;
        assert!(exists(&mut conn, &template).await);
    });
    busy_conn.send(()).unwrap();
    passing(leak_config(Config { project_root: "sweep-2", ..base }), "lifecycle::sweep_again");
    block_on(async {
        let mut conn = admin().await;
        assert!(!exists(&mut conn, &busy).await);
        let stale_run: bool = sqlx::query_scalar(&format!(
            r#"SELECT EXISTS (SELECT 1 FROM "{b}".runs WHERE run = 'stale')"#
        ))
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert!(!stale_run);
        assert!(!exists(&mut conn, &current).await);
        drop_all(&mut conn, &prefix).await;
        execute(&mut conn, &format!(r#"DROP SCHEMA "{b}" CASCADE"#)).await;
    });
}

fn hold_connection(database: &str) -> std::sync::mpsc::Sender<()> {
    let (release, released) = std::sync::mpsc::channel();
    let (connected, is_connected) = std::sync::mpsc::channel();
    let database = database.to_owned();
    std::thread::spawn(move || {
        block_on(async move {
            let url = std::env::var("DATABASE_URL").unwrap();
            let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
            let conn: PgConnection =
                sqlx::Connection::connect_with(&options.database(&database)).await.unwrap();
            connected.send(()).unwrap();
            released.recv().unwrap();
            sqlx::Connection::close(conn).await.unwrap();
        });
    });
    is_connected.recv().unwrap();
    release
}

async fn current_template(conn: &mut PgConnection, prefix: &str) -> String {
    with_prefix(conn, &format!("{prefix}_template_")).await.remove(0)
}
