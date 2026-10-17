mod common;

use std::time::Duration;

use sqlx::PgConnection;
use sqlx_testdb::{Backend, LockKey, LockPurpose, Registration};

use common::{admin, block_on, execute, token};

const HOUR: Duration = Duration::from_secs(3600);
const BLOCKED_FOR: Duration = Duration::from_millis(500);
const WELL_UNDER_STUCK: Duration = Duration::from_secs(10);

fn unique_schema() -> String {
    format!("sqlx_testdb_bk_{}", token())
}

fn bootstrap_key(schema: &str) -> LockKey {
    LockKey::new(LockPurpose::Bootstrap, &[schema])
}

async fn bootstrapped(schema: &str) -> PgConnection {
    let mut conn = admin().await;
    sqlx::Postgres::bootstrap(&mut conn, schema, bootstrap_key(schema)).await.unwrap();
    conn
}

async fn drop_schema(conn: &mut PgConnection, schema: &str) {
    execute(conn, &format!(r#"DROP SCHEMA "{schema}" CASCADE"#)).await;
}

async fn register(
    conn: &mut PgConnection,
    schema: &str,
    run: &str,
    name: &str,
    refresh_after: Duration,
) -> bool {
    let registration = Registration { run, project_root: "root", name, test_path: "t" };
    sqlx::Postgres::register_test(conn, schema, &registration, refresh_after).await.unwrap()
}

async fn text(conn: &mut PgConnection, sql: String) -> String {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_one(conn).await.unwrap()
}

async fn run_seen_at(conn: &mut PgConnection, schema: &str, run: &str) -> String {
    text(conn, format!(r#"SELECT last_seen_at::text FROM "{schema}".runs WHERE run = '{run}'"#))
        .await
}

async fn template_used_at(conn: &mut PgConnection, schema: &str, name: &str) -> String {
    text(
        conn,
        format!(r#"SELECT last_used_at::text FROM "{schema}".templates WHERE name = '{name}'"#),
    )
    .await
}

#[test]
fn only_the_call_that_creates_a_run_reports_it_as_the_first() {
    let schema = unique_schema();
    block_on(async {
        let mut conn = bootstrapped(&schema).await;
        assert!(register(&mut conn, &schema, "run_a", "db_1", HOUR).await);
        assert!(!register(&mut conn, &schema, "run_a", "db_2", HOUR).await);
        assert!(!register(&mut conn, &schema, "run_a", "db_3", Duration::ZERO).await);
        assert!(register(&mut conn, &schema, "run_b", "db_4", Duration::ZERO).await);
        let registered = text(
            &mut conn,
            format!(r#"SELECT string_agg(name || ':' || run, ',' ORDER BY name) FROM "{schema}".databases"#),
        )
        .await;
        assert_eq!(registered, "db_1:run_a,db_2:run_a,db_3:run_a,db_4:run_b");
        drop_schema(&mut conn, &schema).await;
    });
}

#[test]
fn a_run_seen_recently_is_not_rewritten_and_an_old_one_is_refreshed() {
    let schema = unique_schema();
    block_on(async {
        let mut conn = bootstrapped(&schema).await;
        register(&mut conn, &schema, "run_a", "db_1", HOUR).await;
        let created = run_seen_at(&mut conn, &schema, "run_a").await;
        register(&mut conn, &schema, "run_a", "db_2", HOUR).await;
        assert_eq!(run_seen_at(&mut conn, &schema, "run_a").await, created);
        execute(
            &mut conn,
            &format!(r#"UPDATE "{schema}".runs SET last_seen_at = now() - interval '2 hours'"#),
        )
        .await;
        let aged = run_seen_at(&mut conn, &schema, "run_a").await;
        assert!(!register(&mut conn, &schema, "run_a", "db_3", HOUR).await);
        let refreshed = text(
            &mut conn,
            format!(
                r#"SELECT (last_seen_at > now() - interval '1 minute')::text FROM "{schema}".runs"#
            ),
        )
        .await;
        assert_eq!(refreshed, "true");
        assert_ne!(run_seen_at(&mut conn, &schema, "run_a").await, aged);
        drop_schema(&mut conn, &schema).await;
    });
}

#[test]
fn a_template_used_recently_is_not_rewritten_and_an_old_one_is_refreshed() {
    let schema = unique_schema();
    block_on(async {
        let mut conn = bootstrapped(&schema).await;
        sqlx::Postgres::touch_template(&mut conn, &schema, "tpl", HOUR).await.unwrap();
        let created = template_used_at(&mut conn, &schema, "tpl").await;
        sqlx::Postgres::touch_template(&mut conn, &schema, "tpl", HOUR).await.unwrap();
        assert_eq!(template_used_at(&mut conn, &schema, "tpl").await, created);
        execute(
            &mut conn,
            &format!(
                r#"UPDATE "{schema}".templates SET last_used_at = now() - interval '2 hours'"#
            ),
        )
        .await;
        sqlx::Postgres::touch_template(&mut conn, &schema, "tpl", HOUR).await.unwrap();
        let refreshed = text(
            &mut conn,
            format!(
                r#"SELECT (last_used_at > now() - interval '1 minute')::text FROM "{schema}".templates"#
            ),
        )
        .await;
        assert_eq!(refreshed, "true");
        drop_schema(&mut conn, &schema).await;
    });
}

#[test]
fn bootstrapping_complete_bookkeeping_takes_no_lock_and_missing_bookkeeping_waits_for_it() {
    let schema = unique_schema();
    block_on(async {
        let mut conn = bootstrapped(&schema).await;
        let key = bootstrap_key(&schema);
        let mut holder = admin().await;
        sqlx::Postgres::lock(&mut holder, key).await.unwrap();

        let mut again = admin().await;
        tokio::time::timeout(WELL_UNDER_STUCK, sqlx::Postgres::bootstrap(&mut again, &schema, key))
            .await
            .expect("the fast path waited for the lock")
            .unwrap();

        execute(&mut conn, &format!(r#"DROP TABLE "{schema}".templates"#)).await;
        let mut missing = admin().await;
        let blocked = tokio::time::timeout(
            BLOCKED_FOR,
            sqlx::Postgres::bootstrap(&mut missing, &schema, key),
        )
        .await;
        assert!(blocked.is_err(), "bootstrap must wait for the lock when a table is missing");
        drop(missing);

        sqlx::Postgres::unlock(&mut holder, key).await.unwrap();
        let mut repaired = admin().await;
        sqlx::Postgres::bootstrap(&mut repaired, &schema, key).await.unwrap();
        sqlx::Postgres::touch_template(&mut repaired, &schema, "tpl", HOUR).await.unwrap();
        drop_schema(&mut conn, &schema).await;
    });
}
