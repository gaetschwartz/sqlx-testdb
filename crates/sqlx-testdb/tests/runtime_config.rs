mod common;

use std::process::{Command, Output};

use sqlx::PgPool;

use common::{
    BOOKKEEPING, Scratch, admin, block_on, current_database, drop_all, token, with_prefix,
};

const CHILD_TEST: &str = "a_test_under_the_runtime_configuration";
const OUT_VAR: &str = "SQLX_TESTDB_CHILD_OUT";
const CONFIG_VAR: &str = "SQLX_TESTDB_CONFIG";
const URL_VAR: &str = "DATABASE_URL";
const UNREACHABLE_URL: &str = "postgres://nobody:nothing@127.0.0.1:1/nowhere";

#[sqlx_testdb::test]
#[ignore = "spawned by the runtime configuration tests"]
async fn a_test_under_the_runtime_configuration(pool: PgPool) {
    let Ok(out) = std::env::var(OUT_VAR) else { return };
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    std::fs::write(out, format!("{}\n{}", current_database(&pool).await, tables.join(",")))
        .unwrap();
}

struct Seen {
    database: String,
    tables: String,
}

fn child(scratch: &Scratch, config: Option<&str>, url: Option<&str>) -> Output {
    let out = scratch.path().join(format!("out-{}", token()));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", CHILD_TEST, "--ignored", "--nocapture"]).env(OUT_VAR, &out);
    match config {
        Some(path) => command.env(CONFIG_VAR, path),
        None => command.env_remove(CONFIG_VAR),
    };
    match url {
        Some(url) => command.env(URL_VAR, url),
        None => command.env_remove(URL_VAR),
    };
    command.output().unwrap()
}

fn passing_child(scratch: &Scratch, config: &str) -> Seen {
    let out = scratch.path().join(format!("out-{}", token()));
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env(OUT_VAR, &out)
        .env(CONFIG_VAR, config)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = std::fs::read_to_string(&out).unwrap();
    let (database, tables) = text.split_once('\n').unwrap();
    Seen { database: database.to_owned(), tables: tables.to_owned() }
}

fn stderr(output: &Output) -> String {
    assert!(!output.status.success());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn server_url() -> String {
    std::env::var(URL_VAR).unwrap()
}

#[test]
fn the_same_binary_picks_up_config_schema_and_migration_changes_without_a_rebuild() {
    let scratch = Scratch::new();
    let (first, second) = (format!("it{}", token()), format!("it{}", token()));
    let config = scratch.write(
        "sqlx-testdb.toml",
        &format!(
            "prefix = \"{first}\"\nbookkeeping_schema = \"{BOOKKEEPING}\"\n\
             [schema]\nsql = [\"schema.sql\"]\n"
        ),
    );
    scratch.write("schema.sql", "CREATE TABLE before_edit ();");
    let seen = passing_child(&scratch, config.as_str());
    assert!(seen.database.starts_with(&format!("{first}_")), "{}", seen.database);
    assert_eq!(seen.tables, "before_edit");

    scratch.write("schema.sql", "CREATE TABLE after_edit ();");
    assert_eq!(passing_child(&scratch, config.as_str()).tables, "after_edit");

    scratch.write(
        "sqlx-testdb.toml",
        &format!(
            "prefix = \"{second}\"\nbookkeeping_schema = \"{BOOKKEEPING}\"\n\
             [schema]\nmigrations = \"migrations\"\n"
        ),
    );
    scratch.write("migrations/20260101000000_one.sql", "CREATE TABLE one ();");
    let seen = passing_child(&scratch, config.as_str());
    assert!(seen.database.starts_with(&format!("{second}_")), "{}", seen.database);
    assert_eq!(seen.tables, "_sqlx_migrations,one");

    scratch.write("migrations/20260102000000_two.sql", "CREATE TABLE two ();");
    assert_eq!(passing_child(&scratch, config.as_str()).tables, "_sqlx_migrations,one,two");

    block_on(async {
        let mut conn = admin().await;
        assert_eq!(with_prefix(&mut conn, &format!("{first}_template_")).await.len(), 2);
        assert_eq!(with_prefix(&mut conn, &format!("{second}_template_")).await.len(), 2);
        drop_all(&mut conn, &first).await;
        drop_all(&mut conn, &second).await;
    });
}

fn url_config(scratch: &Scratch, prefix: &str, database_url: Option<&str>) -> String {
    let url_line =
        database_url.map_or_else(String::new, |url| format!("database_url = \"{url}\"\n"));
    scratch
        .write(
            "sqlx-testdb.toml",
            &format!("prefix = \"{prefix}\"\nbookkeeping_schema = \"{BOOKKEEPING}\"\n{url_line}"),
        )
        .into_string()
}

#[test]
fn the_database_url_comes_from_the_environment_first_then_the_file() {
    let scratch = Scratch::new();
    let prefix = format!("it{}", token());
    let config = url_config(&scratch, &prefix, Some(&server_url()));
    assert!(child(&scratch, Some(&config), None).status.success(), "the file alone suffices");
    let env_wins = stderr(&child(&scratch, Some(&config), Some(UNREACHABLE_URL)));
    assert!(
        env_wins.contains(
            "cannot reach the test database server at \
             postgres://nobody:redacted@127.0.0.1:1/nowhere"
        ),
        "{env_wins}"
    );

    let config = url_config(&scratch, &prefix, None);
    assert!(child(&scratch, Some(&config), Some(&server_url())).status.success());
    let neither = stderr(&child(&scratch, Some(&config), None));
    let expected = format!(
        "no database server: DATABASE_URL is not set and {config} sets no `database_url`; set one \
         of them to the server the tests may use"
    );
    assert!(neither.contains(&expected), "{neither}");
    block_on(async { drop_all(&mut admin().await, &prefix).await });
}

#[test]
fn a_broken_configuration_fails_the_test_naming_the_file_and_the_problem() {
    let scratch = Scratch::new();
    let url = server_url();
    let cases = [
        ("prefix = ", "parsing <path>"),
        ("colour = \"blue\"", "unknown field `colour`"),
        ("[schema]\nsql = [\"missing.sql\"]", "reading schema file <dir>/missing.sql"),
        (
            "[schema]\nsql = [\"a.sql\"]\nmigrations = \"m\"",
            "<path>: set `schema.sql` or `schema.migrations`, not both",
        ),
    ];
    for (toml, expected) in cases {
        let path = scratch.write(&format!("{}.toml", token()), toml);
        let expected =
            expected.replace("<path>", path.as_str()).replace("<dir>", scratch.path().as_str());
        let failure = stderr(&child(&scratch, Some(path.as_str()), Some(&url)));
        assert!(failure.contains(&expected), "{toml:?}: expected {expected:?} in\n{failure}");
    }
    let missing = scratch.path().join("absent.toml");
    let failure = stderr(&child(&scratch, Some(missing.as_str()), Some(&url)));
    assert!(failure.contains(&format!("reading {missing}")), "{failure}");
}
