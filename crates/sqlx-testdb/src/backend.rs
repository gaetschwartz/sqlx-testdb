use std::fmt;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{Connection, Database};

pub type ConnectOptionsOf<DB> = <<DB as Database>::Connection as Connection>::Options;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropMode {
    Force,
    IfIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectFailure {
    /// Nothing listens there yet; worth retrying briefly in case the server is restarting.
    Refused,
    Unreachable,
    Rejected,
    /// The server is up but cannot take a connection right now.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPurpose {
    Bootstrap,
    Sweep,
    Template,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LockKey([u8; 8]);

impl LockPurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Sweep => "sweep",
            Self::Template => "template",
        }
    }
}

impl LockKey {
    pub fn new(purpose: LockPurpose, scope: &[&str]) -> Self {
        let mut hasher = Sha256::new_with_prefix(purpose.as_str().as_bytes());
        for part in scope {
            hasher.update([0]);
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let mut key = [0; 8];
        let len = key.len();
        key.copy_from_slice(&digest[..len]);
        Self(key)
    }

    pub const fn bytes(self) -> [u8; 8] {
        self.0
    }
}

impl fmt::Display for LockKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

pub trait Backend: Database {
    const MAX_IDENTIFIER_BYTES: usize;

    fn quote_identifier(name: &str) -> String;

    fn parse_options(url: &str) -> Result<ConnectOptionsOf<Self>, sqlx::Error>;

    fn options_for_database(base: &ConnectOptionsOf<Self>, name: &str) -> ConnectOptionsOf<Self>;

    fn is_in_use(error: &sqlx::Error) -> bool;

    fn is_missing(error: &sqlx::Error) -> bool;

    fn classify_connect_error(error: &sqlx::Error) -> ConnectFailure;

    fn lock(
        conn: &mut Self::Connection,
        key: LockKey,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn try_lock(
        conn: &mut Self::Connection,
        key: LockKey,
    ) -> impl Future<Output = Result<bool, sqlx::Error>>;

    fn unlock(
        conn: &mut Self::Connection,
        key: LockKey,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn database_exists(
        conn: &mut Self::Connection,
        name: &str,
    ) -> impl Future<Output = Result<bool, sqlx::Error>>;

    fn databases_with_prefix(
        conn: &mut Self::Connection,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<String>, sqlx::Error>>;

    fn create_database(
        conn: &mut Self::Connection,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn clone_database(
        conn: &mut Self::Connection,
        name: &str,
        template: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn drop_database(
        conn: &mut Self::Connection,
        name: &str,
        mode: DropMode,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn publish_template(
        conn: &mut Self::Connection,
        building: &str,
        template: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn retire_template(
        conn: &mut Self::Connection,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn apply_sql(
        conn: &mut Self::Connection,
        sql: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn apply_migrator(
        conn: &mut Self::Connection,
        migrator: &Migrator,
    ) -> impl Future<Output = Result<(), MigrateError>>;

    fn bootstrap(
        conn: &mut Self::Connection,
        schema: &str,
        key: LockKey,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    /// Records the run as seen now; `true` when this call created it.
    fn register_run(
        conn: &mut Self::Connection,
        schema: &str,
        run: &str,
        project_root: &str,
    ) -> impl Future<Output = Result<bool, sqlx::Error>>;

    fn register_database(
        conn: &mut Self::Connection,
        schema: &str,
        name: &str,
        test_path: &str,
        run: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn mark_failed(
        conn: &mut Self::Connection,
        schema: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn forget_database(
        conn: &mut Self::Connection,
        schema: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn touch_template(
        conn: &mut Self::Connection,
        schema: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    fn forget_template(
        conn: &mut Self::Connection,
        schema: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    /// Databases of runs unseen for `stale_run_after`, and failed ones kept longer than
    /// `kept_failed_for`.
    fn leftover_databases(
        conn: &mut Self::Connection,
        schema: &str,
        stale_run_after: Duration,
        kept_failed_for: Duration,
    ) -> impl Future<Output = Result<Vec<String>, sqlx::Error>>;

    /// Forgets runs unseen for `stale_run_after` that no longer own a database.
    fn forget_stale_runs(
        conn: &mut Self::Connection,
        schema: &str,
        stale_run_after: Duration,
    ) -> impl Future<Output = Result<(), sqlx::Error>>;

    /// Templates named with `prefix`, other than `current`, unused for `idle_after`.
    fn idle_templates(
        conn: &mut Self::Connection,
        schema: &str,
        prefix: &str,
        current: &str,
        idle_after: Duration,
    ) -> impl Future<Output = Result<Vec<String>, sqlx::Error>>;
}
