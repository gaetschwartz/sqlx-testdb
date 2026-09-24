use std::sync::OnceLock;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use snafu::ResultExt;
use sqlx::migrate::Migrator;

use crate::error::{
    ConflictingSchemaSnafu, DatabaseUrlSnafu, Error, MissingDatabaseUrlSnafu, ParseConfigSnafu,
    ReadConfigSnafu,
};

pub const CONFIG_FILE: &str = "sqlx-testdb.toml";
pub const CONFIG_PATH_VAR: &str = "SQLX_TESTDB_CONFIG";
const SECS_PER_MIN: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MIN;

#[derive(Debug, Clone)]
pub struct Config {
    pub schema: Schema,
    pub project_root: Utf8PathBuf,
    pub source: Option<Utf8PathBuf>,
    pub database_url_var: String,
    pub database_url: Option<String>,
    pub prefix: String,
    pub bookkeeping_schema: String,
    pub keep_failed: bool,
    pub pool: PoolSettings,
    pub sweep: SweepSettings,
}

#[derive(Debug, Clone)]
pub enum Schema {
    None,
    Sql(Vec<Utf8PathBuf>),
    Migrations(Utf8PathBuf),
    Migrator(&'static Migrator),
}

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

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: Schema::None,
            project_root: Utf8PathBuf::new(),
            source: None,
            database_url_var: "DATABASE_URL".to_owned(),
            database_url: None,
            prefix: "sqlxt".to_owned(),
            bookkeeping_schema: "sqlx_testdb".to_owned(),
            keep_failed: true,
            pool: PoolSettings::DEFAULT,
            sweep: SweepSettings::DEFAULT,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Settings {
    prefix: Option<String>,
    bookkeeping_schema: Option<String>,
    database_url_var: Option<String>,
    database_url: Option<String>,
    keep_failed: Option<bool>,
    #[serde(default)]
    schema: SchemaFile,
    #[serde(default)]
    pool: PoolFile,
    #[serde(default)]
    sweep: SweepFile,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SchemaFile {
    #[serde(default)]
    sql: Vec<Utf8PathBuf>,
    migrations: Option<Utf8PathBuf>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PoolFile {
    max_connections: Option<u32>,
    idle_timeout_secs: Option<u64>,
    acquire_timeout_secs: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SweepFile {
    stale_run_mins: Option<u64>,
    kept_failed_hours: Option<u64>,
    idle_template_hours: Option<u64>,
}

fn duration(amount: Option<u64>, unit_secs: u64, default: Duration) -> Duration {
    amount.map_or(default, |amount| Duration::from_secs(amount.saturating_mul(unit_secs)))
}

impl Config {
    /// Read once per process: the file `SQLX_TESTDB_CONFIG` names, else the nearest
    /// `sqlx-testdb.toml` at or above `manifest_dir`, else the defaults.
    pub fn discover(manifest_dir: &Utf8Path) -> Result<&'static Self, &'static Error> {
        static DISCOVERED: OnceLock<Result<Config, Error>> = OnceLock::new();
        DISCOVERED
            .get_or_init(|| {
                let explicit = std::env::var(CONFIG_PATH_VAR).ok().map(Utf8PathBuf::from);
                let found = explicit.or_else(|| {
                    manifest_dir
                        .ancestors()
                        .map(|dir| dir.join(CONFIG_FILE))
                        .find(|path| path.is_file())
                });
                found.map_or_else(
                    || Ok(Self { project_root: manifest_dir.to_owned(), ..Self::default() }),
                    |path| Self::from_file(&path),
                )
            })
            .as_ref()
    }

    pub fn from_file(path: &Utf8Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path).context(ReadConfigSnafu { path })?;
        let settings: Settings = toml::from_str(&text).context(ParseConfigSnafu { path })?;
        let root = path.parent().map_or_else(Utf8PathBuf::new, Utf8Path::to_path_buf);
        let schema = match settings.schema {
            SchemaFile { sql, migrations: Some(_) } if !sql.is_empty() => {
                return ConflictingSchemaSnafu { path }.fail();
            }
            SchemaFile { migrations: Some(dir), .. } => Schema::Migrations(root.join(dir)),
            SchemaFile { sql, .. } if sql.is_empty() => Schema::None,
            SchemaFile { sql, .. } => {
                Schema::Sql(sql.into_iter().map(|file| root.join(file)).collect())
            }
        };
        let defaults = Self::default();
        let (pool, sweep) = (settings.pool, settings.sweep);
        let (pool_defaults, sweep_defaults) = (PoolSettings::DEFAULT, SweepSettings::DEFAULT);
        Ok(Self {
            schema,
            project_root: root,
            source: Some(path.to_owned()),
            database_url_var: settings.database_url_var.unwrap_or(defaults.database_url_var),
            database_url: settings.database_url,
            prefix: settings.prefix.unwrap_or(defaults.prefix),
            bookkeeping_schema: settings.bookkeeping_schema.unwrap_or(defaults.bookkeeping_schema),
            keep_failed: settings.keep_failed.unwrap_or(defaults.keep_failed),
            pool: PoolSettings {
                max_connections: pool.max_connections.unwrap_or(pool_defaults.max_connections),
                idle_timeout: duration(pool.idle_timeout_secs, 1, pool_defaults.idle_timeout),
                acquire_timeout: duration(
                    pool.acquire_timeout_secs,
                    1,
                    pool_defaults.acquire_timeout,
                ),
            },
            sweep: SweepSettings {
                stale_run_after: duration(
                    sweep.stale_run_mins,
                    SECS_PER_MIN,
                    sweep_defaults.stale_run_after,
                ),
                kept_failed_for: duration(
                    sweep.kept_failed_hours,
                    SECS_PER_HOUR,
                    sweep_defaults.kept_failed_for,
                ),
                idle_template_after: duration(
                    sweep.idle_template_hours,
                    SECS_PER_HOUR,
                    sweep_defaults.idle_template_after,
                ),
            },
        })
    }

    pub fn database_url(&self) -> Result<String, Error> {
        self.database_url_from(std::env::var(&self.database_url_var))
    }

    fn database_url_from(&self, env: Result<String, std::env::VarError>) -> Result<String, Error> {
        match env {
            Ok(url) => Ok(url),
            Err(std::env::VarError::NotPresent) => self.database_url.clone().ok_or_else(|| {
                let config = self.source.as_ref().map_or_else(
                    || "no sqlx-testdb.toml was found".to_owned(),
                    |path| format!("{path} sets no `database_url`"),
                );
                MissingDatabaseUrlSnafu { var: self.database_url_var.as_str(), config }.build()
            }),
            Err(source) => Err(source).context(DatabaseUrlSnafu { var: &self.database_url_var }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env::VarError;

    use super::*;

    const ENV_URL: &str = "postgres://env@server/db";
    const FILE_URL: &str = "postgres://file@server/db";

    fn with_file_url() -> Config {
        Config {
            database_url: Some(FILE_URL.to_owned()),
            source: Some(Utf8PathBuf::from("/p/sqlx-testdb.toml")),
            ..Config::default()
        }
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let url = with_file_url().database_url_from(Ok(ENV_URL.to_owned())).unwrap();
        assert_eq!(url, ENV_URL);
    }

    #[test]
    fn the_file_is_the_fallback() {
        let url = with_file_url().database_url_from(Err(VarError::NotPresent)).unwrap();
        assert_eq!(url, FILE_URL);
    }

    #[test]
    fn neither_names_both_sources() {
        let config = Config { database_url: None, ..with_file_url() };
        let error = config.database_url_from(Err(VarError::NotPresent)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "no database server: DATABASE_URL is not set and /p/sqlx-testdb.toml sets no \
             `database_url`; set one of them to the server the tests may use"
        );
        let error = Config::default().database_url_from(Err(VarError::NotPresent)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "no database server: DATABASE_URL is not set and no sqlx-testdb.toml was found; set \
             one of them to the server the tests may use"
        );
    }
}
