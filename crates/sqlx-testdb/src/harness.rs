use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use snafu::{Report, ResultExt};
use sqlx::pool::PoolOptions;
use sqlx::{Connection, Pool};

use crate::args::{TestArgs, TestOutcome};
use crate::backend::{Backend, ConnectOptionsOf, DropMode, LockKey, LockPurpose};
use crate::config::{Config, Fixture};
use crate::error::{
    ApplyFixtureSnafu, BookkeepingSnafu, ConnectOptionsSnafu, ConnectSnafu, DatabaseUrlSnafu,
    Error, MissingDatabaseUrlSnafu, RuntimeSnafu,
};
use crate::name::{self, Names};
use crate::schema::LoadedSchema;
use crate::{sweep, template};

const MASTER_POOL_MAX_CONNECTIONS: u32 = 1;

pub struct Ctx {
    pub config: &'static Config,
    pub names: Names,
    pub template: String,
    pub template_key: LockKey,
    pub sweep_key: LockKey,
}

pub fn run<DB, A, F, Fut>(
    config: &'static Config,
    test_path: &'static str,
    fixtures: &'static [Fixture],
    test: F,
) where
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
    test_path: &'static str,
    config: &'static Config,
    master: Pool<DB>,
    options: ConnectOptionsOf<DB>,
    pool: Pool<DB>,
}

impl<DB: Backend> TestDb<DB> {
    pub const fn pool(&self) -> &Pool<DB> {
        &self.pool
    }

    pub fn pool_options(&self) -> PoolOptions<DB> {
        pool_options(self.config)
    }

    pub const fn connect_options(&self) -> &ConnectOptionsOf<DB> {
        &self.options
    }

    pub async fn connect(&self) -> Result<DB::Connection, Error> {
        DB::Connection::connect_with(&self.options).await.context(ConnectSnafu)
    }

    async fn create(
        config: &'static Config,
        test_path: &'static str,
        fixtures: &'static [Fixture],
    ) -> Result<Self, Error> {
        let names = Names { prefix: config.prefix, bookkeeping_schema: config.bookkeeping_schema }
            .validate(DB::MAX_IDENTIFIER_BYTES)?;
        let var = config.database_url_var;
        let url = match std::env::var(var) {
            Ok(url) => url,
            Err(std::env::VarError::NotPresent) => return MissingDatabaseUrlSnafu { var }.fail(),
            Err(source) => return Err(source).context(DatabaseUrlSnafu { var }),
        };
        let server = DB::parse_options(&url).context(ConnectOptionsSnafu { var })?;
        let schema = LoadedSchema::load(config.schema).await?;
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
        let run = name::current_run_key(config.project_root);
        let first_of_run =
            DB::register_run(&mut conn, names.bookkeeping_schema, &run, config.project_root)
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
        let pool = pool_options(config).connect_lazy_with(options.clone());
        let db = Self { name, test_path, config, master, options, pool };
        if let Err(e) = db.apply_fixtures(fixtures).await {
            db.finish(Verdict::Discard).await;
            return Err(e);
        }
        Ok(db)
    }

    async fn apply_fixtures(&self, fixtures: &'static [Fixture]) -> Result<(), Error> {
        if fixtures.is_empty() {
            return Ok(());
        }
        let mut conn = self.connect().await?;
        for fixture in fixtures {
            let applied = DB::apply_sql(&mut conn, fixture.sql).await;
            applied.context(ApplyFixtureSnafu { path: fixture.path, name: &self.name })?;
        }
        conn.close().await.context(ConnectSnafu)
    }

    async fn drop_database(&self) -> Result<(), Error> {
        self.pool.close().await;
        let mut conn = self.master.acquire().await.context(ConnectSnafu)?;
        template::drop::<DB>(&mut conn, &self.name, DropMode::Force).await?;
        DB::forget_database(&mut conn, self.config.bookkeeping_schema, &self.name)
            .await
            .context(BookkeepingSnafu)
    }

    async fn finish(&self, verdict: Verdict) {
        let (test_path, name) = (self.test_path, self.name.as_str());
        if verdict == Verdict::Discard || !self.config.keep_failed {
            if let Err(e) = self.drop_database().await {
                eprintln!("{test_path}: could not drop {name}: {}", Report::from_error(e));
            }
            return;
        }
        self.pool.close().await;
        let marked = match self.master.acquire().await {
            Ok(mut conn) => DB::mark_failed(&mut conn, self.config.bookkeeping_schema, name).await,
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

fn pool_options<DB: Backend>(config: &Config) -> PoolOptions<DB> {
    let settings = config.pool;
    PoolOptions::new()
        .max_connections(settings.max_connections)
        .idle_timeout(settings.idle_timeout)
        .acquire_timeout(settings.acquire_timeout)
}
