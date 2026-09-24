use camino::Utf8PathBuf;
use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display(
        "no database server: {var} is not set and {config}; set one of them to the server the \
         tests may use"
    ))]
    MissingDatabaseUrl { var: String, config: String },
    #[snafu(display("reading {var}"))]
    DatabaseUrl { var: String, source: std::env::VarError },
    #[snafu(display("the database URL is not a connection string this backend accepts"))]
    ConnectOptions { source: sqlx::Error },
    #[snafu(display("reading {path}"))]
    ReadConfig { path: Utf8PathBuf, source: std::io::Error },
    #[snafu(display("parsing {path}"))]
    ParseConfig { path: Utf8PathBuf, source: toml::de::Error },
    #[snafu(display("{path}: set `schema.sql` or `schema.migrations`, not both"))]
    ConflictingSchema { path: Utf8PathBuf },
    #[snafu(display("reading schema file {path}"))]
    ReadSchema { path: Utf8PathBuf, source: std::io::Error },
    #[snafu(display("reading fixture {path}"))]
    ReadFixture { path: Utf8PathBuf, source: std::io::Error },
    #[snafu(display("{what} {name:?} is not a lowercase identifier ([a-z][a-z0-9_]*)"))]
    InvalidName { what: &'static str, name: String },
    #[snafu(display("{what} {name:?} makes {len}-byte names; the backend allows {max}"))]
    NameTooLong { what: &'static str, name: String, len: usize, max: usize },
    #[snafu(display("building the test runtime"))]
    Runtime { source: std::io::Error },
    #[snafu(display("connecting to the test server"))]
    Connect { source: sqlx::Error },
    #[snafu(display("updating the test database bookkeeping"))]
    Bookkeeping { source: sqlx::Error },
    #[snafu(display("taking or releasing lock {key}"))]
    Lock { key: crate::LockKey, source: sqlx::Error },
    #[snafu(display("loading migrations from {path}"))]
    LoadMigrations { path: Utf8PathBuf, source: sqlx::migrate::MigrateError },
    #[snafu(display("applying the schema to {name}"))]
    ApplySchema { name: String, source: sqlx::Error },
    #[snafu(display("migrating {name}"))]
    Migrate { name: String, source: sqlx::migrate::MigrateError },
    #[snafu(display("applying fixture {path} to {name}"))]
    ApplyFixture { path: Utf8PathBuf, name: String, source: sqlx::Error },
    #[snafu(display("marking or unmarking template {name}"))]
    AlterTemplate { name: String, source: sqlx::Error },
    #[snafu(display("creating database {name}"))]
    CreateDatabase { name: String, source: sqlx::Error },
    #[snafu(display("dropping database {name}"))]
    DropDatabase { name: String, source: sqlx::Error },
}
