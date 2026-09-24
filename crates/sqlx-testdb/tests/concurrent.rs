mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use sqlx::PgPool;
use sqlx_testdb::run_with;

use common::{
    BOOKKEEPING, Scratch, admin, block_on, config, current_database, drop_all, exists, token,
    with_prefix,
};

const CHILD_TEST: &str = "one_process_of_the_concurrent_pair";
const PREFIX_VAR: &str = "SQLX_TESTDB_CHILD_PREFIX";
const SCHEMA_VAR: &str = "SQLX_TESTDB_CHILD_SCHEMA";
const OUT_VAR: &str = "SQLX_TESTDB_CHILD_OUT";
const RUN_ID_VAR: &str = "NEXTEST_RUN_ID";
const PROCESSES: usize = 2;
const HOLD_MILLIS: u64 = 500;

#[test]
#[ignore = "spawned by two_processes_running_the_same_test_do_not_collide"]
fn one_process_of_the_concurrent_pair() {
    let (Ok(prefix), Ok(schema), Ok(out)) =
        (std::env::var(PREFIX_VAR), std::env::var(SCHEMA_VAR), std::env::var(OUT_VAR))
    else {
        return;
    };
    let scratch = Scratch::new();
    let config = config(&scratch, &prefix, BOOKKEEPING, "concurrent", &schema);
    run_with(&config, "concurrent::same_test", &[], move |pool: PgPool| async move {
        let name = current_database(&pool).await;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM shared").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
        sqlx::query("INSERT INTO shared VALUES (1)").execute(&pool).await.unwrap();
        tokio::time::sleep(Duration::from_millis(HOLD_MILLIS)).await;
        std::fs::write(out, name).unwrap();
    });
}

#[test]
fn two_processes_running_the_same_test_do_not_collide() {
    let prefix = format!("it{}", token());
    let schema = format!("CREATE TABLE shared (id INT PRIMARY KEY); -- {}", token());
    let run_id = token();
    let dir = std::env::temp_dir();
    let outs: Vec<_> =
        (0..PROCESSES).map(|i| dir.join(format!("sqlx-testdb-{prefix}-{i}"))).collect();
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = outs
        .iter()
        .map(|out| {
            Command::new(&exe)
                .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
                .env(PREFIX_VAR, &prefix)
                .env(SCHEMA_VAR, &schema)
                .env(OUT_VAR, out)
                .env(RUN_ID_VAR, &run_id)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    let names: Vec<String> = outs.iter().map(|out| std::fs::read_to_string(out).unwrap()).collect();
    for out in &outs {
        std::fs::remove_file(out).unwrap();
    }
    assert_ne!(names[0], names[1]);
    let shared = |name: &str| name.rsplit_once('_').map(|(head, _)| head.to_owned());
    assert_eq!(
        shared(&names[0]),
        shared(&names[1]),
        "same test, same run: only the random part differs"
    );
    block_on(async {
        let mut conn = admin().await;
        for name in &names {
            assert!(!exists(&mut conn, name).await, "{name}");
        }
        let templates = with_prefix(&mut conn, &format!("{prefix}_template_")).await;
        assert_eq!(templates.len(), 1, "{templates:?}");
        assert!(with_prefix(&mut conn, &format!("{prefix}_building_")).await.is_empty());
        drop_all(&mut conn, &prefix).await;
    });
}
