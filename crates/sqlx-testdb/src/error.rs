use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("{var} is not set; point it at the database server the tests may use"))]
    MissingDatabaseUrl { var: &'static str },
    #[snafu(display("reading {var}"))]
    DatabaseUrl { var: &'static str, source: std::env::VarError },
    #[snafu(display("{var} is not a connection string this backend accepts"))]
    ConnectOptions { var: &'static str, source: sqlx::Error },
    #[snafu(display("{what} {name:?} is not a lowercase identifier ([a-z][a-z0-9_]*)"))]
    InvalidName { what: &'static str, name: &'static str },
    #[snafu(display("{what} {name:?} makes {len}-byte names; the backend allows {max}"))]
    NameTooLong { what: &'static str, name: &'static str, len: usize, max: usize },
    #[snafu(display("building the test runtime"))]
    Runtime { source: std::io::Error },
    #[snafu(display("connecting to the test server"))]
    Connect { source: sqlx::Error },
    #[snafu(display("updating the test database bookkeeping"))]
    Bookkeeping { source: sqlx::Error },
    #[snafu(display("taking or releasing lock {key}"))]
    Lock { key: crate::LockKey, source: sqlx::Error },
    #[snafu(display("loading migrations from {path}"))]
    LoadMigrations { path: &'static str, source: sqlx::migrate::MigrateError },
    #[snafu(display("applying the schema to {name}"))]
    ApplySchema { name: String, source: sqlx::Error },
    #[snafu(display("migrating {name}"))]
    Migrate { name: String, source: sqlx::migrate::MigrateError },
    #[snafu(display("applying fixture {path} to {name}"))]
    ApplyFixture { path: &'static str, name: String, source: sqlx::Error },
    #[snafu(display("marking or unmarking template {name}"))]
    AlterTemplate { name: String, source: sqlx::Error },
    #[snafu(display("creating database {name}"))]
    CreateDatabase { name: String, source: sqlx::Error },
    #[snafu(display("dropping database {name}"))]
    DropDatabase { name: String, source: sqlx::Error },
}
