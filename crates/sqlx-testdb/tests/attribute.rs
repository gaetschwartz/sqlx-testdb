mod common;

use sqlx::migrate::Migrator;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool, Postgres};
use sqlx_testdb::{Config, Schema};

use common::current_database;

static MIGRATOR: Migrator = sqlx::migrate!("tests/migrations");

fn custom() -> Config {
    Config {
        prefix: "sqlxt_custom".to_owned(),
        bookkeeping_schema: common::BOOKKEEPING.to_owned(),
        schema: Schema::Migrator(&MIGRATOR),
        ..Config::default()
    }
}

async fn tables(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

async fn widget_names(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM widgets ORDER BY id").fetch_all(conn).await.unwrap()
}

#[sqlx_testdb::test]
async fn a_pool_reaches_a_fresh_database_built_from_the_configured_sql_files(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("sqlxt_it_"));
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(tables(&mut conn).await, ["widget_tags", "widgets"]);
    assert!(widget_names(&mut conn).await.is_empty());
}

#[sqlx_testdb::test]
async fn a_pool_connection_is_accepted(mut conn: PoolConnection<Postgres>) {
    assert_eq!(tables(&mut conn).await, ["widget_tags", "widgets"]);
}

#[sqlx_testdb::test]
async fn an_owned_connection_is_accepted(mut conn: PgConnection) {
    sqlx::query("INSERT INTO widgets (name) VALUES ('solo')").execute(&mut conn).await.unwrap();
    assert_eq!(widget_names(&mut conn).await, ["solo"]);
    conn.close().await.unwrap();
}

#[sqlx_testdb::test]
async fn pool_options_and_connect_options_are_accepted(
    pool_options: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = pool_options.connect_with(options).await.unwrap();
    assert!(current_database(&pool).await.starts_with("sqlxt_it_"));
    assert_eq!(pool.options().get_max_connections(), 2);
}

#[sqlx_testdb::test]
async fn connect_options_alone_are_accepted(options: PgConnectOptions) {
    let mut conn = PgConnection::connect_with(&options).await.unwrap();
    assert_eq!(tables(&mut conn).await, ["widget_tags", "widgets"]);
}

#[sqlx_testdb::test]
async fn a_test_may_return_a_result(pool: PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(&pool).await?;
    Ok(())
}

#[sqlx_testdb::test(fixtures("widgets"))]
async fn a_named_fixture_is_applied_after_cloning(mut conn: PgConnection) {
    assert_eq!(widget_names(&mut conn).await, ["alpha", "beta"]);
}

#[sqlx_testdb::test(fixtures(path = "fixtures", scripts("widgets", "more_widgets")))]
async fn fixtures_from_a_directory_apply_in_order(mut conn: PgConnection) {
    assert_eq!(widget_names(&mut conn).await, ["alpha", "beta", "gamma"]);
}

#[sqlx_testdb::test(fixtures("fixtures/more_widgets.sql"))]
async fn a_fixture_path_with_an_extension_is_taken_as_is(mut conn: PgConnection) {
    assert_eq!(widget_names(&mut conn).await, ["gamma"]);
}

#[sqlx_testdb::test(migrator = "MIGRATOR")]
async fn a_migrator_replaces_the_configured_schema(mut conn: PgConnection) {
    assert_eq!(tables(&mut conn).await, ["_sqlx_migrations", "gadgets"]);
}

#[sqlx_testdb::test(migrations = "tests/migrations")]
async fn a_migrations_directory_replaces_the_configured_schema(mut conn: PgConnection) {
    assert_eq!(tables(&mut conn).await, ["_sqlx_migrations", "gadgets"]);
}

#[sqlx_testdb::test(migrations = false)]
async fn no_schema_leaves_the_database_empty(mut conn: PgConnection) {
    assert!(tables(&mut conn).await.is_empty());
}

#[sqlx_testdb::test(config = "custom")]
async fn a_config_item_replaces_the_config_file(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("sqlxt_custom_"));
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(tables(&mut conn).await, ["_sqlx_migrations", "gadgets"]);
}

#[sqlx_testdb::test]
async fn url_of_names_the_test_database(pool: PgPool) {
    let url = sqlx_testdb::url_of(&pool);
    let mut conn = PgConnection::connect(&url).await.unwrap();
    let name: String =
        sqlx::query_scalar("SELECT current_database()").fetch_one(&mut conn).await.unwrap();
    assert_eq!(name, current_database(&pool).await);
}
