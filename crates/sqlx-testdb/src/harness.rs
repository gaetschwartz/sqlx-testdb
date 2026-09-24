use std::borrow::Cow;
use std::panic::AssertUnwindSafe;

use camino::{Utf8Path, Utf8PathBuf};
use futures_util::FutureExt;
use snafu::{Report, ResultExt};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolOptions;
use sqlx::{Connection, Pool};

use crate::args::{TestArgs, TestOutcome};
use crate::backend::{Backend, ConnectOptionsOf, DropMode, LockKey, LockPurpose};
use crate::config::{Config, PoolSettings, Schema};
use crate::error::{
    ApplyFixtureSnafu, BookkeepingSnafu, ConnectOptionsSnafu, ConnectSnafu, Error,
    ReadFixtureSnafu, RuntimeSnafu,
};
use crate::name::{self, Names};
use crate::schema::LoadedSchema;
use crate::{probe, sweep, template};

const MASTER_POOL_MAX_CONNECTIONS: u32 = 1;
const MANIFEST_DIR_VAR: &str = "CARGO_MANIFEST_DIR";

pub struct Ctx<'a> {
    pub config: &'a Config,
    pub names: Names<'a>,
    pub template: String,
    pub template_key: LockKey,
    pub sweep_key: LockKey,
}

#[derive(Debug, Clone, Copy)]
pub enum SchemaOverride {
    None,
    Migrator(&'static Migrator),
    Migrations(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct TestSpec {
    pub manifest_dir: &'static str,
    pub source_dir: &'static str,
    pub config: Option<fn() -> Config>,
    pub schema: Option<SchemaOverride>,
    pub fixtures: &'static [&'static str],
}

pub fn run<DB, A, F, Fut>(spec: &TestSpec, test_path: &str, test: F)
where
    DB: Backend,
    A: TestArgs<DB>,
    F: FnOnce(A) -> Fut,
    Fut: Future,
    Fut::Output: TestOutcome,
{
    let manifest_dir =
        std::env::var(MANIFEST_DIR_VAR).map_or(Cow::Borrowed(spec.manifest_dir), Cow::Owned);
    let manifest_dir = Utf8Path::new(manifest_dir.as_ref());
    let mut config = spec.config.map_or_else(
        || match Config::discover(manifest_dir) {
            Ok(config) => Cow::Borrowed(config),
            Err(e) => panic!("{test_path}: {}", Report::from_error(e)),
        },
        |make| Cow::Owned(make()),
    );
    if let Some(schema) = spec.schema {
        config.to_mut().schema = match schema {
            SchemaOverride::None => Schema::None,
            SchemaOverride::Migrator(migrator) => Schema::Migrator(migrator),
            SchemaOverride::Migrations(dir) => Schema::Migrations(manifest_dir.join(dir)),
        };
    }
    let source_dir = manifest_dir.join(spec.source_dir);
    let fixtures: Vec<Utf8PathBuf> = spec.fixtures.iter().map(|f| source_dir.join(f)).collect();
    run_with(&config, test_path, &fixtures, test);
}

pub fn run_with<DB, A, F, Fut>(config: &Config, test_path: &str, fixtures: &[Utf8PathBuf], test: F)
where
    DB: Backend,
    A: TestArgs<DB>,
    F: FnOnce(A) -> Fut,
    Fut: Future,
    Fut::Output: TestOutcome,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context(RuntimeSnafu)
        .unwrap_or_else(|e| panic!("{test_path}: {}", Report::from_error(e)));
    runtime.block_on(async move {
        let db = TestDb::<DB>::create(config, test_path, fixtures)
            .await
            .unwrap_or_else(|e| panic!("{test_path}: {}", Report::from_error(e)));
        let args = match A::make(&db).await {
            Ok(args) => args,
            Err(e) => {
                db.finish(Verdict::Discard).await;
                panic!("{test_path}: {}", Report::from_error(e));
            }
        };
        match AssertUnwindSafe(async move { test(args).await.into_result() }).catch_unwind().await {
            Ok(Ok(())) => {
                if let Err(e) = db.drop_database().await {
                    panic!("{test_path}: {}", Report::from_error(e));
                }
            }
            Ok(Err(failure)) => {
                db.finish(Verdict::Failed).await;
                panic!("{test_path}: the test returned {failure:?}");
            }
            Err(panic) => {
                db.finish(Verdict::Failed).await;
                std::panic::resume_unwind(panic);
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Discard,
    Failed,
}

pub struct TestDb<DB: Backend> {
    name: String,
    test_path: String,
    bookkeeping_schema: String,
    keep_failed: bool,
    pool_settings: PoolSettings,
    master: Pool<DB>,
    options: ConnectOptionsOf<DB>,
    pool: Pool<DB>,
}

impl<DB: Backend> TestDb<DB> {
    pub const fn pool(&self) -> &Pool<DB> {
        &self.pool
    }

    pub fn pool_options(&self) -> PoolOptions<DB> {
        pool_options(self.pool_settings)
    }

    pub const fn connect_options(&self) -> &ConnectOptionsOf<DB> {
        &self.options
    }

    pub async fn connect(&self) -> Result<DB::Connection, Error> {
        DB::Connection::connect_with(&self.options).await.context(ConnectSnafu)
    }

    async fn create(
        config: &Config,
        test_path: &str,
        fixtures: &[Utf8PathBuf],
    ) -> Result<Self, Error> {
        let names =
            Names { prefix: &config.prefix, bookkeeping_schema: &config.bookkeeping_schema }
                .validate(DB::MAX_IDENTIFIER_BYTES)?;
        let url = config.database_url()?;
        let server = DB::parse_options(&url).context(ConnectOptionsSnafu)?;
        probe::ensure_reachable::<DB>(&url, &server, config.pool.connect_timeout).await?;
        let schema = LoadedSchema::load(&config.schema).await?;
        let mut fixture_sql = Vec::with_capacity(fixtures.len());
        for path in fixtures {
            fixture_sql.push(std::fs::read_to_string(path).context(ReadFixtureSnafu { path })?);
        }
        let ctx = Ctx {
            config,
            names,
            template: names.template(&schema.digest()),
            template_key: LockKey::new(
                LockPurpose::Template,
                &[names.bookkeeping_schema, names.prefix],
            ),
            sweep_key: LockKey::new(LockPurpose::Sweep, &[names.bookkeeping_schema]),
        };
        let master = PoolOptions::<DB>::new()
            .max_connections(MASTER_POOL_MAX_CONNECTIONS)
            .acquire_timeout(config.pool.acquire_timeout)
            .after_release(|_, _| Box::pin(async { Ok(false) }))
            .connect_lazy_with(server.clone());
        let mut conn = master.acquire().await.context(ConnectSnafu)?;
        let bootstrap_key = LockKey::new(LockPurpose::Bootstrap, &[names.bookkeeping_schema]);
        DB::bootstrap(&mut conn, names.bookkeeping_schema, bootstrap_key)
            .await
            .context(BookkeepingSnafu)?;
        let run = name::current_run_key(config.project_root.as_str());
        let first_of_run = DB::register_run(
            &mut conn,
            names.bookkeeping_schema,
            &run,
            config.project_root.as_str(),
        )
        .await
        .context(BookkeepingSnafu)?;
        if first_of_run {
            sweep::sweep::<DB>(&mut conn, &ctx).await?;
        }
        template::ensure::<DB>(&mut conn, &ctx, &schema, &server).await?;
        let name = names.database(test_path, &run);
        DB::register_database(&mut conn, names.bookkeeping_schema, &name, test_path, &run)
            .await
            .context(BookkeepingSnafu)?;
        if let Err(e) = template::clone_into::<DB>(&mut conn, &ctx, &schema, &server, &name).await {
            DB::forget_database(&mut conn, names.bookkeeping_schema, &name)
                .await
                .context(BookkeepingSnafu)?;
            return Err(e);
        }
        drop(conn);
        let options = DB::options_for_database(&server, &name);
        let pool = pool_options(config.pool).connect_lazy_with(options.clone());
        let db = Self {
            name,
            test_path: test_path.to_owned(),
            bookkeeping_schema: config.bookkeeping_schema.clone(),
            keep_failed: config.keep_failed,
            pool_settings: config.pool,
            master,
            options,
            pool,
        };
        if let Err(e) = db.apply_fixtures(fixtures, &fixture_sql).await {
            db.finish(Verdict::Discard).await;
            return Err(e);
        }
        Ok(db)
    }

    async fn apply_fixtures(&self, paths: &[Utf8PathBuf], sql: &[String]) -> Result<(), Error> {
        if sql.is_empty() {
            return Ok(());
        }
        let mut conn = self.connect().await?;
        for (path, sql) in paths.iter().zip(sql) {
            let applied = DB::apply_sql(&mut conn, sql).await;
            applied.context(ApplyFixtureSnafu { path, name: &self.name })?;
        }
        conn.close().await.context(ConnectSnafu)
    }

    async fn drop_database(&self) -> Result<(), Error> {
        self.pool.close().await;
        let mut conn = self.master.acquire().await.context(ConnectSnafu)?;
        template::drop::<DB>(&mut conn, &self.name, DropMode::Force).await?;
        DB::forget_database(&mut conn, &self.bookkeeping_schema, &self.name)
            .await
            .context(BookkeepingSnafu)
    }

    async fn finish(&self, verdict: Verdict) {
        let (test_path, name) = (self.test_path.as_str(), self.name.as_str());
        if verdict == Verdict::Discard || !self.keep_failed {
            if let Err(e) = self.drop_database().await {
                eprintln!("{test_path}: could not drop {name}: {}", Report::from_error(e));
            }
            return;
        }
        self.pool.close().await;
        let marked = match self.master.acquire().await {
            Ok(mut conn) => DB::mark_failed(&mut conn, &self.bookkeeping_schema, name).await,
            Err(e) => Err(e),
        };
        match marked {
            Ok(()) => eprintln!("{test_path}: kept its test database {name}"),
            Err(e) => eprintln!(
                "{test_path}: kept its test database {name} but could not mark it failed: {e}"
            ),
        }
    }
}

pub fn url_of<DB: sqlx::Database>(pool: &Pool<DB>) -> String {
    sqlx::ConnectOptions::to_url_lossy(&*pool.connect_options()).into()
}

fn pool_options<DB: Backend>(settings: PoolSettings) -> PoolOptions<DB> {
    PoolOptions::new()
        .max_connections(settings.max_connections)
        .idle_timeout(settings.idle_timeout)
        .acquire_timeout(settings.acquire_timeout)
}
