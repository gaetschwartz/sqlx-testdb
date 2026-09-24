use snafu::ResultExt;

use crate::backend::{Backend, DropMode};
use crate::error::{AlterTemplateSnafu, BookkeepingSnafu, Error, LockSnafu};
use crate::harness::Ctx;
use crate::template::{self, Dropped};

pub async fn sweep<DB: Backend>(conn: &mut DB::Connection, ctx: &Ctx) -> Result<(), Error> {
    let key = ctx.sweep_key;
    if !DB::try_lock(conn, key).await.context(LockSnafu { key })? {
        return Ok(());
    }
    let swept = sweep_databases::<DB>(conn, ctx).await;
    DB::unlock(conn, key).await.context(LockSnafu { key })?;
    swept?;
    let key = ctx.template_key;
    if !DB::try_lock(conn, key).await.context(LockSnafu { key })? {
        return Ok(());
    }
    let swept = sweep_templates::<DB>(conn, ctx).await;
    DB::unlock(conn, key).await.context(LockSnafu { key })?;
    swept
}

async fn sweep_databases<DB: Backend>(conn: &mut DB::Connection, ctx: &Ctx) -> Result<(), Error> {
    let schema = ctx.names.bookkeeping_schema;
    let ages = ctx.config.sweep;
    let leftovers =
        DB::leftover_databases(conn, schema, ages.stale_run_after, ages.kept_failed_for)
            .await
            .context(BookkeepingSnafu)?;
    for name in leftovers {
        match template::drop::<DB>(conn, &name, DropMode::IfIdle).await? {
            Dropped::Yes => {
                DB::forget_database(conn, schema, &name).await.context(BookkeepingSnafu)?;
            }
            Dropped::InUse => {}
        }
    }
    DB::forget_stale_runs(conn, schema, ages.stale_run_after).await.context(BookkeepingSnafu)
}

async fn sweep_templates<DB: Backend>(conn: &mut DB::Connection, ctx: &Ctx) -> Result<(), Error> {
    template::drop_orphaned_builds::<DB>(conn, ctx).await?;
    let schema = ctx.names.bookkeeping_schema;
    let idle = DB::idle_templates(
        conn,
        schema,
        &ctx.names.template_prefix(),
        &ctx.template,
        ctx.config.sweep.idle_template_after,
    )
    .await
    .context(BookkeepingSnafu)?;
    for name in idle {
        DB::retire_template(conn, &name).await.context(AlterTemplateSnafu { name: &name })?;
        match template::drop::<DB>(conn, &name, DropMode::IfIdle).await? {
            Dropped::Yes => {
                DB::forget_template(conn, schema, &name).await.context(BookkeepingSnafu)?;
            }
            Dropped::InUse => {}
        }
    }
    Ok(())
}
