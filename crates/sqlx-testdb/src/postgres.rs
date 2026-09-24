use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, Executor, PgConnection, Postgres};

use crate::args::TestArgs;
use crate::backend::{Backend, DropMode, LockKey};
use crate::error::Error;
use crate::harness::TestDb;

const OBJECT_IN_USE: &str = "55006";
const INVALID_CATALOG_NAME: &str = "3D000";

impl Backend for Postgres {
    const MAX_IDENTIFIER_BYTES: usize = 63;

    fn quote_identifier(name: &str) -> String {
        let mut quoted = String::with_capacity(name.len() + 2);
        quoted.push('"');
        for c in name.chars() {
            if c == '"' {
                quoted.push('"');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted
    }

    fn parse_options(url: &str) -> Result<PgConnectOptions, sqlx::Error> {
        PgConnectOptions::from_str(url)
    }

    fn options_for_database(base: &PgConnectOptions, name: &str) -> PgConnectOptions {
        base.clone().database(name)
    }

    fn is_in_use(error: &sqlx::Error) -> bool {
        has_code(error, OBJECT_IN_USE)
    }

    fn is_missing(error: &sqlx::Error) -> bool {
        has_code(error, INVALID_CATALOG_NAME)
    }

    async fn lock(conn: &mut PgConnection, key: LockKey) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_lock($1)").bind(advisory(key)).execute(conn).await?;
        Ok(())
    }

    async fn try_lock(conn: &mut PgConnection, key: LockKey) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(advisory(key))
            .fetch_one(conn)
            .await
    }

    async fn unlock(conn: &mut PgConnection, key: LockKey) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_unlock($1)").bind(advisory(key)).execute(conn).await?;
        Ok(())
    }

    async fn database_exists(conn: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(name)
            .fetch_one(conn)
            .await
    }

    async fn databases_with_prefix(
        conn: &mut PgConnection,
        prefix: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE left(datname, length($1)) = $1")
            .bind(prefix)
            .fetch_all(conn)
            .await
    }

    async fn create_database(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
        let name = Self::quote_identifier(name);
        conn.execute(format!("CREATE DATABASE {name}").as_str()).await?;
        Ok(())
    }

    async fn clone_database(
        conn: &mut PgConnection,
        name: &str,
        template: &str,
    ) -> Result<(), sqlx::Error> {
        let (name, template) = (Self::quote_identifier(name), Self::quote_identifier(template));
        conn.execute(format!("CREATE DATABASE {name} TEMPLATE {template}").as_str()).await?;
        Ok(())
    }

    async fn drop_database(
        conn: &mut PgConnection,
        name: &str,
        mode: DropMode,
    ) -> Result<(), sqlx::Error> {
        let name = Self::quote_identifier(name);
        let statement = match mode {
            DropMode::Force => format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
            DropMode::IfIdle => format!("DROP DATABASE IF EXISTS {name}"),
        };
        conn.execute(statement.as_str()).await?;
        Ok(())
    }

    async fn publish_template(
        conn: &mut PgConnection,
        building: &str,
        template: &str,
    ) -> Result<(), sqlx::Error> {
        let (building, template) =
            (Self::quote_identifier(building), Self::quote_identifier(template));
        let publish = format!(
            "ALTER DATABASE {building} WITH IS_TEMPLATE true ALLOW_CONNECTIONS false;
             ALTER DATABASE {building} RENAME TO {template}"
        );
        sqlx::raw_sql(&publish).execute(conn).await?;
        Ok(())
    }

    async fn retire_template(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
        let name = Self::quote_identifier(name);
        let retire = format!("ALTER DATABASE {name} WITH IS_TEMPLATE false ALLOW_CONNECTIONS true");
        conn.execute(retire.as_str()).await?;
        Ok(())
    }

    async fn apply_sql(conn: &mut PgConnection, sql: &str) -> Result<(), sqlx::Error> {
        sqlx::raw_sql(sql).execute(conn).await?;
        Ok(())
    }

    async fn apply_migrator(
        conn: &mut PgConnection,
        migrator: &Migrator,
    ) -> Result<(), MigrateError> {
        migrator.run_direct(conn).await
    }

    async fn bootstrap(
        conn: &mut PgConnection,
        schema: &str,
        key: LockKey,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let ddl = format!(
            "CREATE SCHEMA IF NOT EXISTS {s};
             CREATE TABLE IF NOT EXISTS {s}.runs (
                 run TEXT PRIMARY KEY,
                 project_root TEXT NOT NULL,
                 last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
             );
             CREATE TABLE IF NOT EXISTS {s}.databases (
                 name TEXT PRIMARY KEY,
                 test_path TEXT NOT NULL,
                 run TEXT NOT NULL,
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                 failed_at TIMESTAMPTZ
             );
             CREATE TABLE IF NOT EXISTS {s}.templates (
                 name TEXT PRIMARY KEY,
                 last_used_at TIMESTAMPTZ NOT NULL DEFAULT now()
             );"
        );
        let mut tx = conn.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory(key))
            .execute(&mut *tx)
            .await?;
        sqlx::raw_sql(&ddl).execute(&mut *tx).await?;
        tx.commit().await
    }

    async fn register_run(
        conn: &mut PgConnection,
        schema: &str,
        run: &str,
        project_root: &str,
    ) -> Result<bool, sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!(
            "INSERT INTO {s}.runs (run, project_root) VALUES ($1, $2)
             ON CONFLICT (run) DO UPDATE SET last_seen_at = now()
             RETURNING xmax = 0"
        );
        sqlx::query_scalar(&sql).bind(run).bind(project_root).fetch_one(conn).await
    }

    async fn register_database(
        conn: &mut PgConnection,
        schema: &str,
        name: &str,
        test_path: &str,
        run: &str,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!("INSERT INTO {s}.databases (name, test_path, run) VALUES ($1, $2, $3)");
        sqlx::query(&sql).bind(name).bind(test_path).bind(run).execute(conn).await?;
        Ok(())
    }

    async fn mark_failed(
        conn: &mut PgConnection,
        schema: &str,
        name: &str,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!("UPDATE {s}.databases SET failed_at = now() WHERE name = $1");
        sqlx::query(&sql).bind(name).execute(conn).await?;
        Ok(())
    }

    async fn forget_database(
        conn: &mut PgConnection,
        schema: &str,
        name: &str,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!("DELETE FROM {s}.databases WHERE name = $1");
        sqlx::query(&sql).bind(name).execute(conn).await?;
        Ok(())
    }

    async fn touch_template(
        conn: &mut PgConnection,
        schema: &str,
        name: &str,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!(
            "INSERT INTO {s}.templates (name) VALUES ($1)
             ON CONFLICT (name) DO UPDATE SET last_used_at = now()"
        );
        sqlx::query(&sql).bind(name).execute(conn).await?;
        Ok(())
    }

    async fn forget_template(
        conn: &mut PgConnection,
        schema: &str,
        name: &str,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!("DELETE FROM {s}.templates WHERE name = $1");
        sqlx::query(&sql).bind(name).execute(conn).await?;
        Ok(())
    }

    async fn leftover_databases(
        conn: &mut PgConnection,
        schema: &str,
        stale_run_after: Duration,
        kept_failed_for: Duration,
    ) -> Result<Vec<String>, sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!(
            "SELECT d.name FROM {s}.databases d
             LEFT JOIN {s}.runs r ON r.run = d.run
             WHERE (d.failed_at IS NULL
                    AND (r.last_seen_at IS NULL
                         OR r.last_seen_at < now() - make_interval(secs => $1)))
                OR d.failed_at < now() - make_interval(secs => $2)"
        );
        sqlx::query_scalar(&sql)
            .bind(stale_run_after.as_secs_f64())
            .bind(kept_failed_for.as_secs_f64())
            .fetch_all(conn)
            .await
    }

    async fn forget_stale_runs(
        conn: &mut PgConnection,
        schema: &str,
        stale_run_after: Duration,
    ) -> Result<(), sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!(
            "DELETE FROM {s}.runs r
             WHERE r.last_seen_at < now() - make_interval(secs => $1)
               AND NOT EXISTS (SELECT 1 FROM {s}.databases d WHERE d.run = r.run)"
        );
        sqlx::query(&sql).bind(stale_run_after.as_secs_f64()).execute(conn).await?;
        Ok(())
    }

    async fn idle_templates(
        conn: &mut PgConnection,
        schema: &str,
        prefix: &str,
        current: &str,
        idle_after: Duration,
    ) -> Result<Vec<String>, sqlx::Error> {
        let s = Self::quote_identifier(schema);
        let sql = format!(
            "SELECT datname FROM pg_database
             WHERE left(datname, length($1)) = $1 AND datname <> $2
               AND NOT EXISTS (SELECT 1 FROM {s}.templates t
                               WHERE t.name = datname
                                 AND t.last_used_at >= now() - make_interval(secs => $3))"
        );
        sqlx::query_scalar(&sql)
            .bind(prefix)
            .bind(current)
            .bind(idle_after.as_secs_f64())
            .fetch_all(conn)
            .await
    }
}

impl TestArgs<Postgres> for PgConnection {
    async fn make(db: &TestDb<Postgres>) -> Result<Self, Error> {
        db.connect().await
    }
}

impl TestArgs<Postgres> for PgConnectOptions {
    fn make(db: &TestDb<Postgres>) -> impl Future<Output = Result<Self, Error>> {
        std::future::ready(Ok(db.connect_options().clone()))
    }
}

const fn advisory(key: LockKey) -> i64 {
    i64::from_be_bytes(key.bytes())
}

fn has_code(error: &sqlx::Error, code: &str) -> bool {
    error.as_database_error().and_then(sqlx::error::DatabaseError::code).is_some_and(|c| c == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_doubles_embedded_quotes() {
        assert_eq!(Postgres::quote_identifier("plain"), r#""plain""#);
        assert_eq!(Postgres::quote_identifier(r#"a"b"#), r#""a""b""#);
    }
}
