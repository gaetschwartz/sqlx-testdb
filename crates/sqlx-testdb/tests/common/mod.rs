#![allow(dead_code, reason = "each test binary uses a different subset of these helpers")]

use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use sqlx::{Connection, PgConnection, PgPool};
use sqlx_testdb::{Config, Schema};

pub const BOOKKEEPING: &str = "sqlx_testdb_it";
const DATABASE_URL_VAR: &str = "DATABASE_URL";
const TOKEN_CHARS: usize = 8;

pub fn token() -> String {
    const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; TOKEN_CHARS];
    rand::fill(&mut bytes);
    bytes.iter().map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()])).collect()
}

pub struct Scratch(Utf8PathBuf);

impl Scratch {
    pub fn new() -> Self {
        let dir = Utf8PathBuf::from_path_buf(std::env::temp_dir())
            .unwrap()
            .join(format!("sqlx-testdb-{}", token()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Utf8Path {
        &self.0
    }

    pub fn write(&self, name: &str, contents: &str) -> Utf8PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn config(scratch: &Scratch, prefix: &str, bookkeeping: &str, root: &str, sql: &str) -> Config {
    let schema = scratch.write(&format!("{}.sql", token()), sql);
    Config {
        schema: Schema::Sql(vec![schema]),
        project_root: Utf8PathBuf::from(root),
        prefix: prefix.to_owned(),
        bookkeeping_schema: bookkeeping.to_owned(),
        ..Config::default()
    }
}

pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}

pub async fn admin() -> PgConnection {
    PgConnection::connect(&std::env::var(DATABASE_URL_VAR).unwrap()).await.unwrap()
}

pub async fn current_database(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT current_database()").fetch_one(pool).await.unwrap()
}

pub async fn exists(conn: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(conn)
        .await
        .unwrap()
}

pub async fn with_prefix(conn: &mut PgConnection, prefix: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT datname FROM pg_database WHERE left(datname, length($1)) = $1 ORDER BY datname",
    )
    .bind(prefix)
    .fetch_all(conn)
    .await
    .unwrap()
}

pub async fn is_template(conn: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar("SELECT datistemplate FROM pg_database WHERE datname = $1")
        .bind(name)
        .fetch_one(conn)
        .await
        .unwrap()
}

pub async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::raw_sql(sql).execute(conn).await.unwrap();
}

pub async fn drop_all(conn: &mut PgConnection, prefix: &str) {
    for name in with_prefix(conn, &format!("{prefix}_")).await {
        execute(conn, &format!(r#"ALTER DATABASE "{name}" WITH IS_TEMPLATE false"#)).await;
        execute(conn, &format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#)).await;
    }
}

pub type Seen = Arc<Mutex<Vec<String>>>;

pub fn seen() -> Seen {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn record(seen: &Seen, name: String) {
    seen.lock().unwrap().push(name);
}

pub fn names(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap().clone()
}
