use std::time::Duration;

use sqlx::migrate::Migrator;

const SECS_PER_MIN: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MIN;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub schema: Schema,
    pub project_root: &'static str,
    pub database_url_var: &'static str,
    pub prefix: &'static str,
    pub bookkeeping_schema: &'static str,
    pub keep_failed: bool,
    pub pool: PoolSettings,
    pub sweep: SweepSettings,
}

#[derive(Debug, Clone, Copy)]
pub enum Schema {
    None,
    Sql(&'static [SqlSource]),
    Migrator(&'static Migrator),
    Migrations(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct SqlSource {
    pub path: &'static str,
    pub sql: &'static str,
}

pub type Fixture = SqlSource;

#[derive(Debug, Clone, Copy)]
pub struct PoolSettings {
    pub max_connections: u32,
    pub idle_timeout: Duration,
    pub acquire_timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct SweepSettings {
    pub stale_run_after: Duration,
    pub kept_failed_for: Duration,
    pub idle_template_after: Duration,
}

impl PoolSettings {
    pub const DEFAULT: Self = Self {
        max_connections: 4,
        idle_timeout: Duration::from_secs(1),
        acquire_timeout: Duration::from_secs(2 * SECS_PER_MIN),
    };
}

impl SweepSettings {
    pub const DEFAULT: Self = Self {
        stale_run_after: Duration::from_secs(SECS_PER_HOUR),
        kept_failed_for: Duration::from_secs(24 * SECS_PER_HOUR),
        idle_template_after: Duration::from_secs(24 * SECS_PER_HOUR),
    };
}

impl Config {
    pub const DEFAULT: Self = Self {
        schema: Schema::None,
        project_root: "",
        database_url_var: "DATABASE_URL",
        prefix: "sqlxt",
        bookkeeping_schema: "sqlx_testdb",
        keep_failed: true,
        pool: PoolSettings::DEFAULT,
        sweep: SweepSettings::DEFAULT,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}
